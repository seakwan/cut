//! 用 ffprobe 读媒体信息。
//!
//! JSON 解析是纯函数（`media_info_from_json`），单独可测；进程调用只在 `probe` 里。

use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use super::ffmpeg::{no_console_window, FfmpegEnv};

/// 切割和转换都要用到的媒体信息。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MediaInfo {
    pub duration_s: f64,
    /// 整体码率，bit/s。VBR 文件这是平均值，按大小切割时是有意的近似。
    pub bitrate_bps: Option<u64>,
    pub size_bytes: Option<u64>,
    pub codec_name: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub has_cover_art: bool,
}

#[derive(Debug)]
pub enum ProbeError {
    MalformedJson(String),
    MissingDuration,
    NoAudioStream,
    ProcessFailed(String),
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedJson(err) => write!(f, "ffprobe 输出解析失败：{err}"),
            Self::MissingDuration => f.write_str("读不到时长"),
            Self::NoAudioStream => f.write_str("文件里没有音频流"),
            Self::ProcessFailed(err) => write!(f, "ffprobe 失败：{err}"),
        }
    }
}

impl std::error::Error for ProbeError {}

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    format: Option<FfprobeFormat>,
    streams: Option<Vec<FfprobeStream>>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
    #[serde(rename = "bit_rate")]
    bit_rate: Option<String>,
    size: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    #[serde(rename = "bit_rate")]
    bit_rate: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
    disposition: Option<FfprobeDisposition>,
}

#[derive(Debug, Deserialize)]
struct FfprobeDisposition {
    attached_pic: Option<i64>,
}

pub fn probe(ffmpeg: &FfmpegEnv, path: &Path) -> Result<MediaInfo, ProbeError> {
    let mut command = Command::new(ffmpeg.ffprobe());
    command
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path);
    no_console_window(&mut command);

    let output = command
        .output()
        .map_err(|err| ProbeError::ProcessFailed(err.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim().lines().next_back().unwrap_or("未知错误");
        return Err(ProbeError::ProcessFailed(detail.to_string()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    media_info_from_json(&stdout)
}

pub fn media_info_from_json(json: &str) -> Result<MediaInfo, ProbeError> {
    let parsed: FfprobeOutput =
        serde_json::from_str(json).map_err(|err| ProbeError::MalformedJson(err.to_string()))?;

    let streams = parsed.streams.unwrap_or_default();
    let audio = streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("audio"))
        .ok_or(ProbeError::NoAudioStream)?;

    let format = parsed.format.unwrap_or(FfprobeFormat {
        duration: None,
        bit_rate: None,
        size: None,
    });

    let duration_s = number(&format.duration).ok_or(ProbeError::MissingDuration)?;
    let size_bytes = number::<u64>(&format.size);

    // 码率回退链：容器整体 → 音频流 → 由体积和时长推算（VBR/ogg 常缺前两个）。
    let bitrate_bps = number::<u64>(&format.bit_rate)
        .or_else(|| number::<u64>(&audio.bit_rate))
        .or_else(|| {
            let size = size_bytes?;
            (duration_s > 0.0).then_some((size as f64 * 8.0 / duration_s) as u64)
        });

    Ok(MediaInfo {
        duration_s,
        bitrate_bps,
        size_bytes,
        codec_name: audio.codec_name.clone(),
        sample_rate: number::<u32>(&audio.sample_rate),
        channels: audio.channels,
        has_cover_art: streams.iter().any(|stream| {
            stream
                .disposition
                .as_ref()
                .and_then(|disposition| disposition.attached_pic)
                == Some(1)
        }),
    })
}

/// ffprobe 用字符串装数字，缺失时写 `"N/A"`。
fn number<T>(raw: &Option<String>) -> Option<T>
where
    T: std::str::FromStr,
{
    let raw = raw.as_deref()?.trim();
    if raw.is_empty() || raw == "N/A" {
        return None;
    }
    raw.parse::<T>().ok()
}
