//! 转换界面。
//!
//! 和切割共用 `QueueShell` 外壳、同一套 `JobStore` 语义和同一个 [`Scheduler`] 接线方式，
//! 只有设置表单不同。

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, StyledExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::components::{
    file_rows, log_rows, settings_block, settings_row, settings_row_desc, settings_title, FileRow,
    LogRow, PathsCallback, PickCallback, QueueShell, SelectCallback,
};
use crate::jobs::{ConvertSettings, FileId, JobStore, LogLevel, Settings};
use crate::media::{AudioFormat, FfmpegEnv, Overwrite, Quality, ResolveError};
use crate::pick;
use crate::scan::{self, ScanOutcome};
use crate::scheduler::Scheduler;

/// 「同时处理」的可选档位，和切割页保持一致。
const CONCURRENCY_OPTIONS: [u8; 4] = [1, 2, 3, 4];

pub struct ConvertView {
    store: Entity<JobStore>,
    /// 视图必须活得比它久：`Scheduler` 一掉，正在跑的那批会被 `RunHandle::drop` 杀掉。
    scheduler: Scheduler,
}

impl ConvertView {
    pub fn new(ffmpeg: Result<FfmpegEnv, ResolveError>, cx: &mut Context<Self>) -> Self {
        let store = cx.new(|_| JobStore::new(Settings::Convert(ConvertSettings::default())));
        let scheduler = Scheduler::new(store.clone(), ffmpeg);
        Self { store, scheduler }
    }

    pub fn store(&self) -> Entity<JobStore> {
        self.store.clone()
    }

    fn update_settings(&self, cx: &mut Context<Self>, apply: impl FnOnce(&mut ConvertSettings)) {
        self.store.update(cx, |store, cx| {
            if let Some(settings) = store.settings_mut().as_convert_mut() {
                apply(settings);
            }
            cx.notify();
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
        // 转换同样要先探测：源格式和时长都来自 ffprobe，plan_convert 靠它决定编码器。
        self.scheduler.queue_probe(ids, cx);
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.scheduler.start(cx);
    }

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
            store.push_log(LogLevel::Info, "已清空列表");
            cx.notify();
        });
    }

    fn clear_log(&mut self, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.clear_log();
            cx.notify();
        });
    }
}

struct Snapshot {
    settings: ConvertSettings,
    rows: Vec<FileRow>,
    log: Vec<LogRow>,
    hint: String,
    has_files: bool,
    /// 有几个文件的输出路径会正好落在自己身上。ffmpeg 遇到这种直接拒绝
    /// （`Output … same as Input #0`），runner 会报 `RunError::SameInputOutput`。
    /// 那是跑完以后一片红；能在点之前说明白就先说明白。
    writes_onto_itself: usize,
    busy: bool,
}

impl Render for ConvertView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = {
            let store = self.store.read(cx);
            let settings = store.settings().as_convert().cloned().unwrap_or_default();

            Snapshot {
                rows: file_rows(store.files(), store.selected()),
                log: log_rows(store.log()),
                has_files: !store.files().is_empty(),
                writes_onto_itself: store
                    .files()
                    .iter()
                    .filter(|file| collides_with_source(file.path(), &settings))
                    .count(),
                busy: store.is_busy(),
                hint: hint_text(&settings),
                settings,
            }
        };

        let on_select: SelectCallback = {
            let select = cx.listener(|this: &mut Self, id: &FileId, _, cx| this.select(id, cx));
            Rc::new(move |id, window, cx| select(id, window, cx))
        };

        let can_start = snapshot.has_files && !snapshot.busy;
        let has_files = snapshot.has_files;
        let busy = snapshot.busy;
        let writes_onto_itself = snapshot.writes_onto_itself;

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

        let on_clear_log: crate::components::ClearLogCallback = {
            let clear = cx.listener(|this: &mut Self, _, _, cx| this.clear_log(cx));
            Rc::new(move |window, cx| clear(&(), window, cx))
        };

        QueueShell::new(snapshot.rows, on_select, snapshot.log)
            .on_pick(on_pick)
            .on_paths(on_paths)
            .on_clear_log(on_clear_log)
            .toolbar(
                div()
                    .h_flex()
                    .gap_2()
                    .w_full()
                    .child(div().flex_1())
                    .child(
                        Button::new("convert-clear")
                            .small()
                            .label("清空")
                            .disabled(!has_files || busy)
                            .on_click(cx.listener(|this: &mut Self, _, _, cx| this.clear(cx))),
                    ),
            )
            .sidebar(self.render_settings(&snapshot.settings, writes_onto_itself, cx))
            .hint(snapshot.hint)
            .footer(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .child(
                        Button::new("convert-start")
                            .primary()
                            .label(if busy { "转换中…" } else { "开始转换" })
                            .disabled(!can_start)
                            .on_click(cx.listener(|this: &mut Self, _, _, cx| this.start(cx))),
                    )
                    .child(
                        Button::new("convert-cancel")
                            .label("取消")
                            .disabled(!busy)
                            .on_click(cx.listener(|this: &mut Self, _, _, _| this.cancel())),
                    )
                    .child(div().flex_1()),
            )
    }
}

