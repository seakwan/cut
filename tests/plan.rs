//! 切割数学的边界情况。这些是用户能直接看见的结果（切几段、每段多长），错了就是坏文件。

use cut::split::{
    plan_segments, render_name, segment_seconds, DimensionValue, PlanError, SplitMode, DIMENSIONS,
};

/// 浮点比较统一走这里，避免每处都写 epsilon。
fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-6
}

#[test]
fn by_count_splits_evenly_and_last_absorbs_remainder() {
    // 100 秒切 3 份：前两段 33.333，最后一段吃掉余数，总长必须还原。
    let segments = plan_segments(100.0, None, SplitMode::ByCount(3)).unwrap();
    assert_eq!(segments.len(), 3);
    assert!(close(segments[0].duration_s, 33.333));
    assert!(close(segments[2].duration_s, 33.334), "最后一段吸收余数");
    let total: f64 = segments.iter().map(|s| s.duration_s).sum();
    assert!(close(total, 100.0), "总长 {total} 应该等于原时长");
    assert!(close(segments[2].start_s + segments[2].duration_s, 100.0));
}

#[test]
fn by_duration_covers_whole_file() {
    let segments = plan_segments(95.0, None, SplitMode::ByDuration(30.0)).unwrap();
    assert_eq!(segments.len(), 4, "95 秒按 30 秒切应该是 30/30/30/5");
    assert!(close(segments[3].duration_s, 5.0));
    assert!(close(segments[3].start_s, 90.0));
}

#[test]
fn file_shorter_than_one_segment_yields_single_segment() {
    // 不是错误：整个文件就是一段，界面按提示处理。
    let segments = plan_segments(12.0, None, SplitMode::ByDuration(30.0)).unwrap();
    assert_eq!(segments.len(), 1);
    assert!(close(segments[0].duration_s, 12.0));
    assert!(close(segments[0].start_s, 0.0));
}

#[test]
fn by_size_uses_bitrate_and_floors_to_frame_safe_value() {
    // 128 kbps 下 1 MB ≈ 62.5 秒。
    let seconds = segment_seconds(600.0, Some(128_000), SplitMode::BySize(1_000_000)).unwrap();
    assert!(close(seconds, 62.5), "算出来是 {seconds}");
}

#[test]
fn by_size_without_bitrate_is_an_explicit_error() {
    let err = plan_segments(600.0, None, SplitMode::BySize(1_000_000)).unwrap_err();
    assert_eq!(err, PlanError::UnknownBitrate);
    // 错误文案要能直接显示给用户，并且给出出路。
    assert!(err.to_string().contains("按时长"));
}

#[test]
fn rejects_nonsense_inputs() {
    assert_eq!(
        plan_segments(0.0, None, SplitMode::ByCount(2)).unwrap_err(),
        PlanError::NonPositiveDuration
    );
    assert_eq!(
        plan_segments(100.0, None, SplitMode::ByCount(0)).unwrap_err(),
        PlanError::InvalidCount
    );
    assert_eq!(
        plan_segments(100.0, None, SplitMode::ByDuration(0.0)).unwrap_err(),
        PlanError::NonPositiveTarget
    );
    assert_eq!(
        plan_segments(100.0, None, SplitMode::BySize(0)).unwrap_err(),
        PlanError::NonPositiveTarget
    );
    // 1 秒的文件切 100 份 → 每段 0.01 秒，比一个音频帧还短。
    assert_eq!(
        plan_segments(1.0, None, SplitMode::ByCount(100)).unwrap_err(),
        PlanError::SegmentTooShort
    );
}

#[test]
fn nan_duration_is_rejected_not_propagated() {
    assert_eq!(
        plan_segments(f64::NAN, None, SplitMode::ByDuration(30.0)).unwrap_err(),
        PlanError::NonPositiveDuration
    );
}

#[test]
fn name_template_renders_one_based_index() {
    assert_eq!(render_name("{name}_part{index}", "song", 0), "song_part1");
    assert_eq!(render_name("{name}_part{index}", "song", 9), "song_part10");
    assert_eq!(render_name("{index}-{name}", "a b", 1), "2-a b");
    // 没有占位符时原样返回，不能 panic 也不能吞掉模板。
    assert_eq!(render_name("fixed", "song", 0), "fixed");
}

#[test]
fn dimension_round_trip_keeps_user_intent() {
    for index in 0..DIMENSIONS.len() {
        let mode = SplitMode::from_dimension(index, 7.0).unwrap();
        assert_eq!(mode.dimension_index(), index);
        assert!(close(mode.dimension_value(), 7.0));
    }
}

#[test]
fn by_count_rejects_fractional_count() {
    // 份数必须是整数，"2.5 份" 不该被静默取整成 2。
    assert!(SplitMode::from_dimension(0, 2.5).is_none());
    assert!(SplitMode::from_dimension(0, 0.0).is_none());
    assert!(SplitMode::from_dimension(0, -3.0).is_none());
    assert!(SplitMode::from_dimension(9, 3.0).is_none());
}

#[test]
fn by_size_round_trips_through_mb() {
    let mode = SplitMode::from_dimension(2, 1.5).unwrap();
    assert_eq!(mode, SplitMode::BySize(1_500_000));
    assert!(close(mode.dimension_value(), 1.5));
}

#[test]
fn value_parsing_tolerates_typed_input() {
    assert_eq!(DimensionValue::parse("300"), Some(300.0));
    assert_eq!(DimensionValue::parse(" 1 000 "), Some(1000.0));
    assert_eq!(DimensionValue::parse("1,000"), Some(1000.0));
    assert_eq!(DimensionValue::parse("2.5"), Some(2.5));
    assert_eq!(DimensionValue::parse(""), None);
    assert_eq!(DimensionValue::parse("abc"), None);
    assert!(DimensionValue::parse("inf").is_none(), "inf 不是可用的段长");
}

#[test]
fn value_rendering_hides_pointless_decimals() {
    assert_eq!(DimensionValue::render(300.0), "300");
    assert_eq!(DimensionValue::render(2.5), "2.5");
    assert_eq!(DimensionValue::render(0.125), "0.125");
}
