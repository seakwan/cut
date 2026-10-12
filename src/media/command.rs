//! 构造 ffmpeg 命令行参数。
//!
//! 纯函数、只产出 `Vec<String>`，由上层交给 `std::process::Command`（不经过 shell，
//! 所以空格和中文文件名天然安全）。每种模式的参数都在这里做快照测试。

use std::path::Path;

/// 每次都带的公共参数：`-nostdin` 防止 ffmpeg 抢标准输入，`-progress pipe:1` 给出机器可读进度。
const COMMON: [&str; 7] = [
    "-hide_banner",
    "-nostdin",
    "-nostats",
    "-loglevel",
    "error",
    "-progress",
    "pipe:1",
];

/// 支持的音频容器。输入输出共用这一套。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioFormat {
    M4a,
    Mp3,
    Wav,
    Flac,
    Aac,
    Ogg,
}

impl AudioFormat {
    pub const ALL: [Self; 6] = [
        Self::M4a,
        Self::Mp3,
        Self::Wav,
        Self::Flac,
        Self::Aac,
        Self::Ogg,
    ];

    pub fn from_index(ix: usize) -> Self {
        Self::ALL.get(ix).copied().unwrap_or(Self::Mp3)
    }

    pub fn index(&self) -> usize {
        Self::ALL.iter().position(|item| item == self).unwrap_or(1)
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        Self::from_extension(path.extension()?.to_str()?)
    }

    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "m4a" | "m4b" | "mp4" => Some(Self::M4a),
            "mp3" => Some(Self::Mp3),
            "wav" | "wave" | "aiff" | "aif" => Some(Self::Wav),
            "flac" => Some(Self::Flac),
            "aac" | "adts" => Some(Self::Aac),
            "ogg" | "oga" | "opus" => Some(Self::Ogg),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::M4a => "M4A",
            Self::Mp3 => "MP3",
            Self::Wav => "WAV",
            Self::Flac => "FLAC",
            Self::Aac => "AAC",
            Self::Ogg => "OGG",
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            Self::M4a => "m4a",
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Aac => "aac",
            Self::Ogg => "ogg",
        }
    }

    /// segment muxer 的 `-segment_format` 取值，m4a 对应 `ipod`。
    pub fn segment_format(&self) -> &'static str {
        match self {
            Self::M4a => "ipod",
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Aac => "adts",
            Self::Ogg => "ogg",
        }
    }

    /// 装不装得下封面图。
    ///
    /// 装不下的三个，实测（ffmpeg 9.0.2，源是带 mjpeg attached_pic 的 m4a）**不是"封面丢了
    /// 但音频还在"，而是整条命令失败、产出 0 字节**：
    /// - wav：`wav muxer does not support any stream of type video`
    /// - aac：`adts muxer does not support any stream of type video`
    /// - ogg：`Unsupported codec id in stream 1`（mjpeg 进不了 ogg）
    ///
    /// 所以这个判断是"命令能不能跑通"的判断，不是"效果好不好"的判断。
    /// 依据见 tests/convert_matrix.rs 的 a_cover_survives_exactly_the_targets_that_can_hold_it。
    pub fn supports_attached_pic(&self) -> bool {
        matches!(self, Self::M4a | Self::Mp3 | Self::Flac)
    }

    /// 有没有地方放标签。
    ///
    /// 只有 AAC 不行：裸 ADTS 流根本没有元数据容器，`-map_metadata 0` 写进去也读不出来
    /// （实测 ffprobe 的 format_tags 和 stream_tags 都是空的）。其余五种都留得住，
    /// 只是 **ogg 把标签写在 stream 上而不是 format 上**，查的时候两处都得看。
    pub fn supports_tags(&self) -> bool {
        !matches!(self, Self::Aac)
    }

    /// 能不能用 segment muxer 一趟切完。
    ///
    /// FLAC 不行，而且**重编码也不行**，实测（ffmpeg 9.0.2，9 秒源按 3 秒切）：
    /// `-c:a flac -f segment` 出来的 d_1/d_2/d_3 探测时长全是 `N/A`，
    /// 最后那个渣段反而报 `9.000000`。整条路径对 FLAC 都是废的。
    ///
    /// 依据见 tests/ffmpeg_integration.rs 的 flac_copy_segmenting_reports_the_wrong_duration。
    pub fn supports_segment_muxer(&self) -> bool {
        !matches!(self, Self::Flac)
    }

    /// 能不能无损直拷着切（`-c copy`）。
    ///
    /// FLAC 不行，实测（ffmpeg 9.0.2）：`-ss 3 -t 3 -i tone.flac -c copy` 出来的段，
    /// format 和 stream 两处 duration 都报 `9.000000`，也就是整个源文件的长度；
    /// `-ss` 放到 `-i` 后面结果一样。数据本身是分开写的（每段体积各不相同），
    /// 坏的是 STREAMINFO 里的 total_samples——它被整份抄了过去。
    /// 播放器会显示错误长度，用户看到的就是坏文件。
    ///
    /// FLAC 是无损编码，**重编码回 FLAC 不掉音质**，所以退化成重编码是可接受的，
    /// 只是慢一些。逐段调用 + `-c:a flac` 实测每段时长正确（3.000000）。
    pub fn supports_stream_copy(&self) -> bool {
        !matches!(self, Self::Flac)
    }
}

