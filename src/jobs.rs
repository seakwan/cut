//! 任务队列能力层：待处理文件、设置、每个文件的状态与日志。
//!
//! 纯数据，不认识 gpui_kit。上层用 `Entity<JobStore>` 持有它，所有变更走
//! `store.update(cx, |store, cx| { …; cx.notify(); })`，这样两个 tab 各自一份队列、互不干扰。

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::media::{AudioFormat, Encoding, Overwrite, Quality};
use crate::scan::{self, ScanOutcome};
use crate::split::SplitMode;

/// 日志行数上限：跑大批文件时不能无限涨。
const MAX_LOG_LINES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(u64);

impl FileId {
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FileStatus {
    Probing,
    Ready,
    Queued,
    Running { percent: f32 },
    Done { output_count: usize },
    Cancelled,
    ProbeFailed(String),
    Failed(String),
}

impl FileStatus {
    pub fn label(&self) -> Cow<'static, str> {
        match self {
            Self::Probing => Cow::Borrowed("探测中"),
            Self::Ready => Cow::Borrowed("就绪"),
            Self::Queued => Cow::Borrowed("排队中"),
            Self::Running { percent } => Cow::Owned(format!("{percent:.0}%")),
            Self::Done { output_count } => Cow::Owned(format!("完成 · {output_count} 个")),
            Self::Cancelled => Cow::Borrowed("已取消"),
            Self::ProbeFailed(message) | Self::Failed(message) => Cow::Owned(message.clone()),
        }
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::ProbeFailed(_) | Self::Failed(_))
    }

    /// 已经在跑或排队的文件不能被再次提交。
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Probing | Self::Queued | Self::Running { .. })
    }
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    id: FileId,
    path: PathBuf,
    info: Option<crate::media::MediaInfo>,
    status: FileStatus,
}

impl FileEntry {
    pub fn id(&self) -> FileId {
        self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn info(&self) -> Option<&crate::media::MediaInfo> {
        self.info.as_ref()
    }

    pub fn status(&self) -> &FileStatus {
        &self.status
    }

    pub fn file_name(&self) -> Cow<'_, str> {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_else(|| self.path.to_string_lossy())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Split,
    Convert,
}

/// 切割设置。界面直接改字段，测试直接构造，所以**不要加 `#[non_exhaustive]`**：
/// 那是给「crate 自己产出、外部只读」的类型准备的，加在这里只会让调用方没法用
/// `..Default::default()` 造一个出来。
#[derive(Debug, Clone)]
pub struct SplitSettings {
    pub mode: SplitMode,
    /// 关闭 = 无损直拷（快，切点可能差几十毫秒）；打开 = 重编码，切点样本级准确。
    pub exact: bool,
    pub output_dir: Option<PathBuf>,
    pub overwrite: Overwrite,
    /// 支持 `{name}` 与 `{index}`（1 起）。
    pub name_template: String,
    pub keep_tags_and_cover: bool,
    pub concurrency: u8,
}

impl Default for SplitSettings {
    fn default() -> Self {
        Self {
            mode: SplitMode::ByDuration(300.0),
            exact: false,
            output_dir: None,
            overwrite: Overwrite::Skip,
            name_template: "{name}_part{index}".to_string(),
            keep_tags_and_cover: true,
            concurrency: 2,
        }
    }
}

impl SplitSettings {
    pub fn encoding(&self) -> Encoding {
        if self.exact {
            Encoding::Reencode(Quality::High)
        } else {
            Encoding::Copy
        }
    }
}

/// 转换设置。同 [`SplitSettings`]，界面和测试都要能直接构造。
#[derive(Debug, Clone)]
pub struct ConvertSettings {
    pub target: AudioFormat,
    pub quality: Quality,
    pub keep_tags_and_cover: bool,
    pub output_dir: Option<PathBuf>,
    pub overwrite: Overwrite,
    pub concurrency: u8,
}

impl Default for ConvertSettings {
    fn default() -> Self {
        Self {
            target: AudioFormat::Mp3,
            quality: Quality::Medium,
            keep_tags_and_cover: true,
            output_dir: None,
            overwrite: Overwrite::Skip,
            concurrency: 2,
        }
    }
}

/// 一个 tab 一份设置；`kind` 决定用哪一支，不存在第二份真相。
#[derive(Debug, Clone)]
pub enum Settings {
    Split(SplitSettings),
    Convert(ConvertSettings),
}

impl Settings {
    pub fn kind(&self) -> JobKind {
        match self {
            Self::Split(_) => JobKind::Split,
            Self::Convert(_) => JobKind::Convert,
        }
    }

    pub fn output_dir(&self) -> Option<&Path> {
        match self {
            Self::Split(settings) => settings.output_dir.as_deref(),
            Self::Convert(settings) => settings.output_dir.as_deref(),
        }
    }

    pub fn overwrite(&self) -> Overwrite {
        match self {
            Self::Split(settings) => settings.overwrite,
            Self::Convert(settings) => settings.overwrite,
        }
    }

