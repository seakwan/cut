//! 切割能力：分段数学 + 切割界面。

mod plan;
mod split_view;

pub use plan::{
    plan_segments, render_name, render_pattern, segment_seconds, DimensionValue, PlanError,
    Segment, SplitMode, DIMENSIONS, DIMENSION_DEFAULTS, DIMENSION_UNITS, MIN_SEGMENT_SECONDS,
};
pub use split_view::SplitView;
