//! 切割界面。
//!
//! 视图只持有 `Entity<JobStore>`、两个输入框状态和一个 [`Scheduler`]，其余全部在 `render`
//! 里从 store 投影出来。设置项直接写回 store，界面不自留第二份真相（`value_ok` 除外：
//! 它描述的是输入框当前文本能不能解析成合法的 `SplitMode`，属于纯视图状态）。
//!
//! 界面自己不碰 ffmpeg：加文件只负责 `ingest` 出 id 再交给调度器排队探测，
//! 「开始切割」只是 `Scheduler::start`，进度和状态由调度器写回 store，界面重画一遍就看见了。

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState, SearchableVec};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable, IndexPath, Sizable, StyledExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::components::{
    file_rows, segment_button, settings_row, settings_row_desc, settings_title, FileRow,
    PathsCallback, PickCallback, QueueShell, SelectCallback,
};
use crate::format;
use crate::jobs::{FileEntry, FileId, JobStore, Settings, SplitSettings};
use crate::media::{AudioFormat, FfmpegEnv, Overwrite, ResolveError};
use crate::pick;
use crate::scan::{self, ScanOutcome};
use crate::scheduler::Scheduler;
use crate::split::{
    plan_segments, DimensionValue, SplitMode, DIMENSIONS, DIMENSION_DEFAULTS, DIMENSION_UNITS,
};

/// 维度按钮的展示顺序照设计稿：按文件大小在前，按数量在后。
const DIMENSION_DISPLAY_ORDER: [usize; 3] = [2, 1, 0];

/// 每个维度的数值行标签（按维度下标）。
const VALUE_LABELS: [&str; 3] = ["切割份数", "每段时长", "分片大小"];

const CONCURRENCY_OPTIONS: [u8; 4] = [1, 2, 3, 4];

pub struct SplitView {
    store: Entity<JobStore>,
    /// 切割维度的数值（份数 / 秒 / MB）。维度本身在 store 里，这里只放编辑中的文本载体。
    value: Entity<InputState>,
    value_ok: bool,
    template: Entity<InputState>,
    concurrency_select: Entity<SelectState<SearchableVec<String>>>,
    overwrite_select: Entity<SelectState<SearchableVec<String>>>,
    /// 探测排队、ffmpeg 调度、进度回传、取消，全在这里面。
    /// 视图必须活得比它久：`Scheduler` 一掉，正在跑的那批会被 `RunHandle::drop` 杀掉。
    scheduler: Scheduler,
    /// 授权已到期时禁用"开始"按钮。
    license_expired: bool,
    _subscriptions: Vec<Subscription>,
}

