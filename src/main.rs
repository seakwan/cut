// Windows 上 GUI 程序不带控制台时，exe 不该带黑框；但 debug 构建要保留控制台给
// `--ffmpeg-env` 诊断输出用。`cfg_attr` 让 release 包进 GUI 子系统、debug 留在控制台。
// release 包里也要能跑 `--ffmpeg-env`——靠 print_ffmpeg_env 里 AttachConsole 把进程接
// 回父进程控制台，让 stdout/stderr 的 inherited pipe（在 PowerShell `> $log` 重定向下
// 指向文件）能继续写到那儿。注意不能 SetStdHandle 把句柄换成 CONOUT\$，那会让父进程
// pipe 失效。
#![cfg_attr(
    all(windows, not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::process::ExitCode;

use gpui_kit::*;

#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // GUI 子系统进程启动时没分配控制台，println! 默认哪儿都不去。
    // AttachConsole(ATTACH_PARENT_PROCESS) 把我们接回父进程（PowerShell / cmd）的控制台
    // ——**attach 成功就算成功**，不要再去 SetStdHandle 替换 std 句柄。父进程用 `>` 重定向
    // 时，子进程 inherit 到的 STD_OUTPUT_HANDLE 是 pipe 句柄；SetStdHandle 把它改写成
    // CONOUT\$ 反而让 pipe 失效，重定向文件收不到东西。
    // 失败一般是「已经有控制台」（debug 构建本身就是 console 子系统），直接 return 即可。
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 };
}

/// 诊断开关：打印解析到的 ffmpeg / ffprobe 与来源，然后退出，不开窗口。
///
/// 打包自检靠它（`script/bundle-macos.sh` / `bundle-windows.ps1` 断言来源是「可执行文件同
/// 目录」——光看进程活着证明不了什么，找不到 ffmpeg 时 app 照样起来，只是顶上挂一条红字）。
/// 用户报障时也只要一句"贴一下 `--ffmpeg-env` 的输出"，不用来回截图那条横幅。
const FFMPEG_ENV_FLAG: &str = "--ffmpeg-env";

fn main() -> ExitCode {
    if std::env::args().any(|arg| arg == FFMPEG_ENV_FLAG) {
        #[cfg(windows)]
        attach_parent_console();
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
