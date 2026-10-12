//! 集成测试共用的小工具。
//!
//! 放在 `tests/common/` 下：cargo 只把 `tests/*.rs` 当测试目标，子目录里的模块
//! 不会被单独编译成一个空测试二进制。
#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use cut::media::{probe, AudioFormat, FfmpegEnv, MediaInfo, SourceCodec};

/// 造一个 10 秒的测试文件用不到这么长，但太短会让 segment muxer 只切出一段。
pub const FIXTURE_SECONDS: u32 = 9;

/// 找不到二进制就返回 None，调用方 `let Some(env) = env_or_skip() else { return };` 跳过。
/// 这样没跑过 `script/build-ffmpeg.sh` 的机器上 `cargo test` 也不会红。
pub fn env_or_skip() -> Option<FfmpegEnv> {
    match FfmpegEnv::resolve() {
        Ok(env) => {
            eprintln!(
                "集成测试用 {}：{}",
                env.source().label(),
                env.ffmpeg().display()
            );
            Some(env)
        }
        Err(_) => {
            eprintln!("跳过：找不到 ffmpeg/ffprobe，先跑 script/build-ffmpeg.sh");
            None
        }
    }
}

pub fn run<S: AsRef<OsStr>>(env: &FfmpegEnv, args: &[S]) -> Output {
    std::process::Command::new(env.ffmpeg())
        .args(args)
        .output()
        .expect("起不了 ffmpeg 进程")
}

pub fn run_probe<S: AsRef<OsStr>>(env: &FfmpegEnv, args: &[S]) -> Output {
    std::process::Command::new(env.ffprobe())
        .args(args)
        .output()
        .expect("起不了 ffprobe 进程")
}

/// 跑并断言成功；失败时把 stderr 打出来，否则只看到一个退出码根本没法查。
pub fn run_ok<S: AsRef<OsStr>>(env: &FfmpegEnv, args: &[S]) -> Output {
    let output = run(env, args);
    assert!(
        output.status.success(),
        "ffmpeg 失败（{}）：{}",
        args.iter()
            .map(|arg| arg.as_ref().to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

pub fn fixture_encoder(format: AudioFormat) -> &'static str {
    match format {
        AudioFormat::M4a | AudioFormat::Aac => "aac",
        AudioFormat::Mp3 => "libmp3lame",
        AudioFormat::Wav => "pcm_s16le",
        AudioFormat::Flac => "flac",
        AudioFormat::Ogg => "libvorbis",
    }
}

/// 造一个正弦波音频文件，就是 build.md 里那条手工验收配方。
pub fn sine(env: &FfmpegEnv, dir: &TempDir, name: &str, format: AudioFormat) -> PathBuf {
    sine_of_seconds(env, dir, name, format, FIXTURE_SECONDS)
}

/// 同上，但时长可指定。取消测试要一个够长的文件，好让 cancel 落在跑的中途。
pub fn sine_of_seconds(
    env: &FfmpegEnv,
    dir: &TempDir,
    name: &str,
    format: AudioFormat,
    seconds: u32,
) -> PathBuf {
    let path = dir.join(name);
    let target = path.to_str().unwrap();
    let source = format!("sine=frequency=440:duration={seconds}");
    run_ok(
        env,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "lavfi",
            "-i",
            source.as_str(),
            "-c:a",
            fixture_encoder(format),
            target,
        ],
    );
    path
}

/// 造一个带标题、艺术家和封面的源文件，专门用来验「标签与封面」这条开关。
///
/// 只有装得下 attached_pic 的容器能当源（m4a / mp3 / flac）；wav、adts、ogg 塞不进封面，
/// 拿它们造源只会得到一条假通过的测试。
pub fn sine_with_cover(env: &FfmpegEnv, dir: &TempDir, name: &str, format: AudioFormat) -> PathBuf {
    let cover = dir.join("cover.jpg");
    run_ok(
        env,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=64x64",
            "-frames:v",
            "1",
            "-c:v",
            "mjpeg",
            cover.to_str().unwrap(),
        ],
    );

    let path = dir.join(name);
    let source = format!("sine=frequency=440:duration={FIXTURE_SECONDS}");
    run_ok(
        env,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "lavfi",
            "-i",
            source.as_str(),
            "-i",
            cover.to_str().unwrap(),
            "-map",
            "0:a",
            "-map",
            "1:v",
            "-c:a",
            fixture_encoder(format),
            "-c:v",
            "copy",
            "-disposition:v",
            "attached_pic",
            "-metadata",
            "title=测试标题",
            "-metadata",
            "artist=某人",
            path.to_str().unwrap(),
        ],
    );
    path
}

/// 读标签。ogg 把标签写在 stream 上而不是 format 上，所以两处都要看，
/// 只看 `format_tags` 会把"其实留着"误报成"丢了"。
pub fn tags(env: &FfmpegEnv, path: &Path) -> String {
    let output = run_probe(
        env,
        &[
            "-v",
            "error",
            "-show_entries",
            "format_tags=title,artist:stream_tags=title,artist",
            "-of",
            "csv=p=0",
            path.to_str().unwrap(),
        ],
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn info(env: &FfmpegEnv, path: &Path) -> MediaInfo {
    probe(env, path).unwrap_or_else(|err| panic!("探测 {} 失败：{err}", path.display()))
}

pub fn total_duration(env: &FfmpegEnv, files: &[PathBuf]) -> f64 {
    files
        .iter()
        .map(|file| info(env, file).duration_s)
        .sum::<f64>()
}

/// 目录里某个扩展名的文件，按名字排序（segment muxer 用 %03d，字典序就是数字序）。
pub fn files_with_extension(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(OsStr::to_str) == Some(extension))
        .collect();
    files.sort();
    files
}

pub fn source_of(env: &FfmpegEnv, path: &Path) -> (SourceCodec, Option<u64>) {
    let info = info(env, path);
    (
        SourceCodec::from_name(info.codec_name.as_deref().unwrap_or("")),
        info.bitrate_bps,
    )
}

/// 测试专用临时目录，Drop 时自己清掉。
///
/// 不引 tempfile crate：全项目只有测试用得上，多一个依赖不如这二十行。
pub struct TempDir(PathBuf);

impl TempDir {
    /// `name` 用来区分同一批测试里并行的多个目录，别在两个测试里重名。
    pub fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("cut-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    /// 建子目录并返回路径。
    pub fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.join(relative);
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// 写一个占位文件（内容是 "x"），返回路径。只要"存在且是文件"的场合用它。
    pub fn write(&self, relative: &str) -> PathBuf {
        self.write_bytes(relative, b"x")
    }

    pub fn write_bytes(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
        path
    }

    /// 目录下的文件名，排序过，方便直接断言。
    pub fn names_in(&self, relative: &str) -> Vec<String> {
        let dir = self.join(relative);
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("读不了 {}：{err}", dir.display()))
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 有没有 ffmpeg 进程的命令行里还带着这个目录。取消以后必须是 0，
/// 否则关窗就会留下一堆在后台啃 CPU 的孤儿。
#[cfg(unix)]
pub fn live_ffmpeg_for(marker: &str) -> usize {
    let output = std::process::Command::new("ps")
        .args(["-axo", "command="])
        .output()
        .expect("跑不了 ps");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains("ffmpeg") && line.contains(marker))
        .count()
}
