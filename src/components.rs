//! 两个 tab 共用的展示型零件。
//!
//! 无状态：视图每帧把 `JobStore` 投影成普通数据行交给它们画。这样这两个元素不用认识
//! 队列，也不用借引用，第 3 阶段换成 `List` + delegate 时行布局可以直接搬过去。

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size, StyledExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::format;
use crate::jobs::{FileEntry, FileId, FileStatus};
use crate::scan;

/// 状态分类：决定行尾显示哪个图标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatusKind {
    /// 未开始：就绪、已取消。
    Pending,
    /// 进行中：探测中、排队中、切割/转换中。
    InProgress,
    /// 完成。
    Completed,
    /// 失败。
    Failed,
}

impl FileStatusKind {
    fn from_status(status: &FileStatus) -> Self {
        match status {
            FileStatus::Done { .. } => Self::Completed,
            FileStatus::ProbeFailed(_) | FileStatus::Failed(_) => Self::Failed,
            FileStatus::Probing | FileStatus::Queued | FileStatus::Running { .. } => {
                Self::InProgress
            }
            FileStatus::Ready | FileStatus::Cancelled => Self::Pending,
        }
    }
}

/// 一行待处理文件。视图负责把 `FileEntry` 投影成它。
#[derive(Debug, Clone)]
pub struct FileRow {
    id: FileId,
    title: String,
    duration: String,
    size: String,
    status_kind: FileStatusKind,
    status_text: String,
    is_selected: bool,
}

/// 把队列投影成可画的行。两个 tab 共用，保证同一份文件在两处显示口径一致。
pub fn file_rows(files: &[FileEntry], selected: Option<FileId>) -> Vec<FileRow> {
    files
        .iter()
        .map(|file| {
            let info = file.info();
            FileRow {
                id: file.id(),
                title: file.file_name().to_string(),
                duration: info
                    .map(|info| format::duration(info.duration_s))
                    .unwrap_or_else(|| "—".to_string()),
                size: info
                    .and_then(|info| info.size_bytes.map(format::bytes))
                    .unwrap_or_else(|| "—".to_string()),
                status_kind: FileStatusKind::from_status(file.status()),
                status_text: file.status().label().to_string(),
                is_selected: selected == Some(file.id()),
            }
        })
        .collect()
}

/// 选择回调。`FileId` 是稳定标识，不是列表下标。
pub type SelectCallback = Rc<dyn Fn(&FileId, &mut Window, &mut App)>;

/// 点击空态拖放区：由视图决定弹哪种原生选择框。
pub type PickCallback = Rc<dyn Fn(&mut Window, &mut App)>;

/// 从 Finder 拖进来的路径（可能是文件，也可能是文件夹），扫描归一化由视图负责。
pub type PathsCallback = Rc<dyn Fn(Vec<PathBuf>, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct FileList {
    rows: Vec<FileRow>,
    on_select: SelectCallback,
}

impl FileList {
    pub fn new(rows: impl IntoIterator<Item = FileRow>, on_select: SelectCallback) -> Self {
        Self {
            rows: rows.into_iter().collect(),
            on_select,
        }
    }
}

impl RenderOnce for FileList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.rows.is_empty() {
            return div().size_full().into_any_element();
        }

        let accent = cx.theme().accent;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let success = cx.theme().success;
        let danger = cx.theme().danger;

        let header = div()
            .h_flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(border)
            .child(div().flex_1().min_w_0().text_xs().text_color(muted).child("文件名"))
            .child(div().w_16().flex_shrink_0().text_xs().text_color(muted).child("时长"))
            .child(div().w_16().flex_shrink_0().text_xs().text_color(muted).child("大小"))
            .child(div().w_20().flex_shrink_0().text_xs().text_color(muted).child("状态"));

        let rows = self.rows;

        div()
            .flex_1()
            .min_h_0()
            .v_flex()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .v_flex()
                    .gap_0p5()
                    .overflow_y_scrollbar()
                    .children(rows.into_iter().map(|row| {
                        let id = row.id;
                        let on_select = self.on_select.clone();

                        let status_color = match row.status_kind {
                            FileStatusKind::Pending => muted,
                            FileStatusKind::InProgress => cx.theme().accent,
                            FileStatusKind::Completed => success,
                            FileStatusKind::Failed => danger,
                        };

                        div()
                            .id(("file-row", id.value()))
                            .h_flex()
                            .items_center()
                            .gap_3()
                            .px_3()
                            .py_1p5()
                            .rounded_lg()
                            .cursor_pointer()
                            .when(row.is_selected, |this| this.bg(accent))
                            .on_click(move |_, window, cx| on_select(&id, window, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .truncate()
                                    .child(row.title),
                            )
                            .child(
                                div()
                                    .w_16()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child(row.duration),
                            )
                            .child(
                                div()
                                    .w_16()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child(row.size),
                            )
                            .child(
                                div()
                                    .w_20()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(status_color)
                                    .truncate()
                                    .child(row.status_text),
                            )
                    })),
            )
            .into_any_element()
    }
}

/// 两个 tab 共用的外壳：工具条 / 左侧设置 / 右侧列表 / 底部动作。
///
/// 抽出来是为了让切割和转换在结构上必然一致——只有设置表单那一块各自不同。
#[derive(IntoElement)]
pub struct QueueShell {
    sidebar: AnyElement,
    hint: SharedString,
    footer: AnyElement,
    rows: Vec<FileRow>,
    on_select: SelectCallback,
    on_pick: Option<PickCallback>,
    on_paths: Option<PathsCallback>,
}

