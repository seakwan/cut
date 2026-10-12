//! 转换页的无头端到端：拖文件进去 → 自动探测 → 点「开始转换」→ 磁盘上真有 mp3。
//!
//! 和 `tests/ui_split.rs` 是一对：那边验切割的接线，这边验转换的接线。
//! 转换页在 Phase 4 才接上 `Scheduler`，而 `ingest` 现在会把新文件停在「探测中」——
//! 少接 `queue_probe` 这一根线，转换页就会永远转圈，这条测试会超时红掉，不会悄悄通过。
//!
//! 第二条测的是「输出路径正好是源文件自己」的前置警告：runner 早就会报
//! `SameInputOutput`，但那是跑完以后一片红；界面得在点之前就说清楚，
//! 而且这句话必须跟着设置实时出现/消失。

mod common;

use std::path::PathBuf;

use common::{env_or_skip, files_with_extension, info, sine, TempDir, FIXTURE_SECONDS};
use cut::convert::ConvertView;
use cut::jobs::{FileStatus, JobStore};
use cut::media::{AudioFormat, FfmpegEnv};
// 不能 `use gpui_kit::*`：glob 会把 `test` 模块名带进作用域，遮住宏展开出的 `#[test]`。
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    init, point, AnyWindowHandle, Entity, ExternalPaths, FileDropEvent, PlatformInput,
    TestAppContext,
};

