//! 应用外壳：只负责窗口级的东西——标签页切换、ffmpeg 可用性提示、授权横幅。
//!
//! 功能逻辑全在 `split` / `convert` 两个 feature 里，外壳不认识它们的设置项。

use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, StyledExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::about::AboutView;
use crate::convert::ConvertView;
use crate::license::{self, LicenseStatus};
use crate::license_view;
use crate::media::{FfmpegEnv, ResolveError};
use crate::split::SplitView;

pub struct AppView {
    active_tab: usize,
    ffmpeg: Result<FfmpegEnv, ResolveError>,
    split: Entity<SplitView>,
    convert: Entity<ConvertView>,
    about: Entity<AboutView>,
    fingerprint: String,
    license_status: LicenseStatus,
}

impl AppView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 解析一次就够：应用运行期间不会有人往 bundle 里塞 ffmpeg。
        // 两个视图各拿一份克隆，自己那边的调度器才知道去哪儿找 ffmpeg / ffprobe；
        // 外壳留着原件只为画顶上那条可用性提示。
        let ffmpeg = FfmpegEnv::resolve();
        let split_ffmpeg = ffmpeg.clone();
        let convert_ffmpeg = ffmpeg.clone();
        let about_ffmpeg = ffmpeg.clone();

        // 授权：采集机器指纹 + 记录本次启动。
        let fingerprint = license::machine_fingerprint().unwrap_or_default();
        let license_status = license::record_launch(&fingerprint);
        let license_expired = matches!(license_status, LicenseStatus::Expired { .. });

        Self {
            active_tab: 0,
            ffmpeg,
            split: cx.new(|cx| SplitView::new(window, split_ffmpeg, license_expired, cx)),
            convert: cx.new(|cx| ConvertView::new(window, convert_ffmpeg, license_expired, cx)),
            about: cx.new(|_| AboutView::new(about_ffmpeg)),
            fingerprint,
            license_status,
        }
    }

    /// 激活成功后由对话框回调调用，更新状态并触发重画。
    pub fn set_license_status(&mut self, status: LicenseStatus, cx: &mut Context<Self>) {
        let expired = matches!(status, LicenseStatus::Expired { .. });
        self.license_status = status;
        self.split
            .update(cx, |view, cx| view.set_license_expired(expired, cx));
        self.convert
            .update(cx, |view, cx| view.set_license_expired(expired, cx));
    }
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let app_entity = cx.entity().clone();
        let fingerprint = self.fingerprint.clone();

        div()
            .v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(self.render_ffmpeg_status(cx))
            .when_some(license_view::render_banner(&self.license_status, move |_, window, cx| {
                license_view::open_activation_dialog(app_entity.clone(), fingerprint.clone(), window, cx);
            }), |this, banner| this.child(banner))
            .child(
                div().px_3().pt_2().child(
                    TabBar::new("main-tabs")
                        .selected_index(self.active_tab)
                        .on_click(cx.listener(|this: &mut Self, index: &usize, _, cx| {
                            this.active_tab = *index;
                            cx.notify();
                        }))
                        .child(Tab::new().label("切割"))
                        .child(Tab::new().label("转换"))
                        .child(Tab::new().label("关于")),
                ),
            )
            .child(div().flex_1().min_h_0().child(match self.active_tab {
                0 => self.split.clone().into_any_element(),
                1 => self.convert.clone().into_any_element(),
                _ => self.about.clone().into_any_element(),
            }))
    }
}

impl AppView {
    /// ffmpeg 没找到时给一条能照着做的提示，而不是等用户点"开始"再报错。
    fn render_ffmpeg_status(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();

        match &self.ffmpeg {
            Ok(env) => div()
                .h_flex()
                .gap_2()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("ffmpeg：{}", env.source().label())),
            Err(err) => div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(theme.danger)
                .whitespace_normal()
                .child(err.to_string()),
        }
    }
}
