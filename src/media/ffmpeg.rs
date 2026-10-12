//! 定位随应用一起分发的 ffmpeg / ffprobe。
//!
//! 发布包里绝不能依赖用户机器上装了什么，所以 bundle 路径优先；`PATH` 只是开发期的兜底，
//! 它让 `cargo run` 在还没跑 `script/build-ffmpeg.sh` 时也能起来。

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 覆盖二进制所在目录的环境变量，测试和高级用户用。
pub const ENV_DIR: &str = "CUT_FFMPEG_DIR";

/// 二进制是从哪儿找到的，出错时要展示给用户看。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinarySource {
    EnvDir,
    AppBundle,
    ExeDir,
    Vendor,
    Path,
}

impl BinarySource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::EnvDir => "环境变量 CUT_FFMPEG_DIR",
            Self::AppBundle => "应用包内",
            Self::ExeDir => "可执行文件同目录",
            Self::Vendor => "开发期 vendor 目录",
            Self::Path => "系统 PATH",
        }
    }
}

/// 一对可用的 ffmpeg / ffprobe 绝对路径。
#[derive(Debug, Clone)]
pub struct FfmpegEnv {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    source: BinarySource,
}

/// 一处都没找到；`searched` 是查过的地方，直接给用户看。
#[derive(Debug, Clone)]
pub struct ResolveError {
    pub searched: Vec<String>,
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "找不到可用的 ffmpeg 与 ffprobe。已查找：")?;
        for place in &self.searched {
            writeln!(f, "  · {place}")?;
        }
        write!(f, "设置 {ENV_DIR} 指向同时包含这两个文件的目录即可。")
    }
}

impl std::error::Error for ResolveError {}

pub fn binary_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// 起 ffmpeg / ffprobe 之前调一次。
///
/// Windows 上 GUI 程序 spawn 控制台程序时，系统会**给子进程新建一个控制台窗口**，
/// 于是每探测一个文件、每转一个文件都闪一下黑框。`CREATE_NO_WINDOW` 关的就是这个。
/// 别的平台没有控制台这回事，这个函数什么都不做。
pub fn no_console_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // winbase.h 里的 CREATE_NO_WINDOW。就为一个常量不值得拉一整个 windows-sys。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

impl FfmpegEnv {
    pub fn resolve() -> Result<Self, ResolveError> {
        let mut searched = Vec::new();

        for (source, dir) in candidate_dirs(&mut searched) {
            if let Some(found) = Self::from_dir(dir, source) {
                return Ok(found);
            }
        }

        if let Some((ffmpeg, ffprobe)) = find_in_path(&mut searched) {
            return Ok(Self {
                ffmpeg,
                ffprobe,
                source: BinarySource::Path,
            });
        }

        Err(ResolveError { searched })
    }

    fn from_dir(dir: PathBuf, source: BinarySource) -> Option<Self> {
        let ffmpeg = dir.join(binary_name("ffmpeg"));
        let ffprobe = dir.join(binary_name("ffprobe"));
        (is_file(&ffmpeg) && is_file(&ffprobe)).then_some(Self {
            ffmpeg,
            ffprobe,
            source,
        })
    }

    pub fn ffmpeg(&self) -> &Path {
        &self.ffmpeg
    }

    pub fn ffprobe(&self) -> &Path {
        &self.ffprobe
    }

    pub fn source(&self) -> BinarySource {
        self.source
    }
}

fn candidate_dirs(searched: &mut Vec<String>) -> Vec<(BinarySource, PathBuf)> {
    let mut candidates = Vec::new();

    if let Ok(dir) = env::var(ENV_DIR) {
        let dir = PathBuf::from(dir);
        searched.push(format!("{}：{}", ENV_DIR, dir.display()));
        candidates.push((BinarySource::EnvDir, dir));
    }

    if let Some(exe) = env::current_exe().ok() {
        if let Some(bin) = macos_bundle_bin_dir(&exe) {
            searched.push(format!("应用包：{}", bin.display()));
            candidates.push((BinarySource::AppBundle, bin));
        }
        if let Some(dir) = exe.parent() {
            searched.push(format!("可执行文件目录：{}", dir.display()));
            candidates.push((BinarySource::ExeDir, dir.to_path_buf()));
        }
    }

    // CARGO_MANIFEST_DIR 是编译期路径，只在开发构建里有意义，绝不能进发布包。
    // 认的是 vendor/ffmpeg-slim/：script/build-ffmpeg.sh 自编的 audio-only LGPL 构建，
    // 也是发布包里装的那一份。
    if cfg!(debug_assertions) {
        let vendor = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("vendor")
            .join("ffmpeg-slim")
            .join(vendor_triple());
        searched.push(format!(
            "vendor（没有就跑 script/build-ffmpeg.sh）：{}",
            vendor.display()
        ));
        candidates.push((BinarySource::Vendor, vendor));
    }

    candidates
}

fn vendor_triple() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// `…/Cut.app/Contents/MacOS/Cut` → `…/Cut.app/Contents/Resources/bin`。
fn macos_bundle_bin_dir(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    if macos.file_name()?.to_str()? != "MacOS" {
        return None;
    }
    Some(macos.parent()?.join("Resources").join("bin"))
}

fn find_in_path(searched: &mut Vec<String>) -> Option<(PathBuf, PathBuf)> {
    let path_var = env::var_os("PATH")?;
    let mut ffmpeg = None;
    let mut ffprobe = None;

    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(binary_name("ffmpeg"));
        if ffmpeg.is_none() && is_file(&candidate) {
            ffmpeg = Some(candidate);
        }
        let candidate = dir.join(binary_name("ffprobe"));
        if ffprobe.is_none() && is_file(&candidate) {
            ffprobe = Some(candidate);
        }
        if ffmpeg.is_some() && ffprobe.is_some() {
            searched.push("PATH".to_string());
            return Some((ffmpeg?, ffprobe?));
        }
    }

    searched.push("PATH（未同时找到 ffmpeg 与 ffprobe）".to_string());
    None
}

fn is_file(path: &Path) -> bool {
    match path.metadata() {
        Ok(meta) if meta.is_file() => is_executable(&meta),
        _ => false,
    }
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &std::fs::Metadata) -> bool {
    true
}