/// 反复泵执行器直到条件成立。后台任务不泵就不动，所以不能只 park 一次。
fn wait_for(
    cx: &mut TestAppContext,
    store: &Entity<JobStore>,
    what: &str,
    ready: impl Fn(&JobStore) -> bool,
) {
    let started = std::time::Instant::now();
    loop {
        cx.run_until_parked();
        if cx.read(|cx| ready(store.read(cx))) {
            return;
        }
        assert!(
            started.elapsed().as_secs() < 120,
            "等「{what}」超时了，当前状态：{:?}",
            cx.read(|cx| statuses(store.read(cx)))
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn statuses(store: &JobStore) -> Vec<String> {
    store
        .files()
        .iter()
        .map(|file| format!("{}={:?}", file.file_name(), file.status()))
        .collect()
}

/// 模拟从 Finder 拖一个文件进来。真拖放走平台事件，这里原样重放
/// `Entered → Submit`，和 window.rs 里外部拖拽的翻译路径一致。
fn drop_paths(window: AnyWindowHandle, cx: &mut TestAppContext, paths: &[PathBuf]) {
    window
        .update(cx, |_view, window, cx| {
            window.render_frame(cx);
            let bounds = window.find("queue-drop-zone").bounds();
            let center = point(
                bounds.origin.x + bounds.size.width / 2.0,
                bounds.origin.y + bounds.size.height / 2.0,
            );
            window.dispatch_event(
                PlatformInput::FileDrop(FileDropEvent::Entered {
                    position: center,
                    paths: ExternalPaths(paths.iter().cloned().collect()),
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::FileDrop(FileDropEvent::Submit { position: center }),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
}

/// 改转换设置。界面写的也是这里，所以从这里改和点控件是一回事。
fn set_convert(
    cx: &mut TestAppContext,
    store: &Entity<JobStore>,
    apply: impl FnOnce(&mut cut::jobs::ConvertSettings),
) {
    cx.update(|cx| {
        store.update(cx, |store, cx| {
            apply(store.settings_mut().as_convert_mut().unwrap());
            cx.notify();
        })
    });
}

#[gpui_kit::test]
fn dropping_an_m4a_and_clicking_start_produces_an_mp3(cx: &mut TestAppContext) {
    let Some(env) = env_or_skip() else { return };
    cx.update(init);

    let dir = TempDir::new("ui-convert");
    let out = dir.mkdir("out");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);

    let window = cx.add_window(|window, cx| ConvertView::new(window, FfmpegEnv::resolve(), cx));
    let store = window.update(cx, |view, _, _| view.store()).unwrap();
    // 派发事件会回调视图，不能再借着视图的 update 做，否则实体重入。
    let window: AnyWindowHandle = window.into();

    set_convert(cx, &store, |settings| {
        settings.target = AudioFormat::Mp3;
        settings.output_dir = Some(out.clone());
    });

    drop_paths(window, cx, std::slice::from_ref(&source));

    // 漏了 `queue_probe` 就会一直停在「探测中」，直到超时。
    wait_for(cx, &store, "探测结束", |store| {
        store
            .files()
            .iter()
            .all(|file| matches!(file.status(), FileStatus::Ready))
    });

    window
        .update(cx, |_view, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("convert-start").label(),
                Some("开始转换"),
                "没在跑的时候按钮该写着「开始转换」"
            );
            window.click("convert-start", cx);
        })
        .unwrap();

    wait_for(cx, &store, "跑完", |store| !store.is_busy());

    let status = cx.read(|cx| store.read(cx).files().first().unwrap().status().clone());
    let FileStatus::Done { output_count } = status else {
        panic!("点完「开始转换」应该是「完成」，实际是 {status:?}");
    };
    assert_eq!(output_count, 1, "一进一出，产出该是一个");

    let produced = files_with_extension(&out, "mp3");
    assert_eq!(
        produced.len(),
        1,
        "输出目录里该有一个 mp3，实际是 {:?}",
        produced
            .iter()
            .map(|p| p.file_name().unwrap())
            .collect::<Vec<_>>()
    );

    let before = info(&env, &source);
    let after = info(&env, &produced[0]);
    assert_eq!(after.codec_name.as_deref(), Some("mp3"), "该真转成 mp3 了");
    assert!(
        (after.duration_s - f64::from(FIXTURE_SECONDS)).abs() < 0.5,
        "产出时长 {} 和源文件 {FIXTURE_SECONDS} 对不上（源探测到 {}）",
        after.duration_s,
        before.duration_s
    );

    // 跑完界面要回到可再次开始的状态，不能卡在「转换中…」。
    window
        .update(cx, |_view, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("convert-start").label(), Some("开始转换"));
        })
        .unwrap();
}

/// 「输出会盖掉源文件」这句警告得跟着设置走：撞上才出现，改对了就消失。
#[gpui_kit::test]
fn the_collision_warning_follows_the_settings(cx: &mut TestAppContext) {
    let Some(env) = env_or_skip() else { return };
    cx.update(init);

    let dir = TempDir::new("ui-convert-collision");
    let out = dir.mkdir("out");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);

    let window = cx.add_window(|window, cx| ConvertView::new(window, FfmpegEnv::resolve(), cx));
    let store = window.update(cx, |view, _, _| view.store()).unwrap();
    let window: AnyWindowHandle = window.into();

    // 默认目标不是 m4a，先确认没有警告，免得把"一直显示"当成"跟着设置走"。
    set_convert(cx, &store, |settings| {
        settings.target = AudioFormat::Mp3;
        settings.output_dir = None;
    });
    drop_paths(window, cx, std::slice::from_ref(&source));
    wait_for(cx, &store, "探测结束", |store| {
        store
            .files()
            .iter()
            .all(|file| matches!(file.status(), FileStatus::Ready))
    });

    let has_warning = |cx: &mut TestAppContext| -> bool {
        window
            .update(cx, |_view, window, cx| {
                window.render_frame(cx);
                window.try_find("convert-collision-warning").is_some()
            })
            .unwrap()
    };

    assert!(!has_warning(cx), "m4a → mp3 不该有冲突警告");

    // 目标格式选成和源一样，又没换输出目录：输出路径就是源文件自己。
    // 只断言这句话在不在——div 的无障碍 label 不等于它渲染出来的文字，
    // 文案由 `convert_view::tests` 那条单元测试和渲染代码自己管。
    set_convert(cx, &store, |settings| settings.target = AudioFormat::M4a);
    assert!(has_warning(cx), "m4a → m4a 且同目录，就该警告了");

    // 换个输出目录，冲突就没了。
    set_convert(cx, &store, |settings| {
        settings.output_dir = Some(out.clone())
    });
    assert!(!has_warning(cx), "换了输出目录就不该再警告");

    // 换回来：警告得跟着回来。
    set_convert(cx, &store, |settings| settings.output_dir = None);
    assert!(
        has_warning(cx),
        "目录改回「与源文件同目录」，警告该重新出现"
    );
}
