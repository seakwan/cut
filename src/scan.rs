//! 把用户选的东西（单个文件、一堆文件、一个文件夹）统一成一串音频文件路径。
//!
//! 只认扩展名，不做内容嗅探：ffprobe 会在下一步给出权威答案，这里只负责过滤掉明显不相干的。

use std::fs;
use std::path::{Path, PathBuf};

use crate::media::AudioFormat;

/// 递归上限。用户手滑选中磁盘根目录时不能把机器拖死。
const MAX_DEPTH: usize = 12;
/// 一次最多收这么多文件，超出就停手，由界面告诉用户。
pub const MAX_FILES: usize = 5000;

#[derive(Debug, Default, Clone)]
pub struct ScanOutcome {
    pub files: Vec<PathBuf>,
    /// 因为到达 `MAX_FILES` 而没有继续扫的目录数，用来提示"还有更多"。
    pub truncated: bool,
    /// 跳过的、读不了的目录。
    pub unreadable: usize,
}

pub fn is_audio(path: &Path) -> bool {
    AudioFormat::from_path(path).is_some()
}

/// 收集音频文件：文件原样收（前提是音频扩展名），目录递归展开。结果按路径排序，顺序稳定。
pub fn collect(paths: impl IntoIterator<Item = PathBuf>) -> ScanOutcome {
    let mut outcome = ScanOutcome::default();

    for path in paths {
        if outcome.files.len() >= MAX_FILES {
            outcome.truncated = true;
            break;
        }
        if path.is_dir() {
            walk(&path, 0, &mut outcome);
        } else if path.is_file() && is_audio(&path) {
            outcome.files.push(path);
        }
    }

    outcome.files.sort();
    outcome.files.dedup();
    outcome
}

fn walk(dir: &Path, depth: usize, outcome: &mut ScanOutcome) {
    if depth > MAX_DEPTH || outcome.files.len() >= MAX_FILES {
        return;
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            outcome.unreadable += 1;
            return;
        }
    };

    for entry in entries.flatten() {
        if outcome.files.len() >= MAX_FILES {
            outcome.truncated = true;
            return;
        }
        let path = entry.path();
        // macOS 包（.app/.bundle）当目录看会钻进去，直接跳过。
        if is_bundle(&path) {
            continue;
        }
        if path.is_dir() {
            walk(&path, depth + 1, outcome);
        } else if is_audio(&path) {
            outcome.files.push(path);
        }
    }
}

fn is_bundle(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "app" | "bundle" | "framework"
    )
}