/// 目标已存在时怎么办；`重命名` 由上层挑一个空闲路径，所以这里只需要 ffmpeg 的两个开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overwrite {
    Overwrite,
    /// 默认不动已有文件：批量任务里覆盖是不可逆的。
    #[default]
    Skip,
}

impl Overwrite {
    pub fn flag(&self) -> &'static str {
        match self {
            Self::Overwrite => "-y",
            Self::Skip => "-n",
        }
    }

    /// 两个 tab 共用同一套措辞，避免同一个开关在不同地方叫不同名字。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Overwrite => "覆盖",
            Self::Skip => "跳过",
        }
    }

    pub const OPTIONS: [Self; 2] = [Self::Skip, Self::Overwrite];

    pub fn from_index(ix: usize) -> Self {
        Self::OPTIONS.get(ix).copied().unwrap_or_default()
    }

    pub fn index(&self) -> usize {
        Self::OPTIONS
            .iter()
            .position(|item| item == self)
            .unwrap_or(0)
    }
}

/// 重编码质量档。无损格式（FLAC/WAV）忽略它。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    High,
    #[default]
    Medium,
    Low,
}

impl Quality {
    pub fn label(&self) -> &'static str {
        match self {
            Self::High => "高",
            Self::Medium => "中",
            Self::Low => "低",
        }
    }

    pub const OPTIONS: [Self; 3] = [Self::High, Self::Medium, Self::Low];

    pub fn from_index(ix: usize) -> Self {
        Self::OPTIONS.get(ix).copied().unwrap_or_default()
    }

    pub fn index(&self) -> usize {
        Self::OPTIONS
            .iter()
            .position(|item| item == self)
            .unwrap_or(1)
    }
}

/// 默认无损直拷；精确模式重新编码，切点样本级准确。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    #[default]
    Copy,
    Reencode(Quality),
}

/// 源音频编码，决定重编码时用哪个编码器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceCodec {
    Aac,
    Mp3,
    Flac,
    Vorbis,
    Opus,
    Alac,
    Pcm,
    Other,
}

impl SourceCodec {
    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "aac" => Self::Aac,
            "mp3" | "mp3float" => Self::Mp3,
            "flac" => Self::Flac,
            "vorbis" => Self::Vorbis,
            "opus" => Self::Opus,
            "alac" => Self::Alac,
            name if name.starts_with("pcm_") => Self::Pcm,
            _ => Self::Other,
        }
    }
}

/// 一趟切多段（按时长 / 按大小），走 segment muxer。
#[derive(Debug)]
pub struct SegmentSplitRequest<'a> {
    pub input: &'a Path,
    /// 含 `%03d` 的输出模板，例如 `/out/song_part%03d.m4a`。
    pub output_pattern: &'a str,
    pub format: AudioFormat,
    pub segment_seconds: f64,
    pub encoding: Encoding,
    pub source: SourceCodec,
    pub source_bitrate: Option<u64>,
    pub keep_tags_and_cover: bool,
    pub overwrite: Overwrite,
}

