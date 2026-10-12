//! 原生文件选择对话框。
//!
//! gpui-kit 不带选择器，这里用 rfd。三个函数都是纯 async、不认识 gpui：
//! 调用方在自己的 `cx.spawn` 里 await，拿到路径后再更新 store，
//! 这样对话框绝不会阻塞主线程，也不会把窗口句柄带进异步任务。

use std::path::PathBuf;

/// 和 `AudioFormat::from_extension` 认的后缀保持一致。
const AUDIO_EXTENSIONS: &[&str] = &[
    "m4a", "m4b", "mp4", "mp3", "wav", "wave", "aiff", "aif", "flac", "aac", "adts", "ogg", "oga",
    "opus",
];

/// 用户取消时返回 `None`。
pub async fn audio_files() -> Option<Vec<PathBuf>> {
    let handles = rfd::AsyncFileDialog::new()
        .set_title("选择音频文件")
        .add_filter("音频", AUDIO_EXTENSIONS)
        .pick_files()
        .await?;
    Some(
        handles
            .iter()
            .map(|handle| handle.path().to_path_buf())
            .collect(),
    )
}

pub async fn folder(title: &str) -> Option<PathBuf> {
    let handle = rfd::AsyncFileDialog::new()
        .set_title(title)
        .pick_folder()
        .await?;
    Some(handle.path().to_path_buf())
}
