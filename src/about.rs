//! 关于页：应用版本、机器码、授权状态、ffmpeg 来源与随包许可说明。
//!
//! LGPL-2.1 §6 要求随二进制分发时提供许可证文本。包里已经带了
//! licenses/ffmpeg-slim/（bundle 脚本负责放、bundle 自检负责验），这里把它
//! 露出来，让用户在 app 里真的能找到——合规的关键是"用户拿得到"，不是"文件在包里"。

use std::path::{Path, PathBuf};
use std::process::Command;

use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable as _, StyledExt as _};
use gpui_kit::*;

use crate::license::{self, LicenseStatus};
use crate::media::{no_console_window, FfmpegEnv, ResolveError};

pub struct AboutView {
    ffmpeg: Result<FfmpegEnv, ResolveError>,
    licenses_dir: Option<PathBuf>,
    fingerprint: String,
    license_status: LicenseStatus,
}

impl AboutView {
    pub fn new(ffmpeg: Result<FfmpegEnv, ResolveError>) -> Self {
        let fingerprint = license::machine_fingerprint().unwrap_or_default();
        let license_status = license::check_status(&fingerprint);

        Self {
            ffmpeg,
            licenses_dir: resolve_licenses_dir(),
            fingerprint,
            license_status,
        }
    }

    fn open_licenses_dir(&self) {
        let Some(dir) = &self.licenses_dir else { return };
        let mut cmd = if cfg!(target_os = "macos") {
            let mut c = Command::new("open");
            c.arg(dir);
            c
        } else if cfg!(windows) {
            let mut c = Command::new("explorer");
            c.arg(dir);
            c
        } else {
            let mut c = Command::new("xdg-open");
            c.arg(dir);
            c
        };
        no_console_window(&mut cmd);
        let _ = cmd.spawn();
    }
}

impl Render for AboutView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        let ffmpeg_line = match &self.ffmpeg {
            Ok(env) => format!(
                "ffmpeg / ffprobe（{}）：{}",
                env.source().label(),
                env.ffmpeg().display()
            ),
            Err(_) => "ffmpeg / ffprobe：未找到".to_string(),
        };

        let machine_id = license::display_fingerprint(&self.fingerprint);
        let license_line = match &self.license_status {
            LicenseStatus::Trial { remaining } => format!("试用中（剩余 {remaining} 次）"),
            LicenseStatus::Expired { .. } => "试用已到期".to_string(),
            LicenseStatus::Licensed { permanent, expires } => {
                if *permanent {
                    "已激活（永久）".to_string()
                } else if let Some(date) = expires {
                    format!("已激活（到期：{date}）")
                } else {
                    "已激活".to_string()
                }
            }
        };

        div()
            .v_flex()
            .size_full()
            .p_6()
            .gap_3()
            .text_sm()
            .text_color(theme.foreground)
            .child(format!("Cut v{}", env!("CARGO_PKG_VERSION")))
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child("本地音频切割与格式转换。所有处理都在本机完成，不上传任何文件。"),
            )
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_color(theme.muted_foreground).child("机器码："))
                    .child(div().child(machine_id.clone()))
                    .child(
                        Button::new("copy-machine-id")
                            .label("复制")
                            .small()
                            .on_click({
                                let machine_id = machine_id.clone();
                                move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        machine_id.clone(),
                                    ));
                                }
                            }),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_color(theme.muted_foreground).child("授权状态："))
                    .child(div().child(license_line)),
            )
            .child(ffmpeg_line)
            .child(
                div()
                    .whitespace_normal()
                    .text_color(theme.muted_foreground)
                    .child(concat!(
                        "本应用包含 FFmpeg（LGPL-2.1-or-later）及其静态链接的 libmp3lame、",
                        "libvorbis、libopus。这些库的许可证全文、来源与构建证据随应用分发，",
                        "FFmpeg 的源代码可在 ffmpeg.org 获取，也可用下面的按钮打开许可证文件夹。",
                    )),
            )
            .child(
                div().pt_2().child(match &self.licenses_dir {
                    Some(_) => Button::new("open-licenses")
                        .label("打开许可证文件夹")
                        .small()
                        .on_click(cx.listener(|this: &mut Self, _, _, _| {
                            this.open_licenses_dir()
                        })),
                    None => Button::new("open-licenses")
                        .label("许可证文件夹（未随本包分发）")
                        .small()
                        .disabled(true),
                }),
            )
    }
}

/// 从二进制位置反推随包的 licenses/ffmpeg-slim/。候选顺序和 media::ffmpeg 同构：
/// bundle 路径优先，开发构建再退到仓库根目录。
fn resolve_licenses_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;

    // Cut.app/Contents/MacOS/Cut → Contents/Resources/licenses/ffmpeg-slim
    if let Some(macos) = exe.parent() {
        if macos.file_name().and_then(|n| n.to_str()) == Some("MacOS") {
            let dir = macos
                .parent()?
                .join("Resources")
                .join("licenses")
                .join("ffmpeg-slim");
            if has_notice(&dir) {
                return Some(dir);
            }
        }
    }

    // Windows 包：Cut-windows/Cut.exe → Cut-windows/licenses/ffmpeg-slim
    if let Some(dir) = exe.parent().map(|d| d.join("licenses").join("ffmpeg-slim")) {
        if has_notice(&dir) {
            return Some(dir);
        }
    }

    // CARGO_MANIFEST_DIR 是编译期路径，只在开发构建里有意义，绝不能进发布包。
    if cfg!(debug_assertions) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("licenses")
            .join("ffmpeg-slim");
        if has_notice(&dir) {
            return Some(dir);
        }
    }

    None
}

fn has_notice(dir: &Path) -> bool {
    dir.join("LICENSE.txt").is_file() && dir.join("NOTICE.txt").is_file()
}
