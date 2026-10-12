//! Cut —— 本地音频切割与格式转换。
//!
//! 分层（依赖只向下）：
//!
//! - [`app`] 窗口外壳：标签页切换、ffmpeg 可用性提示；
//! - [`split`] / [`convert`] 两个 feature：各自的界面与工作流；
//! - [`jobs`] 队列与设置的纯数据模型，[`components`] 两个 feature 共用的展示零件；
//! - [`media`] ffmpeg 能力层（定位二进制、探测、构造命令、解析进度），
//!   [`runner`] 把「设置 + 探测结果」翻译成命令并阻塞执行，
//!   [`scheduler`] 用 gpui_kit 的异步原语把 runner 挂到界面上（进度、取消、探测排队），
//!   [`scan`] 目录扫描，[`pick`] 原生选择对话框，[`format`] 显示用的数值格式化。
//!
//! `media` / `runner` / `jobs` / `scan` / `split::plan` / `media::command` 都不认识 gpui_kit，
//! 因此可以脱离窗口做单元测试。

pub mod app;
pub mod components;
pub mod convert;
pub mod format;
pub mod jobs;
pub mod media;
pub mod pick;
pub mod runner;
pub mod scan;
pub mod scheduler;
pub mod split;
