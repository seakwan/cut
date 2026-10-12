//! 执行层的真 ffmpeg 集成测试。
//!
//! `tests/command.rs` 证明参数拼对了，`src/runner.rs` 的单元测试证明规划逻辑对了，
//! 这里证明**整条链路跑完以后磁盘上真的是对的东西**：渣段被清掉、时长加起来对得上、
//! 跳过不会被误报成产出、取消以后既没有半成品也没有孤儿进程。
//!
//! 找不到二进制时整个文件自动跳过（先跑 `script/build-ffmpeg.sh`）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cut::jobs::{ConvertSettings, SplitSettings};
use cut::media::{probe, AudioFormat, FfmpegEnv, Quality};
use cut::runner::{plan_convert, plan_split, run, Cancellation, RunError};
use cut::split::{SplitMode, MIN_SEGMENT_SECONDS};

mod common;

#[cfg(unix)]
use common::live_ffmpeg_for;
use common::{
    env_or_skip, files_with_extension, info, sine, sine_of_seconds, total_duration, TempDir,
};

fn split_settings(mode: SplitMode) -> SplitSettings {
    SplitSettings {
        mode,
        output_dir: None,
        ..Default::default()
    }
}

/// 切完以后目录里该有的东西：探测得出、且不短于 `MIN_SEGMENT_SECONDS`。
fn real_outputs(env: &FfmpegEnv, dir: &Path, extension: &str) -> Vec<PathBuf> {
    files_with_extension(dir, extension)
        .into_iter()
        .filter(|path| probe(env, path).is_ok_and(|info| info.duration_s >= MIN_SEGMENT_SECONDS))
        .collect()
}

fn names(files: &[PathBuf]) -> Vec<String> {
    files
        .iter()
        .map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect()
}

// ---- 切割 · 三种维度 ----

#[test]
fn by_duration_copy_split_covers_the_whole_source() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("run-duration");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let out = dir.mkdir("out");
    let source_info = info(&env, &source);

    let settings = SplitSettings {
        output_dir: Some(out.clone()),
        ..split_settings(SplitMode::ByDuration(3.0))
    };
    let plan = plan_split(&source, &source_info, &settings).unwrap();
    assert_eq!(
        plan.step_count(),
        1,
        "按时长 + 直拷应该是一趟 segment muxer"
    );

    let outcome = run(&env, &plan, &Cancellation::new(), |_| {}).unwrap();

    assert_eq!(outcome.outputs, 3, "9 秒按 3 秒切就是 3 段");
    // segment muxer 一定会多吐一个几毫秒的渣段，执行器必须自己清掉（build.md 已知行为表）。
    assert!(
        outcome.pruned >= 1,
        "渣段应该被清掉，实际 pruned={}",
        outcome.pruned
    );
    assert_eq!(outcome.skipped, 0);

    let kept = real_outputs(&env, &out, "m4a");
    assert_eq!(
        names(&kept),
        ["tone_part1.m4a", "tone_part2.m4a", "tone_part3.m4a"]
    );
    assert_eq!(
        files_with_extension(&out, "m4a").len(),
        3,
        "渣段要真的从磁盘上删掉，不是只在计数里忽略"
    );

    let sum = total_duration(&env, &kept);
    assert!(
        (sum - source_info.duration_s).abs() < 0.5,
        "各段时长加起来（{sum}）应该等于源文件（{}）",
        source_info.duration_s
    );
}

#[test]
fn by_count_copy_split_yields_exactly_n_files() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("run-count");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let out = dir.mkdir("out");
    let source_info = info(&env, &source);

    let settings = SplitSettings {
        output_dir: Some(out.clone()),
        ..split_settings(SplitMode::ByCount(3))
    };
    let plan = plan_split(&source, &source_info, &settings).unwrap();
    assert_eq!(plan.step_count(), 3, "按份数一律逐段调用，才保证恰好 N 个");

    let outcome = run(&env, &plan, &Cancellation::new(), |_| {}).unwrap();
    assert_eq!(outcome.outputs, 3);
    assert_eq!(outcome.pruned, 0, "逐段调用不产渣段");

    let kept = real_outputs(&env, &out, "m4a");
    assert_eq!(
        names(&kept),
        ["tone_part1.m4a", "tone_part2.m4a", "tone_part3.m4a"]
    );
    let sum = total_duration(&env, &kept);
    assert!(
        (sum - source_info.duration_s).abs() < 0.5,
        "各段时长加起来（{sum}）应该等于源文件（{}）",
        source_info.duration_s
    );
}

