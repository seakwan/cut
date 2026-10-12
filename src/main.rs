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

            let _ = open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| cut::app::AppView::new(window, cx))
            })
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