impl ConvertView {
    fn render_settings(
        &self,
        settings: &ConvertSettings,
        writes_onto_itself: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let output_dir = settings
            .output_dir
            .as_deref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| "与源文件同目录".to_string());
        // 空串就是「没有冲突」。ffmpeg 碰到输出路径等于输入路径会直接拒绝
        // （`Output … same as Input #0`），runner 报 `RunError::SameInputOutput`；
        // 与其让用户点下去看一片红，不如在这儿先说清楚。
        let collision = match writes_onto_itself {
            0 => String::new(),
            1 => "有 1 个文件的输出路径就是它自己：目标格式和源一样，输出目录也没换。改成别的格式，或者换个输出目录。".to_string(),
            count => format!(
                "有 {count} 个文件的输出路径就是它们自己：目标格式和源一样，输出目录也没换。改成别的格式，或者换个输出目录。"
            ),
        };

        div()
            .v_flex()
            .gap_2()
            .child(settings_title(cx, "转换选项"))
            .child(settings_block(
                cx,
                "目标格式",
                None,
                RadioGroup::vertical("convert-target")
                    .children(AudioFormat::ALL.map(|format| format.label()))
                    .selected_index(Some(settings.target.index()))
                    .on_change(cx.listener(|this: &mut Self, index: &usize, _, cx| {
                        let target = AudioFormat::from_index(*index);
                        this.update_settings(cx, |settings| settings.target = target)
                    })),
            ))
            .child(settings_block(
                cx,
                "质量",
                Some("无损格式（WAV、FLAC）会忽略质量档"),
                RadioGroup::new("convert-quality")
                    .children(Quality::OPTIONS.map(|quality| quality.label()))
                    .selected_index(Some(settings.quality.index()))
                    .on_change(cx.listener(|this: &mut Self, index: &usize, _, cx| {
                        let quality = Quality::from_index(*index);
                        this.update_settings(cx, |settings| settings.quality = quality)
                    })),
            ))
            .child(settings_row_desc(
                cx,
                "同时处理",
                "一次跑几个文件；已在跑的那批不受影响，改动对下一批生效",
                RadioGroup::new("convert-concurrency")
                    .children(CONCURRENCY_OPTIONS.map(|count| count.to_string()))
                    .selected_index(
                        CONCURRENCY_OPTIONS
                            .iter()
                            .position(|count| *count == settings.concurrency),
                    )
                    .on_change(cx.listener(|this: &mut Self, index: &usize, _, cx| {
                        let concurrency = CONCURRENCY_OPTIONS.get(*index).copied().unwrap_or(2);
                        this.update_settings(cx, |settings| settings.concurrency = concurrency)
                    })),
            ))
            .child(settings_row_desc(
                cx,
                "输出位置",
                &output_dir,
                Button::new("convert-output-dir")
                    .small()
                    .label("选择…")
                    .on_click(cx.listener(|this: &mut Self, _, _, cx| this.pick_output_dir(cx))),
            ))
            .when(!collision.is_empty(), |this| {
                // `test_support()` 挂进观察表，无头测试才能按 id 断言这句话出现了没有；
                // 非 test-support 构建下是恒等转发（同 components.rs 的拖放区）。
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(collision)
                        .id("convert-collision-warning")
                        .test_support(),
                )
            })
            .child(settings_row(
                cx,
                "同名文件",
                RadioGroup::new("convert-overwrite")
                    .children(Overwrite::OPTIONS.map(|item| item.label()))
                    .selected_index(Some(settings.overwrite.index()))
                    .on_change(cx.listener(|this: &mut Self, index: &usize, _, cx| {
                        let overwrite = Overwrite::from_index(*index);
                        this.update_settings(cx, |settings| settings.overwrite = overwrite)
                    })),
            ))
            .child(settings_row_desc(
                cx,
                "标签与封面",
                "保留到输出文件",
                Switch::new("convert-keep-tags")
                    .checked(settings.keep_tags_and_cover)
                    .on_change(cx.listener(|this: &mut Self, keep: &bool, _, cx| {
                        let keep = *keep;
                        this.update_settings(cx, |settings| settings.keep_tags_and_cover = keep)
                    })),
            ))
    }
}