#[test]
fn by_size_copy_split_covers_the_whole_source() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("run-size");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let out = dir.mkdir("out");
    let source_info = info(&env, &source);

    // 取源体积的三分之一，按大小切就应该得到 3 段。
    let size = source_info.size_bytes.expect("m4a 探得到体积");
    let settings = SplitSettings {
        output_dir: Some(out.clone()),
        ..split_settings(SplitMode::BySize(size / 3))
    };
    let plan = plan_split(&source, &source_info, &settings).unwrap();
    let outcome = run(&env, &plan, &Cancellation::new(), |_| {}).unwrap();

    assert_eq!(outcome.outputs, 3, "按大小切出来的段数不对");
    let kept = real_outputs(&env, &out, "m4a");
    let sum = total_duration(&env, &kept);
    assert!(
        (sum - source_info.duration_s).abs() < 0.5,
        "各段时长加起来（{sum}）应该等于源文件（{}）",
        source_info.duration_s
    );
}

#[test]
fn exact_split_is_sample_accurate() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("run-exact");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let out = dir.mkdir("out");
    let source_info = info(&env, &source);

    let settings = SplitSettings {
        exact: true,
        output_dir: Some(out.clone()),
        ..split_settings(SplitMode::ByCount(3))
    };
    let plan = plan_split(&source, &source_info, &settings).unwrap();
    let outcome = run(&env, &plan, &Cancellation::new(), |_| {}).unwrap();
    assert_eq!(outcome.outputs, 3);

    // 重编码走的是解码后的 -t，切点样本级准确；直拷只能落在帧边界上。
    for path in real_outputs(&env, &out, "m4a") {
        let each = info(&env, &path).duration_s;
        assert!(
            (each - 3.0).abs() < 0.05,
            "{} 应该正好 3 秒，实际 {each}",
            path.display()
        );
    }
}

#[test]
fn every_container_survives_a_copy_split() {
    let Some(env) = env_or_skip() else { return };

    for format in AudioFormat::ALL {
        let dir = TempDir::new(&format!("run-all-{}", format.extension()));
        let source = sine(&env, &dir, &format!("tone.{}", format.extension()), format);
        let out = dir.mkdir("out");
        let source_info = info(&env, &source);

        let settings = SplitSettings {
            output_dir: Some(out.clone()),
            ..split_settings(SplitMode::ByCount(3))
        };
        let plan = plan_split(&source, &source_info, &settings).unwrap();
        let outcome = run(&env, &plan, &Cancellation::new(), |_| {})
            .unwrap_or_else(|err| panic!("{} 切失败了：{err}", format.label()));

        assert_eq!(outcome.outputs, 3, "{} 段数不对", format.label());

        // FLAC 走的是逐段调用，每段自己重写文件头，所以时长必须是对的。
        // 这一条正是 `supports_copy_segmenting` 存在的理由：segment muxer 直拷 FLAC
        // 会把整个源文件的时长写进每一段的 STREAMINFO。
        for path in real_outputs(&env, &out, format.extension()) {
            let each = info(&env, &path);
            assert!(
                each.duration_s < source_info.duration_s,
                "{} 的 {} 报了整个源文件的时长 {}",
                format.label(),
                path.display(),
                each.duration_s
            );
        }
    }
}

// ---- 转换 ----

#[test]
fn convert_run_produces_one_playable_mp3() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("run-convert");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let out = dir.mkdir("out");
    let source_info = info(&env, &source);

    let settings = ConvertSettings {
        target: AudioFormat::Mp3,
        quality: Quality::Medium,
        output_dir: Some(out.clone()),
        ..Default::default()
    };
    let plan = plan_convert(&source, &source_info, &settings).unwrap();
    assert_eq!(plan.step_count(), 1);

    let outcome = run(&env, &plan, &Cancellation::new(), |_| {}).unwrap();
    assert_eq!(outcome.outputs, 1);
    assert_eq!(outcome.skipped, 0);

    let produced = files_with_extension(&out, "mp3");
    assert_eq!(names(&produced), ["tone.mp3"]);
    let converted = info(&env, &produced[0]);
    assert_eq!(converted.codec_name.as_deref(), Some("mp3"));
    assert!(
        (converted.duration_s - source_info.duration_s).abs() < 0.5,
        "转完时长变了：{} → {}",
        source_info.duration_s,
        converted.duration_s
    );
}

// ---- 覆盖策略 ----

#[test]
fn skip_is_reported_as_skipped_not_as_an_output() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("run-skip");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let out = dir.mkdir("out");
    let source_info = info(&env, &source);

    // 实测 ffmpeg 9.0.2：`-n` 撞上已存在文件时**退出码是 0**，只在 stderr 里写一句
    // "already exists"。拿退出码判断就会把跳过谎报成产出。
    let existing = out.join("tone.mp3");
    std::fs::write(&existing, b"SENTINEL").unwrap();

    let settings = ConvertSettings {
        target: AudioFormat::Mp3,
        output_dir: Some(out.clone()),
        ..Default::default()
    };
    let plan = plan_convert(&source, &source_info, &settings).unwrap();
    let outcome = run(&env, &plan, &Cancellation::new(), |_| {}).unwrap();

    assert_eq!(outcome.skipped, 1);
    assert_eq!(outcome.outputs, 0, "跳过不能算产出");
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap(),
        "SENTINEL",
        "跳过就必须一个字都不动"
    );
}

