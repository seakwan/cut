//! ffmpeg 能力层。
//!
//! 这一层不认识 gpui_kit：只负责定位二进制、探测媒体信息、构造命令行、解析进度，
//! 因此能脱离窗口做单元测试。上层 `split` / `convert` 用这里的类型组装任务。

mod command;
mod ffmpeg;
mod probe;
mod progress;

pub use command::{
    convert, remux, split_range, split_segments, AudioFormat, ConvertRequest, Encoding, Overwrite,
    Quality, RangeSplitRequest, SegmentSplitRequest, SourceCodec,
};
pub use ffmpeg::{binary_name, no_console_window, BinarySource, FfmpegEnv, ResolveError, ENV_DIR};
pub use probe::{media_info_from_json, probe, MediaInfo, ProbeError};
pub use progress::{parse_progress_line, percent_of, ProgressField};
