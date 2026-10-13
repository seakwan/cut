//! 授权 UI：试用横幅 + 激活对话框。
//!
//! 横幅在 AppView 顶部，Trial 时黄色、Expired 时红色、Licensed 时不显示。
//! 激活对话框用 `window.open_dialog` 弹出，成功后通过 `Entity::update` 通知 AppView 重画。

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Sizable as _, StyledExt as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::license::{self, LicenseStatus};

/// 试用横幅。Licensed 时返回 None，调用方 `.when_some` 即可。
pub fn render_banner(
    status: &LicenseStatus,
    on_activate: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Option<impl IntoElement> {
    let theme_colors = banner_colors(status);
    let (text, show_button) = match status {
        LicenseStatus::Trial { remaining } => {
            (format!("试用模式：还剩 {remaining} 次免费启动"), true)
        }
        LicenseStatus::Expired { .. } => ("试用已到期，请输入激活码继续使用".to_string(), true),
        LicenseStatus::Licensed { .. } => return None,
    };

    let (bg, fg, border) = theme_colors;

    Some(
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .bg(bg)
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(fg)
                    .child(text),
            )
            .when(show_button, |this| {
                this.child(
                    Button::new("activate-btn")
                        .label("输入激活码")
                        .small()
                        .primary()
                        .on_click(on_activate),
                )
            }),
    )
}

fn banner_colors(status: &LicenseStatus) -> (gpui::Hsla, gpui::Hsla, gpui::Hsla) {
    match status {
        LicenseStatus::Trial { .. } => (
            gpui::hsla(45.0 / 360.0, 0.9, 0.95, 1.0),
            gpui::hsla(45.0 / 360.0, 0.8, 0.2, 1.0),
            gpui::hsla(45.0 / 360.0, 0.7, 0.7, 1.0),
        ),
        LicenseStatus::Expired { .. } => (
            gpui::hsla(0.0, 0.9, 0.95, 1.0),
            gpui::hsla(0.0, 0.8, 0.25, 1.0),
            gpui::hsla(0.0, 0.7, 0.7, 1.0),
        ),
        LicenseStatus::Licensed { .. } => unreachable!(),
    }
}

/// 弹出激活对话框。成功后更新 `app_view` 的授权状态并通知重画。
pub fn open_activation_dialog(
    app_view: Entity<crate::app::AppView>,
    fingerprint: String,
    window: &mut Window,
    cx: &mut App,
) {
    let display_id = license::display_fingerprint(&fingerprint);
    let key_input = cx.new(|cx| {
        InputState::new(window, cx)
            .default_value("")
            .placeholder("粘贴激活码")
    });

    let error_text = cx.new(|_| String::new());

    window.open_dialog(cx, {
        let display_id = display_id.clone();
        let key_input = key_input.clone();
        let error_text = error_text.clone();
        let fingerprint = fingerprint.clone();
        let app_view = app_view.clone();

        move |dialog, _, _cx| {
            let key_input_for_content = key_input.clone();
            let error_for_content = error_text.clone();

            dialog
                .title("激活应用")
                .w(px(480.))
                .overlay_closable(false)
                .content({
                    let display_id = display_id.clone();
                    move |content, _, cx| {
                        content
                            .child(
                                div()
                                    .v_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("请将下方的机器码发给开发者获取激活码："),
                                    )
                                    .child(
                                        div()
                                            .h_flex()
                                            .items_center()
                                            .gap_2()
                                            .rounded_lg()
                                            .bg(cx.theme().muted)
                                            .px_3()
                                            .py_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_sm()
                                                    .child(display_id.clone()),
                                            )
                                            .child(
                                                Button::new("copy-id")
                                                    .label("复制")
                                                    .small()
                                                    .on_click({
                                                        let display_id = display_id.clone();
                                                        move |_, _, cx| {
                                                            cx.write_to_clipboard(
                                                                ClipboardItem::new_string(display_id.clone()),
                                                            );
                                                        }
                                                    }),
                                            ),
                                    )
                                    .child(div().pt_2().text_sm().child("激活码："))
                                    .child(Input::new(&key_input_for_content).w_full())
                                    .child(
                                        div()
                                            .pt_1()
                                            .text_sm()
                                            .text_color(cx.theme().danger)
                                            .child(error_for_content.read(cx).clone()),
                                    ),
                            )
                    }
                })
                .button_props(
                    gpui_kit::component::dialog::DialogButtonProps::default()
                        .show_cancel(true)
                        .cancel_text("取消")
                        .ok_text("激活")
                        .on_ok({
                            let key_input = key_input.clone();
                            let error_text = error_text.clone();
                            let fingerprint = fingerprint.clone();
                            let app_view = app_view.clone();

                            move |_, _, cx| {
                                let key = key_input.read(cx).value().to_string();
                                if key.trim().is_empty() {
                                    error_text.update(cx, |text, _| {
                                        *text = "请输入激活码".to_string();
                                    });
                                    return false;
                                }

                                match license::activate_license(&fingerprint, key.trim()) {
                                    Ok(status) => {
                                        app_view.update(cx, |this, cx| {
                                            this.set_license_status(status);
                                            cx.notify();
                                        });
                                        true
                                    }
                                    Err(err) => {
                                        error_text.update(cx, |text, _| {
                                            *text = err;
                                        });
                                        false
                                    }
                                }
                            }
                        }),
                )
        }
    });
}