fn hint_text(settings: &ConvertSettings) -> String {
    let quality = match settings.target {
        AudioFormat::Wav | AudioFormat::Flac => "无损".to_string(),
        _ => format!("{}质量", settings.quality.label()),
    };
    let dir = settings
        .output_dir
        .as_deref()
        .map(|dir| dir.display().to_string())
        .unwrap_or_else(|| "源文件目录".to_string());
    format!("转为 {} · {quality} · 输出到{dir}", settings.target.label())
}

/// 输出路径会不会正好落在源文件自己身上。
///
/// 和 `runner::plan_convert` 用同一套算法（连 `stem_of` 只认 UTF-8 这一点也一样），
/// 免得界面说没事、点下去一片红，或者反过来界面拦了个其实跑得通的。
fn collides_with_source(path: &Path, settings: &ConvertSettings) -> bool {
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    let dir = settings.output_dir.clone().unwrap_or_else(|| {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    });
    dir.join(format!("{stem}.{}", settings.target.extension())) == path
}

#[cfg(test)]
mod tests {
    // 不写 `use super::*`：父模块那条 `use gpui_kit::*` 会把 gpui 的 `#[test]`（UI 测试宏）
    // 一起带进来，盖掉 std 的 `#[test]`，编译期直接 "recursion limit reached"。
    use super::{collides_with_source, ConvertSettings};
    use crate::media::{AudioFormat, MediaInfo};
    use crate::runner::{plan_convert, RunError};
    use std::path::{Path, PathBuf};

    fn info() -> MediaInfo {
        MediaInfo {
            duration_s: 90.0,
            bitrate_bps: Some(128_000),
            size_bytes: None,
            codec_name: Some("aac".to_string()),
            sample_rate: Some(44_100),
            channels: Some(2),
            has_cover_art: false,
        }
    }

    /// 界面上那句警告和 runner 的判断必须是同一件事。
    ///
    /// 只比"两边一致"还不够——两边一起错也算一致——所以每个用例都写死了期望值，
    /// 再顺带断言 runner 确实这么认为。
    #[test]
    fn the_warning_agrees_with_the_runner() {
        let cases: [(&str, AudioFormat, Option<&str>, bool); 5] = [
            // 目标格式和源一样、又没换目录：输出就是输入自己。
            ("/in/song.m4a", AudioFormat::M4a, None, true),
            ("/in/song.m4a", AudioFormat::Mp3, None, false),
            ("/in/song.m4a", AudioFormat::M4a, Some("/out"), false),
            // 目录换了，但换回了源文件所在的那个目录。
            ("/in/song.m4a", AudioFormat::M4a, Some("/in"), true),
            // 根目录下的文件，parent 是 "/" 而不是 None，一样会撞上。
            ("/song.m4a", AudioFormat::M4a, None, true),
        ];

        for (path, target, output_dir, expected) in cases {
            let path = Path::new(path);
            let settings = ConvertSettings {
                target,
                output_dir: output_dir.map(PathBuf::from),
                ..Default::default()
            };
            let label = format!(
                "{} → {}（输出目录 {:?}）",
                path.display(),
                target.label(),
                output_dir
            );

            assert_eq!(collides_with_source(path, &settings), expected, "{label}");
            assert_eq!(
                matches!(
                    plan_convert(path, &info(), &settings),
                    Err(RunError::SameInputOutput)
                ),
                expected,
                "{label}：runner 的判断和期望不一致"
            );
        }
    }
}