impl SplitView {
    pub fn new(
        window: &mut Window,
        ffmpeg: Result<FfmpegEnv, ResolveError>,
        license_expired: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let defaults = SplitSettings::default();
        let store = cx.new(|_| JobStore::new(Settings::Split(defaults.clone())));
        let scheduler = Scheduler::new(store.clone(), ffmpeg);

        let value = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(DimensionValue::render(defaults.mode.dimension_value()))
                .placeholder("数值")
        });

        let value_changed = cx.subscribe_in(&value, window, |this, state, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                let text = state.read(cx).value().to_string();
                this.apply_value(&text, cx);
            }
        });

        let template = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(&defaults.name_template)
                .placeholder("文件名模板")
        });

        let template_changed = cx.subscribe_in(&template, window, |this, state, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                let text = state.read(cx).value().to_string();
                this.update_settings(cx, |settings| settings.name_template = text);
            }
        });

        let concurrency_items = SearchableVec::new(
            CONCURRENCY_OPTIONS
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>(),
        );
        let concurrency_selected = CONCURRENCY_OPTIONS
            .iter()
            .position(|n| *n == defaults.concurrency)
            .map(|ix| IndexPath::default().row(ix));
        let concurrency_select = cx.new(|cx| {
            SelectState::new(concurrency_items, concurrency_selected, window, cx)
        });

        let concurrency_changed =
            cx.subscribe_in(&concurrency_select, window, |this, _, event, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    if let Ok(n) = value.parse::<u8>() {
                        this.update_settings(cx, |settings| settings.concurrency = n);
                    }
                }
            });

        let overwrite_items = SearchableVec::new(
            Overwrite::OPTIONS
                .iter()
                .map(|o| o.label().to_string())
                .collect::<Vec<_>>(),
        );
        let overwrite_selected = Some(IndexPath::default().row(defaults.overwrite.index()));
        let overwrite_select = cx.new(|cx| {
            SelectState::new(overwrite_items, overwrite_selected, window, cx)
        });

        let overwrite_changed =
            cx.subscribe_in(&overwrite_select, window, |this, _, event, _, cx| {
                if let SelectEvent::Confirm(Some(label)) = event {
                    let overwrite = Overwrite::OPTIONS
                        .iter()
                        .find(|o| o.label() == label.as_str())
                        .copied()
                        .unwrap_or_default();
                    this.update_settings(cx, |settings| settings.overwrite = overwrite);
                }
            });

        Self {
            store,
            value,
            value_ok: true,
            template,
            concurrency_select,
            overwrite_select,
            scheduler,
            license_expired,
            _subscriptions: vec![
                value_changed,
                template_changed,
                concurrency_changed,
                overwrite_changed,
            ],
        }
    }

    pub fn store(&self) -> Entity<JobStore> {
        self.store.clone()
    }

    /// 授权状态变化时由 AppView 调用，更新后触发重画。
    pub fn set_license_expired(&mut self, expired: bool, cx: &mut Context<Self>) {
        self.license_expired = expired;
        cx.notify();
    }

    fn settings(&self, cx: &App) -> SplitSettings {
        self.store
            .read(cx)
            .settings()
            .as_split()
            .cloned()
            .unwrap_or_default()
    }

    /// 写回 `SplitSettings` 的唯一入口：所有开关都走它，改完统一 notify 一次。
    fn update_settings(&self, cx: &mut Context<Self>, apply: impl FnOnce(&mut SplitSettings)) {
        self.store.update(cx, |store, cx| {
            if let Some(settings) = store.settings_mut().as_split_mut() {
                apply(settings);
            }
            cx.notify();
        });
    }

    fn apply_value(&mut self, text: &str, cx: &mut Context<Self>) {
        let index = self.settings(cx).mode.dimension_index();
        let parsed =
            DimensionValue::parse(text).and_then(|value| SplitMode::from_dimension(index, value));

        self.value_ok = parsed.is_some();
        if let Some(mode) = parsed {
            self.update_settings(cx, |settings| settings.mode = mode);
        } else {
            cx.notify();
        }
    }

    fn apply_dimension(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(default) = DIMENSION_DEFAULTS.get(index).copied() else {
            return;
        };
        let Some(mode) = SplitMode::from_dimension(index, default) else {
            return;
        };

        self.update_settings(cx, |settings| settings.mode = mode);
        self.value_ok = true;
        // set_value 不发 InputEvent::Change，所以不会和 apply_value 形成回环。
        self.value.update(cx, |input, cx| {
            input.set_value(DimensionValue::render(default), window, cx)
        });
    }

    fn select(&mut self, id: &FileId, cx: &mut Context<Self>) {
        let next = if self.store.read(cx).selected() == Some(*id) {
            None
        } else {
            Some(*id)
        };
        self.store.update(cx, |store, cx| {
            store.select(next);
            cx.notify();
        });
    }

    fn add_paths(&mut self, outcome: ScanOutcome, cx: &mut Context<Self>) {
        let ids = self.store.update(cx, |store, cx| {
            let ids = store.ingest(outcome);
            cx.notify();
            ids
        });
        // ingest 只把新文件停在「探测中」——没跑过 ffprobe 就没有时长和码率，
        // 按大小/按时长切的数学无从算起。这一步漏了它们就会永远转圈。
        self.scheduler.queue_probe(ids, cx);
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.scheduler.start(cx);
    }

    /// 取消只交给调度器：杀子进程、删半成品、把还没轮到的文件落到「已取消」，
    /// 三件事它都在 worker 里做了，界面再兜一遍只会和事件回传抢同一份状态。
    fn cancel(&mut self) {
        self.scheduler.cancel();
    }

    fn pick_files(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(paths) = pick::audio_files().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.add_paths(scan::collect(paths), cx));
        })
        .detach();
    }

    fn pick_output_dir(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let dir = pick::folder("选择输出文件夹").await;
            let _ = this.update(cx, |this, cx| {
                this.update_settings(cx, |settings| settings.output_dir = dir)
            });
        })
        .detach();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.clear();
            cx.notify();
        });
    }
}

/// 一帧要画的东西先算完再建元素，避免 `render` 里夹带逻辑。
struct Snapshot {
    settings: SplitSettings,
    rows: Vec<FileRow>,
    preview: String,
    has_files: bool,
    /// 队列里有 FLAC 时要改口：它既不能直拷也不能走 segment muxer（实测 ffmpeg 9.0.2），
    /// 「无损直拷」那句承诺对它就是假的。
    has_flac: bool,
    busy: bool,
}

impl Render for SplitView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = {
            let store = self.store.read(cx);
            let settings = store.settings().as_split().cloned().unwrap_or_default();
            let preview = store
                .selected()
                .and_then(|id| store.file(id))
                .map(|file| preview_text(&settings.mode, file))
                .unwrap_or_else(|| "选中一个文件可以看到分段预告".to_string());

