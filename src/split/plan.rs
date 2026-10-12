//! 切割数学：把「切几份 / 每段多长 / 每段多大」翻译成具体的分段表。
//!
//! 纯函数，不碰 I/O、不碰 gpui_kit，所有边界情况都在这里单测。

use std::fmt;

/// 小于这个长度的分段没有意义：比一个音频帧还短，ffmpeg 也切不出来。
pub const MIN_SEGMENT_SECONDS: f64 = 0.1;

/// 用户选的切割维度。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SplitMode {
    /// 平均切成 N 份，最后一段吸收余数。
    ByCount(u32),
    /// 每段不超过给定的秒数。
    ByDuration(f64),
    /// 每段不超过给定的字节数，需要已知码率。
    BySize(u64),
}

/// 一段的时间区间，秒。序号就是它在 `plan_segments` 结果里的位置。
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Segment {
    pub start_s: f64,
    pub duration_s: f64,
}

/// 界面上的三个切割维度，顺序就是单选组的顺序。
pub const DIMENSIONS: [&str; 3] = ["按数量", "按时长", "按文件大小"];

/// 每个维度在输入框里用的单位。按大小用 MB（1000 进制），和文件管理器显示口径一致。
pub const DIMENSION_UNITS: [&str; 3] = ["份", "秒", "MB"];

/// 切到某个维度时输入框预填的值。
pub const DIMENSION_DEFAULTS: [f64; 3] = [2.0, 300.0, 10.0];

const BYTES_PER_MB: f64 = 1_000_000.0;

/// 输入框里的数值文本 ↔ `f64`。
///
/// 单独抽出来是因为两个方向都有讲究：解析要容忍 `1,000`、全角空格和空串，
/// 显示要让整数不带 `.0`（`300 秒` 而不是 `300.0 秒`）。
pub struct DimensionValue;

impl DimensionValue {
    pub fn parse(text: &str) -> Option<f64> {
        let cleaned: String = text
            .chars()
            .filter(|c| !c.is_whitespace() && *c != ',')
            .collect();
        if cleaned.is_empty() {
            return None;
        }
        let value = cleaned.parse::<f64>().ok()?;
        value.is_finite().then_some(value)
    }

