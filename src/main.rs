// Windows 上 GUI 程序不带控制台时，exe 不该带黑框；但 debug 构建要保留控制台给
// `--ffmpeg-env` 诊断输出用。`cfg_attr` 让 release 包进 GUI 子系统、debug 留在控制台。
// release 包里也要能跑 `--ffmpeg-env`——靠 print_ffmpeg_env 里 AttachConsole 把 stdout
// 接到调用它的 PowerShell 的控制台上，让 `> $log_file` 的重定向照样生效。
#![cfg_attr(
    all(windows, not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::process::ExitCode;

use gpui_kit::*;

#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{
        AttachConsole, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };
    // GUI 子系统进程没有自己的控制台，println! 默认哪儿都不去。
    // AttachConsole(ATTACH_PARENT_PROCESS) 把我们的 stdout/stderr 接到父进程（PowerShell /
    // cmd）的控制台上，这样父进程做的 `> $log` 重定向也会照样抓到我们的输出。
    // 失败一般是「已经有控制台」（debug 构建本来就是 console 子系统），忽略即可。
    let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 };
    if !attached {
        return;
    }
    // AttachConsole 之后，进程已有的 std 句柄还是原来的——对 GUI 子系统进程来说就是
    // 无效句柄，Rust 的 stdout/stderr.write 照样哪儿都不写。必须把 STD_OUTPUT_HANDLE /
    // STD_ERROR_HANDLE 重写到父进程控制台的 CONOUT$ 上。脚本的 `> $log_file` 重定向
    // 在父进程那一侧把 CONOUT$ 接到了文件，所以这里直接写 CONOUT$ 就被父进程刷到文件里。
    reopen_std_handle_for_windows(STD_OUTPUT_HANDLE);
    reopen_std_handle_for_windows(STD_ERROR_HANDLE);
}

#[cfg(windows)]
fn reopen_std_handle_for_windows(which: u32) {
    use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING, FILE_SHARE_WRITE,
    };
    use windows_sys::Win32::System::Console::SetStdHandle;
    // CONOUT$ 是控制台输出设备的保留名。父进程 PowerShell 已经把它的 stdout 重定向到
    // 文件，所以这里 open CONOUT$ 实际上拿到的是那个文件句柄。
    let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            INVALID_HANDLE_VALUE,
        )
    };
    if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
        unsafe {
            SetStdHandle(which, handle);
        }
    }
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