            Snapshot {
                rows: file_rows(store.files(), store.selected()),
                has_files: !store.files().is_empty(),
                has_flac: store
                    .files()
                    .iter()
                    .any(|file| AudioFormat::from_path(file.path()) == Some(AudioFormat::Flac)),
                busy: store.is_busy(),
                preview,
                settings,
            }
        };

        let on_select: SelectCallback = {
            let select = cx.listener(|this: &mut Self, id: &FileId, _, cx| this.select(id, cx));
            Rc::new(move |id, window, cx| select(id, window, cx))
        };

        let can_start =
            snapshot.has_files && !snapshot.busy && self.value_ok && !self.license_expired;
        let exact = snapshot.settings.exact;
        let has_files = snapshot.has_files;
        let has_flac = snapshot.has_flac;
        let busy = snapshot.busy;

        let on_pick: PickCallback = {
            let pick = cx.listener(|this: &mut Self, _: &(), _, cx| this.pick_files(cx));
            Rc::new(move |window, cx| pick(&(), window, cx))
        };
        let on_paths: PathsCallback = {
            let add = cx.listener(|this: &mut Self, paths: &Vec<PathBuf>, _, cx| {
                this.add_paths(scan::collect(paths.clone()), cx)
            });
            Rc::new(move |paths, window, cx| add(&paths, window, cx))
        };

        QueueShell::new(snapshot.rows, on_select)
            .on_pick(on_pick)
            .on_paths(on_paths)
            .sidebar(self.render_settings(&snapshot.settings, cx))
            .hint(snapshot.preview)
            .footer(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .child(
                        Button::new("split-start")
                            .primary()
                            .label(if busy { "切割中…" } else { "开始" })
                            .disabled(!can_start)
                            .on_click(cx.listener(|this: &mut Self, _, _, cx| this.start(cx))),
                    )
                    .child(
                        Button::new("split-clear")
                            .label("清空列表")
                            .disabled(!has_files || busy)
                            .on_click(cx.listener(|this: &mut Self, _, _, cx| this.clear(cx))),
                    )
                    .child(
                        Button::new("split-cancel")
                            .label("取消")
                            .disabled(!busy)
                            .on_click(cx.listener(|this: &mut Self, _, _, _| this.cancel())),
                    )
                    .child(div().flex_1())
                    .child(div().text_xs().child(match (exact, has_flac) {
                        (true, _) => "重新编码，切点样本级准确，速度较慢",
                        (false, false) => "无损直拷，切点落在帧边界",
                        // 说的是实话：FLAC 走不了 -c copy，runner 会自己升级成无损重编码。
                        (false, true) => "无损直拷；队列里的 FLAC 不能直拷，会无损重编码",
                    })),
            )
    }
}

impl SplitView {
    fn render_settings(
        &self,
        settings: &SplitSettings,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let dimension = settings.mode.dimension_index();
        let unit = DIMENSION_UNITS.get(dimension).copied().unwrap_or("");
        let value_label = VALUE_LABELS.get(dimension).copied().unwrap_or("数值");
        let selected_position = DIMENSION_DISPLAY_ORDER
            .iter()
            .position(|index| *index == dimension);
        let output_dir = settings
            .output_dir
            .as_deref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| "与源文件同目录".to_string());

        div()
            .v_flex()
            .gap_2()
            .child(settings_title(cx, "切割选项"))
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .children(DIMENSION_DISPLAY_ORDER.iter().enumerate().map(
                        |(position, index)| {
                            let label = DIMENSIONS.get(*index).copied().unwrap_or("");
                            segment_button(
                                format!("split-dim-{position}"),
                                label,
                                Some(position) == selected_position,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| this.apply_dimension(*index, window, cx),
                            ))
                        },
                    )),
            )
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(settings_row(
                        cx,
                        value_label,
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(Input::new(&self.value)))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(unit),
                            ),
                    ))
                    .when(!self.value_ok, |this| {
                        this.child(
                            div()
                                .px_3()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child("请输入一个大于 0 的数值"),
                        )
                    }),
            )
            .child(settings_row(
                cx,
                "精确模式",
                Switch::new("split-exact")
                    .checked(settings.exact)
                    .on_change(cx.listener(|this: &mut Self, exact: &bool, _, cx| {
                        let exact = *exact;
                        this.update_settings(cx, |settings| settings.exact = exact)
                    })),
            ))
            .child(settings_row(
                cx,
                "同时处理",
                Select::new(&self.concurrency_select)
                    .small()
                    .placeholder("选择…"),
            ))
            .child(settings_row(
                cx,
                "文件名模板",
                Input::new(&self.template),
            ))
            .child(settings_row_desc(
                cx,
                "输出位置",
                &output_dir,
                Button::new("split-output-dir")
                    .small()
                    .label("选择…")
                    .on_click(cx.listener(|this: &mut Self, _, _, cx| this.pick_output_dir(cx))),
            ))
            .child(settings_row(
                cx,
                "同名文件",
                Select::new(&self.overwrite_select)
                    .small()
                    .placeholder("选择…"),
            ))
    }
}

fn preview_text(mode: &SplitMode, file: &FileEntry) -> String {
    let Some(info) = file.info() else {
        return "正在读取文件信息".to_string();
    };

    match plan_segments(info.duration_s, info.bitrate_bps, *mode) {
        Ok(segments) => {
            let per = segments
                .first()
                .map(|segment| segment.duration_s)
                .unwrap_or(0.0);
            format!(
                "{} → 切成 {} 段 · 每段约 {}",
                file.file_name(),
                segments.len(),
                format::duration_words(per)
            )
        }
        Err(err) => format!("{} → {err}", file.file_name()),
    }
}