    pub fn concurrency(&self) -> u8 {
        match self {
            Self::Split(settings) => settings.concurrency,
            Self::Convert(settings) => settings.concurrency,
        }
    }

    pub fn as_split(&self) -> Option<&SplitSettings> {
        match self {
            Self::Split(settings) => Some(settings),
            _ => None,
        }
    }

    pub fn as_split_mut(&mut self) -> Option<&mut SplitSettings> {
        match self {
            Self::Split(settings) => Some(settings),
            _ => None,
        }
    }

    pub fn as_convert(&self) -> Option<&ConvertSettings> {
        match self {
            Self::Convert(settings) => Some(settings),
            _ => None,
        }
    }

    pub fn as_convert_mut(&mut self) -> Option<&mut ConvertSettings> {
        match self {
            Self::Convert(settings) => Some(settings),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Error,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LogLine {
    pub level: LogLevel,
    pub text: String,
}

#[derive(Debug)]
pub struct JobStore {
    settings: Settings,
    files: Vec<FileEntry>,
    selected: Option<FileId>,
    log: Vec<LogLine>,
    next_id: u64,
}

impl JobStore {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            files: Vec::new(),
            selected: None,
            log: Vec::new(),
            next_id: 0,
        }
    }

    pub fn kind(&self) -> JobKind {
        self.settings.kind()
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// 追加文件，按路径去重；返回真正新增的 id，调用方据此安排探测。
    pub fn add_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Vec<FileId> {
        let existing: HashSet<PathBuf> = self.files.iter().map(|file| file.path.clone()).collect();
        let mut added = Vec::new();

        for path in paths {
            if existing.contains(&path) {
                continue;
            }
            let id = FileId(self.next_id);
            self.next_id += 1;
            self.files.push(FileEntry {
                id,
                path: path.clone(),
                info: None,
                status: FileStatus::Probing,
            });
            added.push(id);
        }

        added
    }

    pub fn files(&self) -> &[FileEntry] {
        &self.files
    }

    /// 收下一批扫描结果：按路径去重、写日志，返回真正新增的 id。
    ///
    /// 新文件一律停在 [`FileStatus::Probing`]：还没跑过 ffprobe 就没有时长和码率，
    /// 按大小/时长切割的数学根本无从算起。调用方**必须**把返回的 id 交给
    /// [`crate::scheduler::Scheduler::queue_probe`]，否则它们会永远转圈。
    ///
    /// 两个 tab 都走这里，保证"添加文件夹"在切割和转换两边行为完全一致。
    pub fn ingest(&mut self, outcome: ScanOutcome) -> Vec<FileId> {
        let count = outcome.files.len();
        let added = self.add_paths(outcome.files);

        if outcome.truncated {
            self.push_log(
                LogLevel::Error,
                format!("文件太多，只收了前 {} 个，请缩小选择范围", scan::MAX_FILES),
            );
        }
        if outcome.unreadable > 0 {
            self.push_log(
                LogLevel::Info,
                format!("有 {} 个目录读不了，已跳过", outcome.unreadable),
            );
        }
        if count > 0 {
            self.push_log(LogLevel::Info, format!("已添加 {count} 个文件"));
        }

        added
    }

    pub fn file(&self, id: FileId) -> Option<&FileEntry> {
        self.files.iter().find(|file| file.id == id)
    }

    pub fn remove(&mut self, id: FileId) -> bool {
        let before = self.files.len();
        self.files.retain(|file| file.id != id);
        if self.selected == Some(id) {
            self.selected = None;
        }
        self.files.len() != before
    }

    pub fn clear(&mut self) {
        self.files.clear();
        self.selected = None;
    }

    pub fn selected(&self) -> Option<FileId> {
        self.selected
    }

    pub fn select(&mut self, id: Option<FileId>) {
        self.selected = id;
    }

    pub fn set_info(&mut self, id: FileId, info: crate::media::MediaInfo) -> bool {
        let Some(file) = self.files.iter_mut().find(|file| file.id == id) else {
            return false;
        };
        file.info = Some(info);
        file.status = FileStatus::Ready;
        true
    }

    pub fn set_status(&mut self, id: FileId, status: FileStatus) -> bool {
        let Some(file) = self.files.iter_mut().find(|file| file.id == id) else {
            return false;
        };
        file.status = status;
        true
    }

    /// 有文件在跑时禁止关窗、禁止再次提交。
    pub fn is_busy(&self) -> bool {
        self.files.iter().any(|file| file.status().is_busy())
    }

    pub fn push_log(&mut self, level: LogLevel, text: impl Into<String>) {
        self.log.push(LogLine {
            level,
            text: text.into(),
        });
        if self.log.len() > MAX_LOG_LINES {
            let overflow = self.log.len() - MAX_LOG_LINES;
            self.log.drain(..overflow);
        }
    }

    pub fn log(&self) -> &[LogLine] {
        &self.log
    }

    pub fn clear_log(&mut self) {
        self.log.clear();
    }
}