/// 只切一段（按份数用逐段调用，保证恰好 N 个文件）。
#[derive(Debug)]
pub struct RangeSplitRequest<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub format: AudioFormat,
    pub start_seconds: f64,
    pub duration_seconds: f64,
    pub encoding: Encoding,
    pub source: SourceCodec,
    pub source_bitrate: Option<u64>,
    pub keep_tags_and_cover: bool,
    pub overwrite: Overwrite,
}

/// 整文件转格式。
#[derive(Debug)]
pub struct ConvertRequest<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub target: AudioFormat,
    pub quality: Quality,
    pub source: SourceCodec,
    pub source_bitrate: Option<u64>,
    pub keep_tags_and_cover: bool,
    pub overwrite: Overwrite,
}

pub fn split_segments(request: &SegmentSplitRequest) -> Vec<String> {
    let mut args = prefix(request.overwrite);
    args.extend(input(request.input));
    args.extend(stream_args(
        request.encoding,
        request.format,
        request.source,
        request.source_bitrate,
        request.keep_tags_and_cover,
    ));
    args.extend(strings([
        "-f",
        "segment",
        "-segment_time",
        &format_seconds(request.segment_seconds),
        // 少了这个，每段的起始时间戳会接着上一段涨，播放器显示的时间就全乱了。
        "-reset_timestamps",
        "1",
        // ffmpeg 默认从 0 开始编号，而界面上的 `{index}` 是 1 起（`plan::render_name`）。
        // 不统一的话，换个切割方式文件名就变了。
        "-segment_start_number",
        "1",
        "-segment_format",
        request.format.segment_format(),
        request.output_pattern,
    ]));
    args
}

pub fn split_range(request: &RangeSplitRequest) -> Vec<String> {
    let mut args = prefix(request.overwrite);
    // `-ss` 放在 `-i` 前面是输入端 seek，对音频足够准且快得多。
    args.extend(strings(["-ss", &format_seconds(request.start_seconds)]));

    match request.encoding {
        Encoding::Copy => {
            args.extend(strings(["-t", &format_seconds(request.duration_seconds)]));
            args.extend(input(request.input));
            args.extend(stream_args(
                request.encoding,
                request.format,
                request.source,
                request.source_bitrate,
                request.keep_tags_and_cover,
            ));
        }
        Encoding::Reencode(_) => {
            args.extend(input(request.input));
            args.extend(strings(["-t", &format_seconds(request.duration_seconds)]));
            args.extend(stream_args(
                request.encoding,
                request.format,
                request.source,
                request.source_bitrate,
                request.keep_tags_and_cover,
            ));
        }
    }

    args.push(request.output.to_string_lossy().into_owned());
    args
}

pub fn convert(request: &ConvertRequest) -> Vec<String> {
    let mut args = prefix(request.overwrite);
    args.extend(input(request.input));
    args.extend(stream_args(
        Encoding::Reencode(request.quality),
        request.target,
        request.source,
        request.source_bitrate,
        request.keep_tags_and_cover,
    ));
    if request.target == AudioFormat::Mp3 {
        // id3v2.3 的兼容性最好，老播放器也认。
        args.extend(strings(["-id3v2_version", "3", "-write_id3v1", "1"]));
    }
    args.push(request.output.to_string_lossy().into_owned());
    args
}

/// 源和目标容器、编码都相同时用得上：只换壳，不重编码。
///
/// `keep_tags_and_cover` 照样要传：换壳不等于"用户没表态"，
/// 关掉了就不该把标签和封面带过去，哪怕技术上白送。
pub fn remux(
    source: &Path,
    destination: &Path,
    overwrite: Overwrite,
    keep_tags_and_cover: bool,
) -> Vec<String> {
    let mut args = prefix(overwrite);
    args.extend(input(source));
    args.extend(metadata_args(keep_tags_and_cover));
    args.extend(strings(["-map", "0", "-c", "copy"]));
    args.push(destination.to_string_lossy().into_owned());
    args
}

