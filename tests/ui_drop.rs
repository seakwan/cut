//! 空态拖放区的无头验收：从「Finder」拖一个文件夹加一个单文件进去，队列要全收。
//!
//! 真拖放走的是平台事件，这里用 `Window::dispatch_event` 原样重放
//! `FileDropEvent::Entered → Submit`，和 window.rs 里外部拖拽的翻译路径一致。

use std::fs;
use std::path::PathBuf;

use cut::media::FfmpegEnv;
use cut::split::SplitView;
// 不能 `use gpui_kit::*`：glob 会把 `test` 模块名带进作用域，遮住宏展开出的 `#[test]`。
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    init, point, AnyWindowHandle, ExternalPaths, FileDropEvent, PlatformInput, TestAppContext,
};

fn fixture_root() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cut-ui-drop-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("album")).unwrap();
    fs::write(dir.join("album").join("a.mp3"), b"fake").unwrap();
    fs::write(dir.join("album").join("b.mp3"), b"fake").unwrap();
    fs::write(dir.join("loose.mp3"), b"fake").unwrap();
    fs::write(dir.join("notes.txt"), b"not audio").unwrap();
    dir
}

#[gpui_kit::test]
fn drop_zone_takes_folders_and_single_files(cx: &mut TestAppContext) {
    cx.update(init);
    let dir = fixture_root();
    let window =
        cx.add_window(|window, cx| SplitView::new(window, FfmpegEnv::resolve(), false, cx));
    let store = window.update(cx, |view, _, _| view.store()).unwrap();
    // 派发事件会回调视图，不能再借着视图的 update 做，否则实体重入。
    let window: AnyWindowHandle = window.into();

    window
        .update(cx, |_view, window, cx| {
            window.render_frame(cx);

            let zone = window.find("queue-drop-zone");
            assert!(zone.visible(), "空态必须画出拖放区");
            let bounds = zone.bounds();
            let center = point(
                bounds.origin.x + bounds.size.width / 2.0,
                bounds.origin.y + bounds.size.height / 2.0,
            );

            let dragged = vec![dir.join("album"), dir.join("loose.mp3")];
            window.dispatch_event(
                PlatformInput::FileDrop(FileDropEvent::Entered {
                    position: center,
                    paths: ExternalPaths(dragged.iter().cloned().collect()),
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::FileDrop(FileDropEvent::Submit { position: center }),
                cx,
            );
            window.render_frame(cx);

            assert!(
                window.try_find("queue-drop-zone").is_none(),
                "有文件后拖放区让位给列表"
            );
        })
        .unwrap();

    // 文件夹递归展开 + 单文件原样收，非音频（notes.txt）被过滤。
    let files = cx.update(|cx| store.read(cx).files().len());
    assert_eq!(files, 3, "文件夹 2 个 + 单文件 1 个，txt 不收");

    let _ = fs::remove_dir_all(&dir);
}
