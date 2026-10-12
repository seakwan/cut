//! 切割页的无头端到端：拖文件进去 → 自动探测 → 点「开始切割」→ 磁盘上真有分段。
//!
//! `tests/scheduler.rs` 验的是调度器本身（没有窗口），这里验的是**接线**：
//! 视图有没有把 `ingest` 返回的 id 交给 `queue_probe`、按钮有没有真的调 `start`、
//! 跑完以后界面有没有从 store 重新投影出「就绪/取消」的可用状态。
//! 少接任何一根线，这条就红——而且是超时或者"什么都没发生"，不会悄悄通过。

mod common;

use std::path::PathBuf;

use common::{env_or_skip, files_with_extension, sine, total_duration, TempDir, FIXTURE_SECONDS};
use cut::jobs::{FileStatus, JobStore};
use cut::media::{AudioFormat, FfmpegEnv};
use cut::split::{SplitMode, SplitView};
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

/// 从拖文件一直走到磁盘上出现分段，全程只用界面上的东西。
#[gpui_kit::test]
fn dropping_a_file_and_clicking_start_produces_segments(cx: &mut TestAppContext) {
    let Some(env) = env_or_skip() else { return };
    cx.update(init);

    let dir = TempDir::new("ui-split");
    let out = dir.mkdir("out");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);

    let window = cx.add_window(|window, cx| SplitView::new(window, FfmpegEnv::resolve(), cx));
    let store = window.update(cx, |view, _, _| view.store()).unwrap();
    // 派发事件会回调视图，不能再借着视图的 update 做，否则实体重入。
    let window: AnyWindowHandle = window.into();

    // 设置从 store 改（界面写的也是这里），9 秒按 3 秒切 → 3 段。
    cx.update(|cx| {
        store.update(cx, |store, cx| {
            let settings = store.settings_mut().as_split_mut().unwrap();
            settings.output_dir = Some(out.clone());
            settings.mode = SplitMode::ByDuration(3.0);
            cx.notify();
        })
    });

    drop_paths(window, cx, std::slice::from_ref(&source));

    // 探测是自动排队的：`add_paths` 漏了 `queue_probe`，这里就会一直停在「探测中」直到超时。
    wait_for(cx, &store, "探测结束", |store| {
        store
            .files()
            .iter()
            .all(|file| matches!(file.status(), FileStatus::Ready))
    });

    let duration = cx.read(|cx| {
        store
            .read(cx)
            .files()
            .first()
            .and_then(|file| file.info())
            .map(|info| info.duration_s)
    });
    let duration = duration.expect("探测完就该有时长");
    assert!(
        (duration - f64::from(FIXTURE_SECONDS)).abs() < 0.5,
        "界面探测到的时长 {duration} 和源文件 {FIXTURE_SECONDS} 对不上"
    );

    // 「开始切割」得真能点出结果来。Button 不注册 disabled 标志（`disabled()` 只会是
    // None），所以按 test.md 的说法用交互本身来证明它可点，而不是断言一个拿不到的属性。
    window
        .update(cx, |_view, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("split-start").label(),
                Some("开始切割"),
                "没在跑的时候按钮该写着「开始切割」"
            );
            window.click("split-start", cx);
        })
        .unwrap();

    wait_for(cx, &store, "跑完", |store| !store.is_busy());

    let status = cx.read(|cx| store.read(cx).files().first().unwrap().status().clone());
    let FileStatus::Done { output_count } = status else {
        panic!("点完「开始切割」应该是「完成」，实际是 {status:?}");
    };

    let parts = files_with_extension(&out, "m4a");
    assert_eq!(
        parts.len(),
        output_count,
        "界面说有 {output_count} 个，磁盘上是 {:?}",
        parts
            .iter()
            .map(|p| p.file_name().unwrap())
            .collect::<Vec<_>>()
    );
    assert_eq!(parts.len(), 3, "9 秒按 3 秒切就是 3 段");

    let total = total_duration(&env, &parts);
    assert!(
        (total - duration).abs() < 0.6,
        "各段时长求和 {total} 对不上源文件 {duration}"
    );

    // 跑完界面要回到可再次开始的状态，不能卡在「切割中…」；
    // 空手点一下「取消」也不该把已经完成的队列改乱。
    window
        .update(cx, |_view, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("split-start").label(), Some("开始切割"));
            window.click("split-cancel", cx);
            window.render_frame(cx);
            assert_eq!(window.find("split-start").label(), Some("开始切割"));
        })
        .unwrap();

    assert!(
        cx.read(|cx| matches!(
            store.read(cx).files().first().unwrap().status(),
            FileStatus::Done { .. }
        )),
        "空点「取消」以后完成状态还得留着：{:?}",
        cx.read(|cx| statuses(store.read(cx)))
    );
}
