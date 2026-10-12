//! 执行层：把「文件 + 设置」翻译成真实的 ffmpeg 调用，并把它跑完。
//!
//! 刻意分成两半：
//! - `plan_split` / `plan_convert` 是纯函数，产出 `FilePlan`，能直接单测；
//! - [`run`] 是阻塞 I/O，只能放进 `cx.background_spawn`，**绝不能在 GPUI 主线程上跑**。
//!
//! 这一层不认识 gpui_kit：它只返回数据，界面怎么显示是 view 的事。

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::jobs::{ConvertSettings, SplitSettings};
use crate::media::{
    convert, no_console_window, parse_progress_line, percent_of, probe, remux, split_range,
    split_segments, AudioFormat, ConvertRequest, Encoding, FfmpegEnv, MediaInfo, ProgressField,
    Quality, RangeSplitRequest, SegmentSplitRequest, SourceCodec,
};
use crate::split::{
    plan_segments, render_name, render_pattern, segment_seconds, SplitMode, MIN_SEGMENT_SECONDS,
};

/// 一次 ffmpeg 调用。
#[derive(Debug)]
enum Step {
    /// 一趟 segment muxer。产出的文件名由 ffmpeg 自己编号，事先不知道，跑完扫目录。
    Segment {
        args: Vec<String>,
        out_dir: PathBuf,
        /// 产物名的形状是 `<head><数字><tail>.<extension>`。
        head: String,
        tail: String,
        extension: String,
        seconds: f64,
    },
    /// 一次单文件调用，产出就是 `output` 这一个路径。
    Single {
        args: Vec<String>,
        output: PathBuf,
        seconds: f64,
    },
}

impl Step {
    fn args(&self) -> &[String] {
        match self {
            Self::Segment { args, .. } | Self::Single { args, .. } => args,
        }
    }

    /// 这一步覆盖源文件的多长，用来把多步的进度拼成一条 0–100。
    fn seconds(&self) -> f64 {
        match self {
            Self::Segment { seconds, .. } | Self::Single { seconds, .. } => *seconds,
        }
    }
}

/// 一个文件要跑的全部步骤。`Send + 'static`，可以直接扔进后台线程。
#[derive(Debug)]
pub struct FilePlan {
    steps: Vec<Step>,
    total_seconds: f64,
}

impl FilePlan {
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }
}

#[derive(Debug)]
pub enum RunError {
    /// 认不出的容器，比如 `.txt` 或者没有扩展名。
    Unsupported(String),
    /// 模板里没有 `{index}`，多段会写到同一个路径上互相覆盖。
    TemplateNeedsIndex,
    /// 输出路径和输入路径是同一个文件。
    SameInputOutput,
    /// 切割数学算不出来（`split::PlanError` 的中文原文）。
    Plan(String),
    Failed(String),
    Cancelled,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(path) => write!(f, "认不出这个文件的音频格式：{path}"),
            Self::TemplateNeedsIndex => {
                f.write_str("文件名模板里要有 {index}，否则每一段都会覆盖上一段")
            }
            Self::SameInputOutput => {
                f.write_str("输出路径和源文件是同一个，请改输出目录或目标格式")
            }
            Self::Plan(message) => f.write_str(message),
            Self::Failed(message) => f.write_str(message),
            Self::Cancelled => f.write_str("已取消"),
        }
    }
}

impl std::error::Error for RunError {}

#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct FileOutcome {
    /// 真正写出来的文件数。
    pub outputs: usize,
    /// 因为「同名文件 = 跳过」而没动的文件数。
    pub skipped: usize,
    /// 被清掉的渣段数（见 [`prune_segments`]）。
    pub pruned: usize,
}

/// 取消开关。`clone()` 出来一份交给后台线程，界面上留着另一份。
#[derive(Clone)]
pub struct Cancellation {
    flag: Arc<AtomicBool>,
    /// 正在跑的子进程。放在这里是为了能**从主线程直接 kill**：
    /// 只靠标志位的话，得等 ffmpeg 吐出下一条进度才反应得过来。
    /// 用 HashMap 存所有正在跑的进程（并发 worker 可能同时有多个）。
    child: Arc<Mutex<HashMap<u64, Child>>>,
}

