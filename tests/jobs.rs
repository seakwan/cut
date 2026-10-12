//! 队列语义与目录扫描。
//!
//! 这两块都不认识 gpui_kit，所以能在没有窗口的情况下测：去重、日志上限、
//! 忙碌状态、以及"选中文件夹"到底会收进哪些文件。

use std::path::{Path, PathBuf};

use cut::jobs::{FileStatus, JobStore, LogLevel, Settings, SplitSettings};
use cut::scan;

mod common;

use common::TempDir;

fn split_store() -> JobStore {
    JobStore::new(Settings::Split(SplitSettings::default()))
}

fn outcome(paths: &[&str]) -> scan::ScanOutcome {
    scan::ScanOutcome {
        files: paths.iter().map(PathBuf::from).collect(),
        truncated: false,
        unreadable: 0,
    }
}

#[test]
fn ingest_dedupes_by_path_and_keeps_order() {
    let mut store = split_store();

    store.ingest(outcome(&["/a/1.m4a", "/a/2.m4a"]));
    store.ingest(outcome(&["/a/2.m4a", "/a/3.m4a"]));

    let names: Vec<String> = store
        .files()
        .iter()
        .map(|f| f.file_name().to_string())
        .collect();
    assert_eq!(
        names,
        ["1.m4a", "2.m4a", "3.m4a"],
        "拖两次同一个文件夹不能翻倍"
    );
}

#[test]
fn ingested_files_wait_for_a_probe() {
    let mut store = split_store();
    let added = store.ingest(outcome(&["/a/1.m4a"]));

    // 没跑过 ffprobe 就没有时长和码率，按大小/时长切割的数学无从算起——
    // 所以新文件必须先卡在「探测中」，由 scheduler 把它推进到「就绪」。
    assert_eq!(added.len(), 1, "要告诉调用方去探测哪几个");
    assert_eq!(store.files()[0].status(), &FileStatus::Probing);
    assert!(store.is_busy(), "探测中的文件也是忙，这期间不能再提交一批");
}

#[test]
fn busy_tracks_probe_queue_and_running_states() {
    let mut store = split_store();
    store.ingest(outcome(&["/a/1.m4a", "/a/2.m4a"]));
    let ids: Vec<_> = store.files().iter().map(|f| f.id()).collect();
    for &id in &ids {
        store.set_status(id, FileStatus::Ready);
    }

    assert!(!store.is_busy());
    store.set_status(ids[0], FileStatus::Running { percent: 40.0 });
    assert!(store.is_busy(), "有文件在跑时不能关窗也不能再次提交");

    store.set_status(ids[0], FileStatus::Done { output_count: 3 });
    assert!(!store.is_busy());

    store.set_status(ids[1], FileStatus::Queued);
    assert!(store.is_busy());
}

#[test]
fn status_labels_are_ready_to_render() {
    assert_eq!(FileStatus::Probing.label(), "探测中");
    assert_eq!(FileStatus::Running { percent: 42.4 }.label(), "42%");
    assert_eq!(FileStatus::Done { output_count: 3 }.label(), "完成 · 3 个");

    let failed = FileStatus::Failed("磁盘满了".to_string());
    assert!(failed.is_failed());
    assert_eq!(failed.label(), "磁盘满了", "错误原文要直接可见");
    assert!(!FileStatus::Done { output_count: 1 }.is_failed());
}

#[test]
fn setting_status_of_a_removed_file_is_a_no_op() {
    let mut store = split_store();
    store.ingest(outcome(&["/a/1.m4a"]));
    let id = store.files()[0].id();

    assert!(store.remove(id));
    assert!(!store.remove(id));
    assert!(!store.set_status(id, FileStatus::Done { output_count: 1 }));
}

#[test]
fn removing_the_selected_file_clears_the_selection() {
    let mut store = split_store();
    store.ingest(outcome(&["/a/1.m4a"]));
    let id = store.files()[0].id();
    store.select(Some(id));

    store.remove(id);
    assert_eq!(store.selected(), None, "选中项没了，详情面板不能指着空气");
}

