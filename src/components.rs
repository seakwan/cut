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
use crate::jobs::{FileEntry, FileId, LogLevel, LogLine};
use crate::scan;

/// 一行待处理文件。视图负责把 `FileEntry` 投影成它。
#[derive(Debug, Clone)]
pub struct FileRow {
    id: FileId,
    title: String,
    meta: String,
    status: String,
    is_failed: bool,
    is_selected: bool,
}

/// 把队列投影成可画的行。两个 tab 共用，保证同一份文件在两处显示口径一致。
pub fn file_rows(files: &[FileEntry], selected: Option<FileId>) -> Vec<FileRow> {
    files
        .iter()
        .map(|file| FileRow {
            id: file.id(),
            title: file.file_name().to_string(),
            meta: file
                .info()
                .map(|info| {
                    let mut parts = vec![format::duration(info.duration_s)];
                    if let Some(size) = info.size_bytes {
                        parts.push(format::bytes(size));
                    }
                    if let Some(codec) = &info.codec_name {
                        parts.push(codec.to_uppercase());
                    }
                    parts.join(" · ")
                })
                .unwrap_or_else(|| "尚未探测".to_string()),
            status: file.status().label().to_string(),
            is_failed: file.status().is_failed(),
            is_selected: selected == Some(file.id()),
        })
        .collect()
}

pub fn log_rows(log: &[LogLine]) -> Vec<LogRow> {
    log.iter()
        .map(|line| LogRow {
            text: line.text.clone(),
            is_error: line.level == LogLevel::Error,
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
        let danger = cx.theme().danger;

        div()
            .flex_1()
            .min_h_0()
            .v_flex()
            .gap_1()
            .children(self.rows.into_iter().map(|row| {
                let id = row.id;
                let on_select = self.on_select.clone();
                let accent = accent;
                let muted = muted;
                let danger = danger;

                div()
                    .id(("file-row", id.value()))
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .cursor_pointer()
                    .when(row.is_selected, |this| this.bg(accent))
                    .on_click(move |_, window, cx| on_select(&id, window, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .v_flex()
                            .gap_0p5()
                            .child(div().text_sm().truncate().child(row.title))
                            .child(div().text_xs().text_color(muted).truncate().child(row.meta)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .when(row.is_failed, |this| this.text_color(danger))
                            .when(!row.is_failed, |this| this.text_color(muted))
                            .child(row.status),
                    )
            }))
            // 滚动容器要在所有子元素加完之后再包，`Scrollable` 是整体渲染的。
            .overflow_y_scrollbar()
            .into_any_element()
    }
}

/// 一行日志。
#[derive(Debug, Clone)]
pub struct LogRow {
    text: String,
    is_error: bool,
}

/// 两个 tab 共用的外壳：工具条 / 左侧设置 / 右侧列表 / 底部动作 / 日志。
///
/// 抽出来是为了让切割和转换在结构上必然一致——只有设置表单那一块各自不同。
#[derive(IntoElement)]
pub struct QueueShell {
    toolbar: AnyElement,
    sidebar: AnyElement,
    hint: SharedString,
    footer: AnyElement,
    rows: Vec<FileRow>,
    on_select: SelectCallback,
    on_pick: Option<PickCallback>,
    on_paths: Option<PathsCallback>,
    log: Vec<LogRow>,
    on_clear_log: Option<ClearLogCallback>,
}

impl QueueShell {
    pub fn new(rows: Vec<FileRow>, on_select: SelectCallback, log: Vec<LogRow>) -> Self {
        Self {
            toolbar: div().into_any_element(),
            sidebar: div().into_any_element(),
            hint: SharedString::default(),
            footer: div().into_any_element(),
            rows,
            on_select,
            on_pick: None,
            on_paths: None,
            log,
            on_clear_log: None,
        }
    }

    pub fn toolbar(mut self, toolbar: impl IntoElement) -> Self {
        self.toolbar = toolbar.into_any_element();
        self
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

    pub fn on_clear_log(mut self, on_clear_log: ClearLogCallback) -> Self {
        self.on_clear_log = Some(on_clear_log);
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
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(self.toolbar),
            )
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
            .child({
                let mut pane = LogPane::new(self.log);
                if let Some(on_clear) = self.on_clear_log {
                    pane = pane.on_clear(on_clear);
                }
                pane
            })
    }
}

/// 清理日志的回调。
pub type ClearLogCallback = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct LogPane {
    rows: Vec<LogRow>,
    on_clear: Option<ClearLogCallback>,
}
impl LogPane {
    pub fn new(rows: impl IntoIterator<Item = LogRow>) -> Self {
        Self {
            rows: rows.into_iter().collect(),
            on_clear: None,
        }
    }

    pub fn on_clear(mut self, on_clear: ClearLogCallback) -> Self {
        self.on_clear = Some(on_clear);
        self
    }
}

impl RenderOnce for LogPane {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let border = cx.theme().border;

        let log_content = div()
            .flex_1()
            .min_h_0()
            .px_3()
            .pb_2()
            .text_xs()
            .v_flex()
            .gap_0p5()
            .when(self.rows.is_empty(), |this| {
                this.text_color(muted).child("日志会显示在这里")
            })
            .children(self.rows.into_iter().enumerate().map(|(ix, row)| {
                div()
                    .id(("log-line", ix))
                    .when(row.is_error, |this| this.text_color(danger))
                    .child(row.text)
            }))
            .overflow_y_scrollbar();

        let header = div()
            .h_flex()
            .items_center()
            .justify_end()
            .px_3()
            .pt_2()
            .pb_1()
            .child(
                Button::new("log-clear")
                    .small()
                    .label("清理")
                    .on_click(move |_, window, cx| {
                        if let Some(on_clear) = &self.on_clear {
                            on_clear(window, cx);
                        }
                    }),
            );

        div()
            .h_48()
            .w_full()
            .border_t_1()
            .border_color(border)
            .v_flex()
            .child(header)
            .child(log_content)
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
