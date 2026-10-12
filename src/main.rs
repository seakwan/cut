// Windows 上 GUI 程序不带控制台时，exe 不该带黑框；但 debug 构建要保留控制台给
// `--ffmpeg-env` 诊断输出用。`cfg_attr` 让 release 包进 GUI 子系统、debug 留在控制台。
// 这个取舍得在真 Windows 机上验证——如果 `--ffmpeg-env` 在 release 包里也需要，
// 就得改走日志文件或 `AttachConsole`，见 build.md 的 Windows 章节。
#![cfg_attr(
    all(windows, not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::process::ExitCode;

use gpui_kit::*;

/// 诊断开关：打印解析到的 ffmpeg / ffprobe 与来源，然后退出，不开窗口。
///
/// 打包自检靠它（`script/bundle-macos.sh` 断言来源是「应用包内」——光看进程活着
/// 证明不了什么，找不到 ffmpeg 时 app 照样起来，只是顶上挂一条红字）。
/// 用户报障时也只要一句"贴一下 `--ffmpeg-env` 的输出"，不用来回截图那条横幅。
const FFMPEG_ENV_FLAG: &str = "--ffmpeg-env";

fn main() -> ExitCode {
    if std::env::args().any(|arg| arg == FFMPEG_ENV_FLAG) {
        return print_ffmpeg_env();
    }

    application()
        .with_assets(assets::Assets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx| {
            // 必须在创建任何组件视图之前初始化，`open_window` 会自己套上 `Root`。
            init(cx);

            let bounds = WindowBounds::centered(size(px(900.), px(600.)), cx);
            let _ = open_window(
                WindowOptions {
                    window_bounds: Some(bounds),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    cx.new(|cx| cut::app::AppView::new(window, cx))
                },
            )
            .expect("打不开窗口");
        });

    ExitCode::SUCCESS
}

fn print_ffmpeg_env() -> ExitCode {
    match cut::media::FfmpegEnv::resolve() {
        Ok(env) => {
            println!("来源：{}", env.source().label());
            println!("ffmpeg：{}", env.ffmpeg().display());
            println!("ffprobe：{}", env.ffprobe().display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            // ResolveError 的 Display 已经把查过的地方列全了，直接给用户看。
            print!("{err}");
            ExitCode::FAILURE
        }
    }
}