// ---- 进度 ----

#[test]
fn progress_climbs_to_a_hundred() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("run-progress");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let out = dir.mkdir("out");
    let source_info = info(&env, &source);

    let settings = SplitSettings {
        output_dir: Some(out.clone()),
        ..split_settings(SplitMode::ByCount(3))
    };
    let plan = plan_split(&source, &source_info, &settings).unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    run(&env, &plan, &Cancellation::new(), move |percent| {
        sink.lock().unwrap().push(percent);
    })
    .unwrap();

    let events = seen.lock().unwrap();
    assert!(!events.is_empty(), "一条进度都没收到");
    for window in events.windows(2) {
        assert!(
            window[1] >= window[0] - 0.001,
            "进度条倒退了：{} → {}",
            window[0],
            window[1]
        );
    }
    for percent in events.iter() {
        assert!((0.0..=100.0).contains(percent), "进度越界：{percent}");
    }
    // 收尾那一下必须打到 100，否则界面上永远停在 99%。
    assert_eq!(*events.last().unwrap(), 100.0);
}

// ---- 取消 ----

#[test]
fn cancel_stops_the_process_and_removes_the_half_written_output() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("run-cancel");
    // 要一个跑得够久的任务，取消才落得进"正在跑"的窗口里。240 秒 wav 转 mp3 大约十几秒。
    let source = sine_of_seconds(&env, &dir, "long.wav", AudioFormat::Wav, 240);
    let out = dir.mkdir("out");
    let source_info = info(&env, &source);

    let settings = ConvertSettings {
        target: AudioFormat::Mp3,
        quality: Quality::High,
        output_dir: Some(out.clone()),
        ..Default::default()
    };
    let plan = plan_convert(&source, &source_info, &settings).unwrap();
    let output_path = out.join("long.mp3");

    let cancel = Cancellation::new();
    let worker = cancel.clone();
    let events = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&events);

    let result = run(&env, &plan, &cancel, move |_| {
        // 收到第一条进度就说明 ffmpeg 真的在跑，这时候取消才是"取消一个进行中的任务"。
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            worker.cancel();
        }
    });

    assert!(
        events.load(Ordering::SeqCst) > 0,
        "一条进度都没收到就取消不了，把 long.wav 造长一点"
    );
    assert!(
        matches!(result, Err(RunError::Cancelled)),
        "取消以后应该返回 Cancelled，实际是 {result:?}"
    );
    assert!(
        !output_path.exists(),
        "半成品必须删掉：它探测得出来、也能播前半段，用户不会当它是垃圾"
    );

    #[cfg(unix)]
    assert_eq!(
        live_ffmpeg_for(dir.path().to_str().unwrap()),
        0,
        "取消以后不该留下孤儿 ffmpeg 进程"
    );
}

#[test]
fn cancelling_before_the_run_starts_never_spawns_a_process() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("run-cancel-early");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let out = dir.mkdir("out");
    let source_info = info(&env, &source);

    let settings = SplitSettings {
        output_dir: Some(out.clone()),
        ..split_settings(SplitMode::ByCount(3))
    };
    let plan = plan_split(&source, &source_info, &settings).unwrap();

    let cancel = Cancellation::new();
    cancel.cancel();
    let result = run(&env, &plan, &cancel, |_| {});

    assert!(matches!(result, Err(RunError::Cancelled)));
    assert!(
        files_with_extension(&out, "m4a").is_empty(),
        "还没开始就取消了，输出目录应该是空的"
    );
}

// ---- 失败路径 ----

#[test]
fn a_failure_carries_ffmpegs_own_reason() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("run-fail");
    // 探测失败的文件根本进不了规划（队列里会停在 ProbeFailed），所以这里用真文件探出
    // 一份 MediaInfo，再拿它去规划一个内容是纯文本的 .m4a：规划过得去，ffmpeg 会失败。
    // MediaInfo 是 #[non_exhaustive]，测试里造不出来，只能这么借。
    let real = sine(&env, &dir, "real.m4a", AudioFormat::M4a);
    let real_info = info(&env, &real);
    let broken = dir.write_bytes("broken.m4a", "这不是音频，只是一段文字".as_bytes());
    let out = dir.mkdir("out");

    let settings = SplitSettings {
        output_dir: Some(out),
        ..split_settings(SplitMode::ByCount(3))
    };
    let plan = plan_split(&broken, &real_info, &settings).unwrap();
    let err = run(&env, &plan, &Cancellation::new(), |_| {}).unwrap_err();

    match err {
        RunError::Failed(message) => {
            assert!(!message.trim().is_empty(), "失败原因不能是空的");
            assert_ne!(message, "ffmpeg 失败了，但没有给出原因");
        }
        other => panic!("期望 Failed，实际是 {other:?}"),
    }
}