#[test]
fn clear_keeps_settings() {
    let mut store = split_store();
    store.ingest(outcome(&["/a/1.m4a"]));
    store.select(Some(store.files()[0].id()));
    store.settings_mut().as_split_mut().unwrap().exact = true;

    store.clear();

    assert!(store.files().is_empty());
    assert_eq!(store.selected(), None);
    assert!(
        store.settings().as_split().unwrap().exact,
        "清空列表不该顺手改掉设置"
    );
}

#[test]
fn log_is_ring_buffered() {
    let mut store = split_store();
    for ix in 0..600 {
        store.push_log(LogLevel::Info, format!("第 {ix} 行"));
    }

    let log = store.log();
    assert_eq!(log.len(), 500, "跑几千个文件时日志不能无限涨");
    assert_eq!(log.first().unwrap().text, "第 100 行", "裁掉的是最旧的");
    assert_eq!(log.last().unwrap().text, "第 599 行");
    assert_eq!(log.last().unwrap().level, LogLevel::Info);
}

#[test]
fn settings_variant_decides_the_tab() {
    let store = split_store();
    assert_eq!(store.kind(), cut::jobs::JobKind::Split);
    assert!(store.settings().as_split().is_some());
    assert!(store.settings().as_convert().is_none());
}

#[test]
fn defaults_are_the_safe_choice() {
    let settings = SplitSettings::default();
    // 覆盖是不可逆的，默认必须是不动已有文件。
    assert_eq!(settings.overwrite, cut::media::Overwrite::Skip);
    assert!(!settings.exact, "默认无损直拷，重编码要用户主动打开");
    assert!(settings.keep_tags_and_cover);
    assert_eq!(settings.name_template, "{name}_part{index}");
    assert_eq!(settings.output_dir, None, "默认落在源文件旁边");
}

// ---- 目录扫描 ----

#[test]
fn folder_scan_is_recursive_sorted_and_filtered() {
    let dir = TempDir::new("scan");
    dir.write("song1.m4a");
    dir.write("song3.mp3");
    dir.write("notes.txt");
    dir.write("cover.jpg");

    let outcome = scan::collect([dir.path().to_path_buf()]);

    let names: Vec<String> = outcome
        .files
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["song1.m4a", "song3.mp3"],
        "顺序要稳定，非音频要滤掉"
    );
    assert!(!outcome.truncated);
    assert_eq!(outcome.unreadable, 0);
}

#[test]
fn scan_order_is_by_full_path_so_subdirectories_group_together() {
    // 契约是"按完整路径排序"，不是按文件名：子目录整体排在同名前缀的前面。
    // 界面若要按名字显示，得自己再排一次，不能指望这里。
    let dir = TempDir::new("scan-order");
    dir.write("b/song2.mp3");
    dir.write("song1.m4a");

    let outcome = scan::collect([dir.path().to_path_buf()]);
    assert_eq!(
        outcome.files,
        vec![dir.path().join("b/song2.mp3"), dir.path().join("song1.m4a")]
    );

    // 排序还要保证同一批路径两次扫描结果一致，否则列表会无理由跳动。
    let again = scan::collect([dir.path().to_path_buf()]);
    assert_eq!(again.files, outcome.files);
}

#[test]
fn single_files_pass_through_and_duplicates_collapse() {
    let dir = TempDir::new("single");
    let song = dir.write("song.flac");

    let outcome = scan::collect([song.clone(), song.clone(), dir.path().join("missing.wav")]);
    assert_eq!(outcome.files, vec![song], "missing.wav 不存在，不该进队列");
}

#[test]
fn macos_bundles_are_not_walked_into() {
    let dir = TempDir::new("bundle");
    dir.write("Library/Some.app/Contents/Resources/song.mp3");
    dir.write("real.m4a");

    let outcome = scan::collect([dir.path().to_path_buf()]);
    let names: Vec<String> = outcome
        .files
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["real.m4a"], "钻进 .app 会收进一堆系统音频");
}

#[test]
fn is_audio_matches_the_format_table() {
    assert!(scan::is_audio(Path::new("/x/a.M4A")));
    assert!(scan::is_audio(Path::new("/x/a.opus")));
    assert!(!scan::is_audio(Path::new("/x/a.txt")));
    assert!(!scan::is_audio(Path::new("/x/a")));
}