impl Default for Cancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl Cancellation {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        // 杀掉所有注册了的子进程，但不移除（让 run_step 自己 take 回去处理）
        let mut children = self.lock_child();
        for (_, child) in children.iter_mut() {
            let _ = child.kill();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// 锁被毒（某个线程 panic 在临界区里）也要能继续取消，否则关窗时会留下孤儿 ffmpeg。
    fn lock_child(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Child>> {
        self.child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Drop for Cancellation {
    /// 最后一个持有者消失（通常是关窗）时兜底 kill + wait，不留孤儿进程。
    fn drop(&mut self) {
        if Arc::strong_count(&self.flag) == 1 {
            self.flag.store(true, Ordering::SeqCst);
            let mut children = self.lock_child();
            for (_, mut child) in children.drain() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn source_of(info: &MediaInfo) -> (SourceCodec, Option<u64>) {
    (
        SourceCodec::from_name(info.codec_name.as_deref().unwrap_or("")),
        info.bitrate_bps,
    )
}

fn output_dir(input: &Path, configured: Option<&Path>) -> PathBuf {
    match configured {
        Some(dir) => dir.to_path_buf(),
        None => input
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
    }
}

fn stem_of(input: &Path) -> Result<String, RunError> {
    input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
        .ok_or_else(|| RunError::Unsupported(input.display().to_string()))
}

/// 用户勾了「无损直拷」，但这个容器直拷切出来是坏的，只能退回重编码。
///
/// 目前只有 FLAC 会走到这里（见 `AudioFormat::supports_stream_copy`）。FLAC 是无损编码，
/// 重编码回 FLAC 不掉音质，所以这是"慢一点但结果对"，不是降级——但**界面必须说出来**，
/// 否则用户会以为我们无视了他的选择。
pub fn effective_encoding(format: AudioFormat, requested: Encoding) -> Encoding {
    match requested {
        Encoding::Copy if !format.supports_stream_copy() => Encoding::Reencode(Quality::High),
        other => other,
    }
}

/// 规划一个文件的切割。
///
/// 走 segment muxer 还是逐段调用，这个选择是有实测依据的（见 `build.md` 的已知行为表）：
/// - **ByCount 一律逐段**：segment muxer 在帧边界切，可能给出 N±1 个文件，而按份数
///   的语义就是"恰好 N 个"。
/// - **FLAC 一律逐段**：segment muxer 对 FLAC 整个是废的，连重编码都救不回来
///   （`AudioFormat::supports_segment_muxer`）。
/// - 其余情况一趟 segment muxer，切点连续、进程也少。
pub fn plan_split(
    input: &Path,
    info: &MediaInfo,
    settings: &SplitSettings,
) -> Result<FilePlan, RunError> {
    let format = AudioFormat::from_path(input)
        .ok_or_else(|| RunError::Unsupported(input.display().to_string()))?;
    let (source, source_bitrate) = source_of(info);
    let encoding = effective_encoding(format, settings.encoding());
    let dir = output_dir(input, settings.output_dir.as_deref());
    let stem = stem_of(input)?;
    let template = settings.name_template.as_str();

    let use_segments =
        !matches!(settings.mode, SplitMode::ByCount(_)) && format.supports_segment_muxer();

    if use_segments {
        let seconds = segment_seconds(info.duration_s, info.bitrate_bps, settings.mode)
            .map_err(|err| RunError::Plan(err.to_string()))?;
        let pattern = render_pattern(template, &stem).ok_or(RunError::TemplateNeedsIndex)?;
        let (head, tail) = pattern
            .split_once("%d")
            .ok_or(RunError::TemplateNeedsIndex)?;
        let args = split_segments(&SegmentSplitRequest {
            input,
            output_pattern: dir
                .join(format!("{pattern}.{}", format.extension()))
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| RunError::Unsupported(dir.display().to_string()))?
                .as_str(),
            format,
            segment_seconds: seconds,
            encoding,
            source,
            source_bitrate,
            keep_tags_and_cover: settings.keep_tags_and_cover,
            overwrite: settings.overwrite,
        });
        return Ok(FilePlan {
            steps: vec![Step::Segment {
                args,
                out_dir: dir,
                head: head.to_string(),
                tail: tail.to_string(),
                extension: format.extension().to_string(),
                seconds: info.duration_s,
            }],
            total_seconds: info.duration_s,
        });
    }

    let segments = plan_segments(info.duration_s, info.bitrate_bps, settings.mode)
        .map_err(|err| RunError::Plan(err.to_string()))?;
    let mut steps = Vec::with_capacity(segments.len());
    for (ix, segment) in segments.iter().enumerate() {
        let output = dir.join(format!(
            "{}.{}",
            render_name(template, &stem, ix),
            format.extension()
        ));
        if output == input {
            return Err(RunError::SameInputOutput);
        }
        let args = split_range(&RangeSplitRequest {
            input,
            output: &output,
            format,
            start_seconds: segment.start_s,
            duration_seconds: segment.duration_s,
            encoding,
            source,
            source_bitrate,
            keep_tags_and_cover: settings.keep_tags_and_cover,
            overwrite: settings.overwrite,
        });
        steps.push(Step::Single {
            args,
            output,
            seconds: segment.duration_s,
        });
    }

    Ok(FilePlan {
        steps,
        total_seconds: info.duration_s,
    })
}

/// 规划一个文件的格式转换。源容器和目标容器相同时只换壳，不重编码。
pub fn plan_convert(
    input: &Path,
    info: &MediaInfo,
    settings: &ConvertSettings,
) -> Result<FilePlan, RunError> {
    let (source, source_bitrate) = source_of(info);
    let dir = output_dir(input, settings.output_dir.as_deref());
    let stem = stem_of(input)?;
    let output = dir.join(format!("{stem}.{}", settings.target.extension()));

    // 实测：输出和输入是同一个路径时 ffmpeg 直接拒绝（"Output … same as Input #0"）。
    // 用户把目标格式选成和源一样、又不改输出目录就会撞上，得在这里挡住并说清楚。
    if output == input {
        return Err(RunError::SameInputOutput);
    }

    let args = if AudioFormat::from_path(input) == Some(settings.target) {
        remux(
            input,
            &output,
            settings.overwrite,
            settings.keep_tags_and_cover,
        )
    } else {
        convert(&ConvertRequest {
            input,
            output: &output,
            target: settings.target,
            quality: settings.quality,
            source,
            source_bitrate,
            keep_tags_and_cover: settings.keep_tags_and_cover,
            overwrite: settings.overwrite,
        })
    };

    Ok(FilePlan {
        steps: vec![Step::Single {
            args,
            output,
            seconds: info.duration_s,
        }],
        total_seconds: info.duration_s,
    })
}

/// 跑完一个文件的全部步骤。阻塞调用，只能在后台线程里跑。
pub fn run(
    env: &FfmpegEnv,
    plan: &FilePlan,
    cancel: &Cancellation,
    mut on_progress: impl FnMut(f32),
) -> Result<FileOutcome, RunError> {
    let mut outputs = 0usize;
    let mut skipped = 0usize;
    let mut runts = 0usize;
    let mut completed = 0.0f64;

    for step in &plan.steps {
        if cancel.is_cancelled() {
            return Err(RunError::Cancelled);
        }

        let seconds = step.seconds();
        let base = percent_of(completed, plan.total_seconds);
        let span = percent_of(seconds, plan.total_seconds);

        let outcome = match run_step(env, step, cancel, base, span, &mut on_progress) {
            Ok(outcome) => outcome,
            // 半途取消会在输出目录里留一个只写了一半的文件。它探测得出来、也能播放
            // 前半段，用户不会觉得那是垃圾——所以必须主动删掉，不能留给下一次跳过策略
            // 当成"已存在"。
            Err(err @ RunError::Cancelled) => {
                discard_partial(step);
                return Err(err);
            }
            Err(err) => return Err(err),
        };

        match outcome {
            StepOutcome::Skipped => skipped += 1,
            StepOutcome::Done => match step {
                Step::Single { output, .. } => {
                    if output.is_file() {
                        outputs += 1;
                    }
                }
                Step::Segment {
                    out_dir,
                    head,
                    tail,
                    extension,
                    ..
                } => {
                    let (kept, pruned) = prune_segments(env, out_dir, head, tail, extension);
                    outputs += kept;
                    runts += pruned;
                }
            },
        }

        completed += seconds;
        on_progress(percent_of(completed, plan.total_seconds));
    }

    Ok(FileOutcome {
        outputs,
        skipped,
        pruned: runts,
    })
}

/// 把某一步的产物清掉，取消时用。
///
/// segment muxer 那一步产物名字事先不知道，只能按 `<head><数字><tail>.<ext>` 扫目录，
/// 规则和 [`prune_segments`] 一致——**别的一律不碰**，用户目录里可能有同名前缀的东西。
fn discard_partial(step: &Step) {
    match step {
        Step::Single { output, .. } => {
            let _ = fs::remove_file(output);
        }
        Step::Segment {
            out_dir,
            head,
            tail,
            extension,
            ..
        } => {
            let Ok(entries) = fs::read_dir(out_dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let matches = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| matches_pattern(name, head, tail, extension));
                if matches {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }
}

enum StepOutcome {
    Done,
    Skipped,
}

fn run_step(
    env: &FfmpegEnv,
    step: &Step,
    cancel: &Cancellation,
    base: f32,
    span: f32,
    on_progress: &mut impl FnMut(f32),
) -> Result<StepOutcome, RunError> {
    let mut command = Command::new(env.ffmpeg());
    command
        .args(step.args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_console_window(&mut command);

    let mut child = command
        .spawn()
        .map_err(|err| RunError::Failed(format!("起不了 ffmpeg：{err}")))?;

    // stderr 必须单独一个线程抽干：管道写满（64KB）时 ffmpeg 会阻塞在写日志上，
    // 而这边还堵在读 stdout，两边就互相等死了。
    let stderr = child.stderr.take().expect("上面刚 piped 过");
    let drained = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut text);
        text
    });

    let stdout = child.stdout.take().expect("上面刚 piped 过");
    let child_id = child.id() as u64;
    cancel.lock_child().insert(child_id, child);

    let step_seconds = step.seconds();
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if let Some(ProgressField::OutTime(seconds)) = parse_progress_line(&line) {
            // 这一步在自己的时长里走了多少，映射到它占整体进度的那一段区间上。
            let within = percent_of(seconds, step_seconds) / 100.0;
            let percent = (base + span * within).clamp(0.0, 100.0);
            on_progress(percent);
        }
    }

    let mut child = cancel.lock_child().remove(&(child_id as u64)).expect("自己的进程一定在");
    let status = child
        .wait()
        .map_err(|err| RunError::Failed(format!("等不到 ffmpeg 结束：{err}")))?;
    let stderr_text = drained.join().unwrap_or_default();

    // 取消检查要在成功检查之前：被 kill 的 ffmpeg 退出码一定不是 0，
    // 如果先判 success() 会谎报成 Failed。
    if cancel.is_cancelled() {
        return Err(RunError::Cancelled);
    }

    // 实测 ffmpeg 9.0.2：`-n`（跳过）遇到已存在文件时**退出码是 0**，
    // 只在 stderr 写 "File '…' already exists. Exiting."。
    // 拿 status.success() 判断会把「跳过」当成「成功」，界面上就会谎报产出了文件。
    if stderr_text.contains("already exists") {
        return Ok(StepOutcome::Skipped);
    }

    if !status.success() {
        return Err(RunError::Failed(stderr_tail(&stderr_text)));
    }

    Ok(StepOutcome::Done)
}

/// 清掉 segment muxer 末尾多吐的渣段，返回（保留数，清掉数）。
///
/// 实测 ffmpeg 9.0.2：9 秒的源按 3 秒切，除了 3 个正常段之外还会多出一个几毫秒的尾巴
/// ——m4a 0.014 秒、ogg 0.0008 秒、aac 0.023 秒，**mp3 那个 461 字节连 ffprobe 都读不出来**。
/// 不清掉的话用户就会在输出目录里看到一个坏文件。
///
/// 只动名字严格长成 `<head><数字><tail>.<extension>` 的文件，别的一律不碰。
fn prune_segments(
    env: &FfmpegEnv,
    dir: &Path,
    head: &str,
    tail: &str,
    extension: &str,
) -> (usize, usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return (0, 0);
    };

    let mut kept = 0;
    let mut pruned = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !matches_pattern(name, head, tail, extension) {
            continue;
        }
        // 探测不了（mp3 的渣段就是这种）或者短得没有意义，都算渣。
        let is_runt = probe(env, &path)
            .map(|info| !(info.duration_s >= MIN_SEGMENT_SECONDS))
            .unwrap_or(true);
        if is_runt {
            if fs::remove_file(&path).is_ok() {
                pruned += 1;
            }
        } else {
            kept += 1;
        }
    }

    (kept, pruned)
}

fn matches_pattern(name: &str, head: &str, tail: &str, extension: &str) -> bool {
    let suffix = if tail.is_empty() {
        format!(".{extension}")
    } else {
        format!("{tail}.{extension}")
    };
    let Some(rest) = name.strip_prefix(head) else {
        return false;
    };
    let Some(digits) = rest.strip_suffix(&suffix) else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// 报错只留最后几行：ffmpeg 失败时前面几十行往往是无关的探测噪声。
fn stderr_tail(stderr: &str) -> String {
    let meaningful: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let start = meaningful.len().saturating_sub(4);
    let text = meaningful[start..].join(" / ");
    if text.is_empty() {
        "ffmpeg 失败了，但没有给出原因".to_string()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::Overwrite;

    fn info(duration_s: f64, codec: &str, bitrate_bps: Option<u64>) -> MediaInfo {
        MediaInfo {
            duration_s,
            bitrate_bps,
            size_bytes: None,
            codec_name: Some(codec.to_string()),
            sample_rate: Some(44_100),
            channels: Some(2),
            has_cover_art: false,
        }
    }

    fn split(mode: SplitMode) -> SplitSettings {
        SplitSettings {
            mode,
            ..Default::default()
        }
    }

    /// segment muxer 那一步的输出模板，就是 ffmpeg 参数表的最后一项。
    fn segment_pattern(plan: &FilePlan) -> &str {
        match &plan.steps[0] {
            Step::Segment { args, .. } => args.last().map(String::as_str).unwrap_or(""),
            other => panic!("期望一趟 segment muxer，实际是 {other:?}"),
        }
    }

    fn single_outputs(plan: &FilePlan) -> Vec<PathBuf> {
        plan.steps
            .iter()
            .map(|step| match step {
                Step::Single { output, .. } => output.clone(),
                other => panic!("期望逐段调用，实际是 {other:?}"),
            })
            .collect()
    }

    #[test]
    fn by_count_always_splits_per_range_even_when_copying() {
        // segment muxer 在帧边界切，给不出"恰好 N 段"；按份数的语义就是恰好 N 个。
        let plan = plan_split(
            Path::new("/in/song.m4a"),
            &info(90.0, "aac", Some(128_000)),
            &split(SplitMode::ByCount(3)),
        )
        .unwrap();

        assert_eq!(plan.step_count(), 3);
        let outputs = single_outputs(&plan);
        let names: Vec<String> = outputs
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["song_part1.m4a", "song_part2.m4a", "song_part3.m4a"]
        );
    }

    #[test]
    fn copy_split_of_a_normal_container_uses_one_segment_pass() {
        let plan = plan_split(
            Path::new("/in/song.m4a"),
            &info(90.0, "aac", Some(128_000)),
            &split(SplitMode::ByDuration(30.0)),
        )
        .unwrap();

        assert_eq!(plan.step_count(), 1);
        // 期望值也要用 join 拼，不能写死 "/in/…"：生产代码是 PathBuf::join 出来的，
        // Windows 上是反斜杠，写死正斜杠的字面量在那边会红。
        let expected = Path::new("/in").join("song_part%d.m4a");
        assert_eq!(segment_pattern(&plan), expected.to_str().unwrap());
        match &plan.steps[0] {
            Step::Segment { head, tail, .. } => {
                assert_eq!(head, "song_part");
                assert!(tail.is_empty());
            }
            other => panic!("期望 Segment，实际是 {other:?}"),
        }
    }

    #[test]
    fn flac_never_uses_the_segment_muxer_even_in_exact_mode() {
        // 实测：segment muxer 对 FLAC 整个是废的，重编码也救不回来——
        // `-c:a flac -f segment` 出来的前几段 duration=N/A，最后那个渣段反而报 9 秒（整个源）。
        for exact in [false, true] {
            let settings = SplitSettings {
                exact,
                ..split(SplitMode::ByDuration(30.0))
            };
            let plan = plan_split(
                Path::new("/in/song.flac"),
                &info(90.0, "flac", None),
                &settings,
            )
            .unwrap();

            assert_eq!(plan.step_count(), 3, "exact={exact} 时 FLAC 也必须逐段切");
            let outputs = single_outputs(&plan);
            assert!(outputs[0].to_string_lossy().ends_with("song_part1.flac"));
        }
    }

    #[test]
    fn flac_copy_is_upgraded_to_a_lossless_reencode() {
        // 实测：`-ss 3 -t 3 -i tone.flac -c copy` 出来的段，format 和 stream 两处 duration
        // 都报 9 秒（整个源文件），`-ss` 挪到 `-i` 后面也一样。
        let plan = plan_split(
            Path::new("/in/song.flac"),
            &info(90.0, "flac", None),
            &split(SplitMode::ByCount(3)),
        )
        .unwrap();

        let args = plan.steps[0].args();
        assert!(
            args.contains(&"-c:a".to_string()),
            "FLAC 不能直拷：{args:?}"
        );
        assert!(!args.contains(&"-c".to_string()));
    }

    #[test]
    fn effective_encoding_only_overrides_copy_on_flac() {
        assert_eq!(
            effective_encoding(AudioFormat::Flac, Encoding::Copy),
            Encoding::Reencode(Quality::High)
        );
        assert_eq!(
            effective_encoding(AudioFormat::Flac, Encoding::Reencode(Quality::Low)),
            Encoding::Reencode(Quality::Low),
            "已经要重编码了，别再动用户挑的音质档"
        );
        for format in AudioFormat::ALL {
            if format != AudioFormat::Flac {
                assert_eq!(
                    effective_encoding(format, Encoding::Copy),
                    Encoding::Copy,
                    "{} 该直拷就直拷",
                    format.label()
                );
            }
        }
    }

    #[test]
    fn copy_stays_copy_for_every_container_except_flac() {
        for format in AudioFormat::ALL {
            let path = format!("/in/song.{}", format.extension());
            let plan = plan_split(
                Path::new(&path),
                &info(90.0, "aac", None),
                &split(SplitMode::ByCount(3)),
            )
            .unwrap();

            assert_eq!(
                plan.steps[0].args().contains(&"-c".to_string()),
                format.supports_stream_copy(),
                "{} 的直拷策略和 supports_stream_copy 对不上",
                format.label()
            );
        }
    }

    #[test]
    fn exact_mode_reencodes() {
        let settings = SplitSettings {
            exact: true,
            ..split(SplitMode::ByCount(2))
        };
        let plan = plan_split(
            Path::new("/in/song.m4a"),
            &info(60.0, "aac", Some(128_000)),
            &settings,
        )
        .unwrap();

        assert!(plan.steps[0].args().iter().any(|arg| arg == "-c:a"));
        assert!(!plan.steps[0].args().contains(&"-c".to_string()));
    }

    #[test]
    fn configured_output_dir_wins_over_sitting_next_to_the_source() {
        let settings = SplitSettings {
            output_dir: Some(PathBuf::from("/out")),
            ..split(SplitMode::ByCount(2))
        };
        let plan = plan_split(
            Path::new("/in/song.m4a"),
            &info(60.0, "aac", None),
            &settings,
        )
        .unwrap();

        assert_eq!(single_outputs(&plan)[0], Path::new("/out/song_part1.m4a"));
    }

    #[test]
    fn a_template_without_index_is_rejected_instead_of_overwriting_itself() {
        let settings = SplitSettings {
            name_template: "{name}_cut".to_string(),
            ..split(SplitMode::ByDuration(30.0))
        };
        let err = plan_split(
            Path::new("/in/song.m4a"),
            &info(90.0, "aac", None),
            &settings,
        )
        .unwrap_err();

        assert!(matches!(err, RunError::TemplateNeedsIndex));
        assert!(err.to_string().contains("{index}"));
    }

    #[test]
    fn a_split_onto_its_own_source_is_rejected() {
        // 默认模板撞不上，但模板只写 {index} 时，一个叫 "1.m4a" 的文件的第一段
        // 就正好落回源文件自己。真撞上时 ffmpeg 会直接把源文件截断成前 30 秒。
        let settings = SplitSettings {
            name_template: "{index}".to_string(),
            ..split(SplitMode::ByCount(1))
        };
        let err =
            plan_split(Path::new("/in/1.m4a"), &info(30.0, "aac", None), &settings).unwrap_err();

        assert!(matches!(err, RunError::SameInputOutput));
    }

    #[test]
    fn an_unknown_container_is_rejected_before_any_planning() {
        let err = plan_split(
            Path::new("/in/notes.txt"),
            &info(90.0, "aac", None),
            &split(SplitMode::ByCount(2)),
        )
        .unwrap_err();

        assert!(matches!(err, RunError::Unsupported(_)));
    }

    #[test]
    fn splitting_into_too_many_parts_surfaces_the_planning_error() {
        let err = plan_split(
            Path::new("/in/song.m4a"),
            &info(1.0, "aac", None),
            &split(SplitMode::ByDuration(0.001)),
        )
        .unwrap_err();

        assert!(matches!(err, RunError::Plan(_)), "实际是 {err:?}");
    }

    #[test]
    fn converting_m4a_to_mp3_reencodes_and_keeps_tags() {
        let settings = ConvertSettings::default();
        let plan = plan_convert(
            Path::new("/in/song.m4a"),
            &info(200.0, "aac", Some(256_000)),
            &settings,
        )
        .unwrap();

        assert_eq!(plan.step_count(), 1);
        assert_eq!(single_outputs(&plan)[0], Path::new("/in/song.mp3"));
        let args = plan.steps[0].args();
        assert!(args.contains(&"-c:a".to_string()));
        assert!(args.contains(&"-map_metadata".to_string()));
    }

    #[test]
    fn converting_into_the_same_container_only_remuxes() {
        let settings = ConvertSettings {
            target: AudioFormat::M4a,
            ..Default::default()
        };
        let plan = plan_convert(
            Path::new("/in/song.m4b"),
            &info(200.0, "aac", None),
            &settings,
        )
        .unwrap();

        // .m4b 和 .m4a 都是 AudioFormat::M4a，音频不用重编码，换个壳就行。
        let args = plan.steps[0].args();
        assert!(args.contains(&"-c".to_string()));
        assert!(!args.iter().any(|arg| arg == "-c:a"));
    }

    #[test]
    fn converting_onto_its_own_source_is_rejected() {
        let settings = ConvertSettings {
            target: AudioFormat::Mp3,
            ..Default::default()
        };
        let err = plan_convert(
            Path::new("/in/song.mp3"),
            &info(200.0, "mp3", None),
            &settings,
        )
        .unwrap_err();

        assert!(matches!(err, RunError::SameInputOutput));
        assert!(err.to_string().contains("输出目录"));
    }

    #[test]
    fn quality_reaches_the_command_line() {
        let settings = ConvertSettings {
            quality: Quality::High,
            ..Default::default()
        };
        let plan = plan_convert(
            Path::new("/in/a.wav"),
            &info(10.0, "pcm_s16le", None),
            &settings,
        )
        .unwrap();

        assert!(plan.steps[0].args().contains(&"-q:a".to_string()));
    }

    #[test]
    fn overwrite_skip_becomes_dash_n() {
        let settings = ConvertSettings {
            overwrite: Overwrite::Skip,
            ..Default::default()
        };
        let plan = plan_convert(
            Path::new("/in/a.wav"),
            &info(10.0, "pcm_s16le", None),
            &settings,
        )
        .unwrap();

        assert!(plan.steps[0].args().contains(&"-n".to_string()));
    }

    #[test]
    fn total_seconds_is_the_source_duration_not_the_sum_of_steps() {
        let plan = plan_split(
            Path::new("/in/song.m4a"),
            &info(90.0, "aac", None),
            &split(SplitMode::ByCount(3)),
        )
        .unwrap();

        // 进度条按"已覆盖的源时长 / 源总时长"算，用步骤时长求和会因为最后一段不足而差一截。
        assert_eq!(plan.total_seconds, 90.0);
        assert_eq!(plan.steps.iter().map(Step::seconds).sum::<f64>(), 90.0);
    }

    #[test]
    fn pruning_only_touches_files_shaped_like_segments() {
        let ext = "m4a";
        assert!(matches_pattern("song_part1.m4a", "song_part", "", ext));
        assert!(matches_pattern("song_part42.m4a", "song_part", "", ext));
        // 前缀对不上、后缀对不上、数字位置是空的、扩展名不对，全都不动。
        assert!(!matches_pattern("other_part1.m4a", "song_part", "", ext));
        assert!(!matches_pattern("song_part1.mp3", "song_part", "", ext));
        assert!(!matches_pattern("song_part.m4a", "song_part", "", ext));
        assert!(!matches_pattern("song_partX1.m4a", "song_part", "", ext));
        assert!(!matches_pattern(
            "song_part1.m4a.backup",
            "song_part",
            "",
            ext
        ));

        // 模板带尾巴时（{name}-{index}of3）也要认得出来。
        assert!(matches_pattern("song-2of3.m4a", "song-", "of3", ext));
        assert!(!matches_pattern("song-2of4.m4a", "song-", "of3", ext));
    }

    #[test]
    fn a_failure_message_keeps_only_the_last_lines_of_stderr() {
        let stderr = "\n\nline1\nline2\nline3\nline4\nline5\nline6\n";
        assert_eq!(stderr_tail(stderr), "line3 / line4 / line5 / line6");
        assert_eq!(stderr_tail("  \n"), "ffmpeg 失败了，但没有给出原因");
    }

    #[test]
    fn cancelling_flips_the_flag_and_is_visible_from_another_clone() {
        let cancel = Cancellation::new();
        let worker = cancel.clone();
        assert!(!worker.is_cancelled());
        cancel.cancel();
        assert!(worker.is_cancelled());
        // 反复取消不能炸：第二次时槽里已经是 None。
        cancel.cancel();
        assert!(worker.is_cancelled());
    }

    /// 建一个只有本测试用的目录，闭包跑完就删。
    /// 不复用 `tests/common`：那个只在集成测试目标里可见，单元测试够不着。
    fn with_dir<R>(name: &str, body: impl FnOnce(&Path) -> R) -> R {
        let dir = std::env::temp_dir().join(format!("cut-unit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let result = body(&dir);
        let _ = fs::remove_dir_all(&dir);
        result
    }

    #[test]
    fn discarding_a_single_step_removes_exactly_its_output() {
        with_dir("discard-single", |dir| {
            let output = dir.join("song_part1.m4a");
            let bystander = dir.join("song_part1.m4a.bak");
            fs::write(&output, b"half").unwrap();
            fs::write(&bystander, b"keep").unwrap();

            discard_partial(&Step::Single {
                args: Vec::new(),
                output: output.clone(),
                seconds: 1.0,
            });

            assert!(!output.exists(), "半成品必须删掉");
            assert!(bystander.exists(), "别的一概不动");
        });
    }

    #[test]
    fn discarding_a_segment_step_removes_only_files_shaped_like_segments() {
        with_dir("discard-segment", |dir| {
            for name in [
                "song_part1.m4a",
                "song_part2.m4a",
                "song_partX.m4a",
                "song.mp3",
            ] {
                fs::write(dir.join(name), b"half").unwrap();
            }

            discard_partial(&Step::Segment {
                args: Vec::new(),
                out_dir: dir.to_path_buf(),
                head: "song_part".to_string(),
                tail: String::new(),
                extension: "m4a".to_string(),
                seconds: 1.0,
            });

            let mut left: Vec<String> = fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            left.sort();
            assert_eq!(left, ["song.mp3", "song_partX.m4a"]);
        });
    }
}
