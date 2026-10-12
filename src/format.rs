//! 界面上反复用到的几个格式化：时长、体积、码率。
//!
//! 纯函数。放在这里而不是各视图里，是为了让切割和转换两个 tab 显示口径一致。

/// `3725.4` → `1:02:05`，`185.0` → `3:05`。不足一秒的部分丢掉，列表里没人在意。
pub fn duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "--:--".to_string();
    }

    let total = seconds as u64;
    let (hours, minutes, secs) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

/// 1000 进制：文件系统显示的就是这个口径，用 1024 会和访达对不上。
pub fn bytes(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn bitrate(bps: u64) -> String {
    format!("{} kbps", (bps as f64 / 1000.0).round() as u64)
}

/// `3600` 秒 → `60 分钟`；切割预告里说人话比说秒数好读。
pub fn duration_words(seconds: f64) -> String {
    if !(seconds.is_finite()) || seconds <= 0.0 {
        return "0 秒".to_string();
    }
    if seconds < 60.0 {
        return format!("{} 秒", seconds.round() as u64);
    }
    if seconds < 3600.0 {
        let minutes = seconds / 60.0;
        return format!("{} 分钟", trim(minutes));
    }
    trim(seconds / 3600.0) + " 小时"
}

fn trim(value: f64) -> String {
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{}", rounded as u64)
    } else {
        format!("{rounded:.1}")
    }
}
