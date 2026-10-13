// Windows 上 GUI 程序不带控制台时，exe 不该带黑框；但 debug 构建要保留控制台给
// `--ffmpeg-env` 诊断输出用。`cfg_attr` 让 release 包进 GUI 子系统、debug 留在控制台。
// release 包里也要能跑 `--ffmpeg-env`——直接 WriteFile 到 inherited STD_OUTPUT_HANDLE。
// GUI 子系统进程没分配控制台，Rust std 检测到 stdout 句柄是 NULL/invalid 时把 stdout
// 标成 broken，println! 写什么都不出——父进程 PowerShell `>` 重定向创建的 inherited
// pipe 句柄够用，但绕开 std::io::stdout() 这层最稳；PrintStream 都进不去，WriteFile 进去。
#![cfg_attr(
    all(windows, not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::process::ExitCode;

use gpui_kit::*;

/// 诊断开关：打印解析到的 ffmpeg / ffprobe 与来源，然后退出，不开窗口。
///
/// 打包自检靠它（`script/bundle-macos.sh` / `bundle-windows.ps1` 断言来源是「可执行文件同
/// 目录」——光看进程活着证明不了什么，找不到 ffmpeg 时 app 照样起来，只是顶上挂一条红字）。
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
    let text = match cut::media::FfmpegEnv::resolve() {
        Ok(env) => format!(
            "来源：{}\nffmpeg：{}\nffprobe：{}\n",
            env.source().label(),
            env.ffmpeg().display(),
            env.ffprobe().display()
        ),
        Err(err) => {
            // ResolveError 的 Display 已经把查过的地方列全了，直接给用户看。
            return write_output(&format!("{err}"));
        }
    };
    write_output(&text)
}

fn write_output(text: &str) -> ExitCode {
    // 打包自检走文件路径，不依赖 stdout：release 包是 GUI 子系统进程，PowerShell
    // `> $log_file` 的 inherited pipe 句柄 WriteFile 写不进去（CUT_FFMPEG_ENV_OUT
    // 是给 bundle 脚本传明确目标位置的约定环境变量）。用户从命令行跑
    // `cut.exe --ffmpeg-env` 看 stdout 时仍走 WriteFile 那条路。
    if let Ok(path) = std::env::var("CUT_FFMPEG_ENV_OUT") {
        return write_to_path(&path, text);
    }
    // 顺手再写一份 %TEMP% 备份，方便脚本/用户从文件系统抓结果。
    let _ = std::fs::write(
        std::env::temp_dir().join("cut-ffmpeg-env-out.log"),
        text,
    );
    #[cfg(windows)]
    {
        write_to_inherited_stdout(text);
    }
    #[cfg(not(windows))]
    {
        print!("{text}");
    }
    ExitCode::SUCCESS
}

fn write_to_path(path: &str, text: &str) -> ExitCode {
    if let Err(err) = std::fs::write(path, text) {
        let _ = std::fs::write(
            std::env::temp_dir().join("cut-ffmpeg-env-out.log"),
            format!("CUT_FFMPEG_ENV_OUT={path} 写失败：{err}\n{text}"),
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(windows)]
fn write_to_inherited_stdout(text: &str) {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, WriteFile, FILE_TYPE_UNKNOWN};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        // 诊断：handle 真值 / GetFileType / WriteFile 返回 / LastError 都写到
        // %TEMP%\cut-ffmpeg-env-debug.log。这条路服务"用户从命令行跑 --ffmpeg-env
        // 看 stdout"的场景，build 自检走 CUT_FFMPEG_ENV_OUT，不依赖它。
        if let Some(h) = handle_checked(handle) {
            let file_type = GetFileType(h);
            let bytes = text.as_bytes();
            let mut written = 0u32;
            let ok = WriteFile(
                h,
                bytes.as_ptr(),
                bytes.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            );
            let last_err = if ok == 0 { GetLastError() } else { 0 };
            let dbg = format!(
                "handle={:p} file_type={} bytes={} written={} ok={} last_err={}\n",
                h, file_type, bytes.len(), written, ok, last_err
            );
            let _ = std::fs::write(
                std::env::temp_dir().join("cut-ffmpeg-env-debug.log"),
                dbg,
            );
            let _ = std::fs::write(
                std::env::temp_dir().join("cut-ffmpeg-env-filetype.log"),
                if file_type == FILE_TYPE_UNKNOWN {
                    "file_type=UNKNOWN\n".to_string()
                } else {
                    format!("file_type={}\n", file_type)
                },
            );
        }
    }
}

#[cfg(windows)]
unsafe fn handle_checked(handle: *mut core::ffi::c_void) -> Option<*mut core::ffi::c_void> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    if handle.is_null() {
        let _ = std::fs::write(
            std::env::temp_dir().join("cut-ffmpeg-env-debug.log"),
            "handle=NULL\n",
        );
        return None;
    }
    if handle == INVALID_HANDLE_VALUE {
        let _ = std::fs::write(
            std::env::temp_dir().join("cut-ffmpeg-env-debug.log"),
            "handle=INVALID\n",
        );
        return None;
    }
    Some(handle)
}