    pub fn render(value: f64) -> String {
        if value.fract() == 0.0 {
            return format!("{}", value as i64);
        }
        let text = format!("{value:.3}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

impl SplitMode {
    pub fn dimension_index(&self) -> usize {
        match self {
            Self::ByCount(_) => 0,
            Self::ByDuration(_) => 1,
            Self::BySize(_) => 2,
        }
    }

    /// 输入框里的数值（按维度带各自的单位）。
    pub fn dimension_value(&self) -> f64 {
        match self {
            Self::ByCount(count) => f64::from(*count),
            Self::ByDuration(seconds) => *seconds,
            Self::BySize(bytes) => *bytes as f64 / BYTES_PER_MB,
        }
    }

    /// 数值非法（0、负数、份数带小数）时返回 `None`，由界面提示，不在这里猜。
    pub fn from_dimension(index: usize, value: f64) -> Option<Self> {
        if !(value.is_finite()) || value <= 0.0 {
            return None;
        }
        match index {
            0 => {
                let count = value as u32;
                (f64::from(count) == value && count > 0).then_some(Self::ByCount(count))
            }
            1 => Some(Self::ByDuration(value)),
            2 => Some(Self::BySize((value * BYTES_PER_MB) as u64)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    NonPositiveDuration,
    InvalidCount,
    NonPositiveTarget,
    /// 按大小切割必须知道码率；VBR 文件用平均码率近似。
    UnknownBitrate,
    SegmentTooShort,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::NonPositiveDuration => "读不到有效的音频时长",
            Self::InvalidCount => "份数必须是大于 0 的整数",
            Self::NonPositiveTarget => "目标值必须大于 0",
            Self::UnknownBitrate => "无法确定码率，不能按大小切割。可改用按时长或按份数",
            Self::SegmentTooShort => "按这个设置算出来的每段太短，请调大目标值",
        };
        f.write_str(message)
    }
}

impl std::error::Error for PlanError {}

/// 每段的目标秒数。按大小时用码率换算，向下取到 0.1 秒（宁可每段略小，不要超出上限）。
pub fn segment_seconds(
    duration_s: f64,
    bitrate_bps: Option<u64>,
    mode: SplitMode,
) -> Result<f64, PlanError> {
    if !(duration_s > 0.0) || !duration_s.is_finite() {
        return Err(PlanError::NonPositiveDuration);
    }

    let seconds = match mode {
        SplitMode::ByCount(0) => return Err(PlanError::InvalidCount),
        SplitMode::ByCount(count) => duration_s / f64::from(count),
        SplitMode::ByDuration(seconds) => {
            if !(seconds > 0.0) || !seconds.is_finite() {
                return Err(PlanError::NonPositiveTarget);
            }
            seconds
        }
        SplitMode::BySize(bytes) => {
            if bytes == 0 {
                return Err(PlanError::NonPositiveTarget);
            }
            let bitrate = bitrate_bps
                .filter(|bitrate| *bitrate > 0)
                .ok_or(PlanError::UnknownBitrate)?;
            bytes as f64 * 8.0 / bitrate as f64
        }
    };

    let seconds = round3(seconds);
    if seconds < MIN_SEGMENT_SECONDS {
        return Err(PlanError::SegmentTooShort);
    }
    Ok(seconds)
}

/// 完整的分段表，用于界面预告和结果校验。
///
/// 文件本身不长于一段时返回单段（整个文件），这是提示而不是错误。
pub fn plan_segments(
    duration_s: f64,
    bitrate_bps: Option<u64>,
    mode: SplitMode,
) -> Result<Vec<Segment>, PlanError> {
    let per_segment = segment_seconds(duration_s, bitrate_bps, mode)?;

    if duration_s <= per_segment {
        return Ok(vec![Segment {
            start_s: 0.0,
            duration_s: round3(duration_s),
        }]);
    }

    // ByCount 单独走一条路：segment muxer 在帧边界切，可能给出 N±1 个文件，
    // 而逐段调用能保证恰好 N 份。
    if let SplitMode::ByCount(count) = mode {
        let count = count as usize;
        return Ok((0..count)
            .map(|ix| {
                let start = round3(per_segment * ix as f64);
                let duration = if ix + 1 == count {
                    round3(duration_s - start)
                } else {
                    per_segment
                };
                Segment {
                    start_s: start,
                    duration_s: duration,
                }
            })
            .collect());
    }

    let count = (duration_s / per_segment).ceil() as usize;
    Ok((0..count)
        .map(|ix| {
            let start = round3(per_segment * ix as f64);
            Segment {
                start_s: start,
                duration_s: round3((duration_s - start).min(per_segment)),
            }
        })
        .collect())
}

/// 输出文件名。`{index}` 是 1 起的序号，符合人在文件管理器里看到的顺序。
pub fn render_name(template: &str, stem: &str, ix: usize) -> String {
    template
        .replace("{name}", stem)
        .replace("{index}", &(ix + 1).to_string())
}

/// segment muxer 用的输出模板：把 `{index}` 换成 ffmpeg 自己认的 `%d`。
///
/// 配合 `-segment_start_number 1`（见 `command::split_segments`），产出的文件名和
/// `render_name` 在逐段调用路径下**完全一致**——都是 1 起、不补零。用户换切割方式
/// 不会看到两套命名，这是实测过的（`%d` 确实被 segment muxer 接受）。
///
/// 模板里没有 `{index}` 时返回 `None`：那样每一段都会写到同一个路径上互相覆盖，
/// 必须由调用方拦下来，不能默默产出一个文件。
pub fn render_pattern(template: &str, stem: &str) -> Option<String> {
    if !template.contains("{index}") {
        return None;
    }
    Some(template.replace("{name}", stem).replace("{index}", "%d"))
}

fn round3(seconds: f64) -> f64 {
    (seconds * 1000.0).round() / 1000.0
}
