//! 进度解析。ffmpeg 的 `-progress` 输出有几个坑，都在这里钉住。

use cut::media::{parse_progress_line, percent_of, ProgressField};

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-6
}

#[test]
fn parses_clock_format() {
    match parse_progress_line("out_time=00:01:02.500000").unwrap() {
        ProgressField::OutTime(seconds) => assert!(close(seconds, 62.5), "得到 {seconds}"),
        other => panic!("期望 OutTime，得到 {other:?}"),
    }
}

#[test]
fn out_time_ms_actually_carries_microseconds() {
    // ffmpeg 的历史包袱：out_time_ms 与 out_time_us 同义，都是微秒。
    // 当成毫秒会让进度条快 1000 倍，直接冲过 100%。
    for key in ["out_time_us", "out_time_ms"] {
        match parse_progress_line(&format!("{key}=1500000")).unwrap() {
            ProgressField::OutTime(seconds) => assert!(close(seconds, 1.5), "{key} 得到 {seconds}"),
            other => panic!("{key} 期望 OutTime，得到 {other:?}"),
        }
    }
}

#[test]
fn some_builds_emit_bare_seconds_for_out_time() {
    match parse_progress_line("out_time=42.0").unwrap() {
        ProgressField::OutTime(seconds) => assert!(close(seconds, 42.0)),
        other => panic!("期望 OutTime，得到 {other:?}"),
    }
}

#[test]
fn naa_out_time_is_ignored_not_zero() {
    // 开头几帧 ffmpeg 会给 N/A；当成 0 会让进度条反复跳回起点。
    assert!(parse_progress_line("out_time=N/A").is_none());
}

#[test]
fn speed_drops_the_x_suffix() {
    match parse_progress_line("speed=2.35x").unwrap() {
        ProgressField::Speed(value) => assert!(close(value, 2.35)),
        other => panic!("期望 Speed，得到 {other:?}"),
    }
}

#[test]
fn recognises_end_and_ignores_noise() {
    assert_eq!(
        parse_progress_line("progress=end"),
        Some(ProgressField::End)
    );
    assert!(parse_progress_line("progress=continue").is_none());
    assert!(parse_progress_line("frame=123").is_none());
    assert!(parse_progress_line("bitrate=128.0kbits/s").is_none());
    assert!(parse_progress_line("").is_none());
    assert!(parse_progress_line("no-equals-sign").is_none());
}

#[test]
fn percent_is_clamped_and_survives_unknown_total() {
    assert_eq!(percent_of(30.0, 60.0), 50.0);
    assert_eq!(percent_of(0.0, 60.0), 0.0);
    // segment muxer 的 out_time 可能超过单文件时长，不能给出 137%。
    assert_eq!(percent_of(90.0, 60.0), 100.0);
    assert_eq!(percent_of(10.0, 0.0), 0.0);
    assert_eq!(percent_of(10.0, -1.0), 0.0);
    assert_eq!(percent_of(f64::NAN, 60.0), 0.0);
}