fn prefix(overwrite: Overwrite) -> Vec<String> {
    let mut args = strings(COMMON);
    args.push(overwrite.flag().to_string());
    args
}

fn input(path: &Path) -> Vec<String> {
    strings(["-i", &path.to_string_lossy()])
}

/// 元数据映射。
///
/// ffmpeg **默认就会**把第一个输入的全局元数据抄到输出，所以「保留」那半边其实是空操作；
/// 真正必须显式写出来的是「不保留」——`-map_metadata -1`。少这一句的话，界面上明明关掉了
/// 「标签与封面」，用户还是会在产出文件里看到原标题和艺术家。
/// 流级别的元数据要单独关（ogg 把标签写在 stream 上），所以两条都给。
fn metadata_args(keep_tags_and_cover: bool) -> Vec<String> {
    if keep_tags_and_cover {
        strings(["-map_metadata", "0"])
    } else {
        strings(["-map_metadata", "-1", "-map_metadata:s:v", "-1"])
    }
}

fn stream_args(
    encoding: Encoding,
    target: AudioFormat,
    source: SourceCodec,
    source_bitrate: Option<u64>,
    keep_tags_and_cover: bool,
) -> Vec<String> {
    match encoding {
        Encoding::Copy => {
            let mut args = metadata_args(keep_tags_and_cover);
            args.extend(strings(["-map", "0", "-c", "copy"]));
            args
        }
        Encoding::Reencode(quality) => {
            let mut args = metadata_args(keep_tags_and_cover);
            args.extend(strings(["-map", "0:a"]));
            if keep_tags_and_cover && target.supports_attached_pic() {
                // `0:v?` 的问号让纯音频文件不报错；disposition 是播放器认出封面的关键。
                args.extend(strings([
                    "-map",
                    "0:v?",
                    "-c:v",
                    "copy",
                    "-disposition:v",
                    "attached_pic",
                ]));
            }
            args.extend(encoder_args(target, source, source_bitrate, quality));
            args
        }
    }
}

fn encoder_args(
    target: AudioFormat,
    source: SourceCodec,
    source_bitrate: Option<u64>,
    quality: Quality,
) -> Vec<String> {
    match target {
        AudioFormat::M4a | AudioFormat::Aac => {
            let mut args = strings(["-c:a", "aac"]);
            args.extend(aac_bitrate(source, source_bitrate, quality));
            args
        }
        AudioFormat::Mp3 => strings([
            "-c:a",
            "libmp3lame",
            "-q:a",
            match quality {
                Quality::High => "0",
                Quality::Medium => "2",
                Quality::Low => "5",
            },
        ]),
        AudioFormat::Flac => strings(["-c:a", "flac"]),
        AudioFormat::Wav => strings(["-c:a", "pcm_s16le"]),
        AudioFormat::Ogg => {
            if source == SourceCodec::Opus {
                strings([
                    "-c:a",
                    "libopus",
                    "-b:a",
                    match quality {
                        Quality::High => "160k",
                        Quality::Medium => "128k",
                        Quality::Low => "96k",
                    },
                ])
            } else {
                strings([
                    "-c:a",
                    "libvorbis",
                    "-q:a",
                    match quality {
                        Quality::High => "7",
                        Quality::Medium => "5",
                        Quality::Low => "3",
                    },
                ])
            }
        }
    }
}

/// 源就是 AAC 时沿用源码率，避免"转一次掉一档"；否则按质量档给一个常见码率。
fn aac_bitrate(source: SourceCodec, source_bitrate: Option<u64>, quality: Quality) -> Vec<String> {
    if source == SourceCodec::Aac {
        if let Some(bitrate) = source_bitrate {
            return strings(["-b:a", &format!("{bitrate}")]);
        }
    }
    strings([
        "-b:a",
        match quality {
            Quality::High => "192k",
            Quality::Medium => "128k",
            Quality::Low => "96k",
        },
    ])
}

fn strings<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    items.into_iter().map(str::to_string).collect()
}

/// 3 位小数，和 `plan::round3` 对齐；ffmpeg 接受裸秒数。
fn format_seconds(seconds: f64) -> String {
    format!("{seconds:.3}")
}
