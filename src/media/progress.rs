//! 解析 `ffmpeg -progress pipe:1` 的 key=value 输出。

/// 一行进度里我们关心的三种信息。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProgressField {
    /// 已写出的媒体时长（秒）。
    OutTime(f64),
    /// 处理速度倍数，例如 2.5 表示 2.5x。
    Speed(f64),
    /// `progress=end`：本次调用正常结束。
    End,
}

pub fn parse_progress_line(line: &str) -> Option<ProgressField> {
    let (key, value) = line.split_once('=')?;
    let value = value.trim();

    match key.trim() {
        "out_time" => parse_clock(value).map(ProgressField::OutTime),
        // ffmpeg 的历史包袱：out_time_ms 里装的其实是微秒，与 out_time_us 同义。
        "out_time_us" | "out_time_ms" => value
            .parse::<f64>()
            .ok()
            .map(|micros| ProgressField::OutTime(micros / 1_000_000.0)),
        "speed" => value
            .trim_end_matches('x')
            .parse::<f64>()
            .ok()
            .map(ProgressField::Speed),
        "progress" if value == "end" => Some(ProgressField::End),
        _ => None,
    }
}

/// `HH:MM:SS.uuuuuu`；个别构建只给秒数，也接住。
fn parse_clock(value: &str) -> Option<f64> {
    if value == "N/A" {
        return None;
    }

    let mut parts = value.split(':');
    let (Some(hours), Some(minutes), Some(seconds)) = (parts.next(), parts.next(), parts.next())
    else {
        return value.parse::<f64>().ok();
    };

    let hours: f64 = hours.trim().parse().ok()?;
    let minutes: f64 = minutes.trim().parse().ok()?;
    let seconds: f64 = seconds.trim().parse().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

/// 0–100 的百分比，总时长未知或已写时长不是有限数时给 0。
pub fn percent_of(out_time_s: f64, total_s: f64) -> f32 {
    // NaN 不能进 clamp，否则一路传到进度条上变成画不出来的宽度。
    if !(total_s > 0.0) || !out_time_s.is_finite() {
        return 0.0;
    }
    ((out_time_s / total_s) * 100.0).clamp(0.0, 100.0) as f32
}