impl QueueShell {
    pub fn new(rows: Vec<FileRow>, on_select: SelectCallback) -> Self {
        Self {
            sidebar: div().into_any_element(),
            hint: SharedString::default(),
            footer: div().into_any_element(),
            rows,
            on_select,
            on_pick: None,
            on_paths: None,
        }
    }

    pub fn sidebar(mut self, sidebar: impl IntoElement) -> Self {
        self.sidebar = sidebar.into_any_element();
        self
    }

    /// 列表上方那行说明：切割放分段预告，转换放目标格式说明。
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = hint.into();
        self
    }

    /// 空态拖放区的点击行为（一般是弹原生文件选择框）。
    pub fn on_pick(mut self, on_pick: PickCallback) -> Self {
        self.on_pick = Some(on_pick);
        self
    }

    /// 拖入文件/文件夹时的接收口。挂着它，空态和有列表时都能拖。
    pub fn on_paths(mut self, on_paths: PathsCallback) -> Self {
        self.on_paths = Some(on_paths);
        self
    }

    pub fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = footer.into_any_element();
        self
    }
}

impl RenderOnce for QueueShell {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let empty = self.rows.is_empty();

        let mut main = div().v_flex().flex_1().min_w_0();
        if let Some(on_paths) = self.on_paths.clone() {
            main = main
                .on_drop(move |paths: &ExternalPaths, window, cx| {
                    on_paths(paths.paths().to_vec(), window, cx)
                })
                // 拖拽悬停高亮：空态时这条边框就是虚线框自己的边框。
                .drag_over(|style: StyleRefinement, _: &ExternalPaths, _, cx| {
                    style
                        .bg(cx.theme().accent.opacity(0.08))
                        .border_color(cx.theme().accent)
                });
        }

        let main: AnyElement = if empty {
            let accent = theme.accent;
            let muted = theme.muted_foreground;
            let border = theme.border;
            let main = main
                .id("queue-drop-zone")
                .m_3()
                .rounded_xl()
                .border_2()
                .border_dashed()
                .border_color(border)
                .items_center()
                .justify_center()
                .gap_1()
                .child(
                    Icon::new(assets::IconName::CloudUpload)
                        .with_size(Size::Large)
                        .text_color(accent)
                        .mb_2(),
                )
                .child(
                    div()
                        .text_base()
                        .font_medium()
                        .child("点击或拖拽音频文件到此处"),
                )
                .child(div().text_sm().text_color(muted).child(format!(
                    "支持文件夹或单个文件 · 一次最多 {} 个",
                    scan::MAX_FILES
                )));
            let main = if let Some(on_pick) = self.on_pick {
                main.on_click(move |_, window, cx| on_pick(window, cx))
            } else {
                main
            };
            // 挂进观察表，无头测试才能按 id 找到拖放区；非 test-support 构建下是恒等转发。
            main.test_support().into_any_element()
        } else {
            main.p_2()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(self.hint),
                )
                .child(FileList::new(self.rows, self.on_select))
                .into_any_element()
        };

        div()
            .v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div()
                    .h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(
                        div()
                            .w_80()
                            .v_flex()
                            .min_h_0()
                            .p_3()
                            .border_r_1()
                            .border_color(theme.border)
                            .child(self.sidebar)
                            .overflow_y_scrollbar(),
                    )
                    .child(main),
            )
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(self.footer),
            )
    }
}

/// 设置面板标题，对应设计稿里的「切割选项」那一行。
pub fn settings_title(cx: &App, title: &str) -> Div {
    div()
        .text_base()
        .font_semibold()
        .text_color(cx.theme().foreground)
        .pb_1()
        .child(title.to_string())
}

/// 灰底圆角设置行：标签在左、控件在右，对应设计稿里「分片大小 [10] MB」那种行。
pub fn settings_row(cx: &App, label: &str, control: impl IntoElement) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap_2()
        .rounded_lg()
        .bg(cx.theme().muted)
        .px_3()
        .py_2()
        .child(
            div()
                .text_sm()
                .w_20()
                .flex_shrink_0()
                .child(label.to_string()),
        )
        .child(div().flex_1().min_w_0().child(control))
}

/// 带说明的设置行：标签和说明叠在左侧，控件靠右（开关、选择按钮这类）。
pub fn settings_row_desc(
    cx: &App,
    label: &str,
    description: &str,
    control: impl IntoElement,
) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap_2()
        .rounded_lg()
        .bg(cx.theme().muted)
        .px_3()
        .py_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .v_flex()
                .gap_0p5()
                .child(div().text_sm().child(label.to_string()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .whitespace_normal()
                        .child(description.to_string()),
                ),
        )
        .child(control)
}

/// 竖排设置块：选项多的单选组（目标格式）放不进一行，用整块灰底。
pub fn settings_block(
    cx: &App,
    label: &str,
    description: Option<&str>,
    control: impl IntoElement,
) -> Div {
    div()
        .v_flex()
        .gap_1()
        .rounded_lg()
        .bg(cx.theme().muted)
        .px_3()
        .py_2()
        .child(div().text_sm().child(label.to_string()))
        .when_some(description, |this, description| {
            this.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(description.to_string()),
            )
        })
        .child(control)
}

/// 分段选择按钮：选中态实心主色，未选中描边，对应设计稿顶排三个按钮。
pub fn segment_button(id: impl Into<ElementId>, label: &str, selected: bool) -> Button {
    Button::new(id)
        .label(label.to_string())
        .flex_1()
        .when(selected, |this| this.primary())
}
