//! 授权模块：试用计数 + 离线激活码。
//!
//! 状态机：
//! ```text
//! 首次启动 → Trial(3) → Trial(2) → Trial(1) → Trial(0) → Expired
//!                                                       ↓ 输入激活码
//!                                                   Licensed
//! ```

mod fingerprint;
mod license_key;
mod trial;

pub use fingerprint::{display_fingerprint, machine_fingerprint};
pub use license_key::{generate_license_key, verify_license_key, LicenseInfo};
pub use trial::TrialPayload;

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// 免费试用次数。
pub const TRIAL_LAUNCHES: u32 = 3;

/// 授权状态。
#[derive(Debug, Clone)]
pub enum LicenseStatus {
    /// 试用中。`remaining` 从 3 递减到 0。
    Trial { remaining: u32 },
    /// 试用已到期或被篡改。
    Expired { reason: ExpireReason },
    /// 已激活。
    Licensed { permanent: bool, expires: Option<String> },
}

/// 过期原因。
#[derive(Debug, Clone, Copy)]
pub enum ExpireReason {
    /// 试用次数用完。
    Exhausted,
    /// 试用文件被篡改。
    Tampered,
    /// 试用文件来自另一台机器。
    MachineMismatch,
    /// 采集机器指纹失败。
    FingerprintError,
}

/// 检查当前授权状态。启动时调用一次。
pub fn check_status(fingerprint: &str) -> LicenseStatus {
    if fingerprint.is_empty() {
        return LicenseStatus::Expired {
            reason: ExpireReason::FingerprintError,
        };
    }

    if let Some(info) = load_license_file() {
        return LicenseStatus::Licensed {
            permanent: info.permanent,
            expires: info.expires,
        };
    }

    match trial::load_trial(fingerprint) {
        Ok(Some(payload)) => {
            if payload.n >= TRIAL_LAUNCHES {
                return LicenseStatus::Expired {
                    reason: ExpireReason::Exhausted,
                };
            }
            let remaining = TRIAL_LAUNCHES - payload.n;
            LicenseStatus::Trial { remaining }
        }
        Ok(None) => {
            // 首次启动，创建试用记录
            let now = now_timestamp();
            let payload = TrialPayload {
                v: 1,
                mid: fingerprint.to_string(),
                n: 1,
                ts0: now,
                tsn: now,
                tsm: now,
                nonce: random_nonce(),
            };
            if let Err(e) = trial::save_trial(fingerprint, &payload) {
                eprintln!("创建试用记录失败：{e}");
            }
            LicenseStatus::Trial {
                remaining: TRIAL_LAUNCHES - 1,
            }
        }
        Err(e) => {
            eprintln!("加载试用记录失败：{e}");
            LicenseStatus::Expired {
                reason: ExpireReason::Tampered,
            }
        }
    }
}

/// 记录一次启动。返回更新后的状态。
pub fn record_launch(fingerprint: &str) -> LicenseStatus {
    if fingerprint.is_empty() {
        return LicenseStatus::Expired {
            reason: ExpireReason::FingerprintError,
        };
    }

    if load_license_file().is_some() {
        return check_status(fingerprint);
    }

    let mut payload = match trial::load_trial(fingerprint) {
        Ok(Some(p)) => p,
        Ok(None) => {
            // 首次启动，创建试用记录
            let now = now_timestamp();
            let payload = TrialPayload {
                v: 1,
                mid: fingerprint.to_string(),
                n: 1,
                ts0: now,
                tsn: now,
                tsm: now,
                nonce: random_nonce(),
            };
            if let Err(e) = trial::save_trial(fingerprint, &payload) {
                eprintln!("创建试用记录失败：{e}");
            }
            return LicenseStatus::Trial {
                remaining: TRIAL_LAUNCHES - 1,
            };
        }
        Err(_) => {
            return LicenseStatus::Expired {
                reason: ExpireReason::Tampered,
            };
        }
    };

    let now = now_timestamp();
    payload.n += 1;
    payload.tsn = now;
    payload.tsm = payload.tsm.max(now);

    if let Err(e) = trial::save_trial(fingerprint, &payload) {
        eprintln!("保存试用记录失败：{e}");
    }

    if payload.n >= TRIAL_LAUNCHES {
        LicenseStatus::Expired {
            reason: ExpireReason::Exhausted,
        }
    } else {
        LicenseStatus::Trial {
            remaining: TRIAL_LAUNCHES - payload.n,
        }
    }
}

/// 尝试激活。成功返回新的状态。
pub fn activate_license(fingerprint: &str, key: &str) -> Result<LicenseStatus, String> {
    eprintln!("[activate_license] start, fingerprint len={}, key len={}", fingerprint.len(), key.len());
    if fingerprint.is_empty() {
        return Err("机器指纹为空".to_string());
    }

    let info = verify_license_key(fingerprint, key)?;
    eprintln!("[activate_license] verify ok, permanent={}", info.permanent);

    // 写入 license 文件
    let path = license_file_path()?;
    eprintln!("[activate_license] license path: {:?}", path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建数据目录失败：{e}"))?;
    }

    let content = serde_json::to_string(&info).map_err(|e| format!("序列化失败：{e}"))?;
    std::fs::write(&path, content).map_err(|e| format!("写 license.dat 失败：{e}"))?;
    eprintln!("[activate_license] license.dat written");

    Ok(LicenseStatus::Licensed {
        permanent: info.permanent,
        expires: info.expires,
    })
}

fn load_license_file() -> Option<LicenseInfo> {
    let path = license_file_path().ok()?;
    if !path.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn license_file_path() -> Result<PathBuf, String> {
    let dir = data_dir()?;
    Ok(dir.join("license.dat"))
}

fn data_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").map_err(|_| "找不到 HOME 环境变量")?;
        Ok(PathBuf::from(home).join("Library/Application Support/Cut"))
    }
    #[cfg(windows)]
    {
        let appdata = std::env::var("APPDATA").map_err(|_| "找不到 APPDATA 环境变量")?;
        Ok(PathBuf::from(appdata).join("Cut"))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Err("不支持的平台".to_string())
    }
}

fn now_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn random_nonce() -> [u8; 8] {
    let mut nonce = [0u8; 8];
    let now = now_timestamp();
    for (i, b) in nonce.iter_mut().enumerate() {
        *b = ((now >> (i * 8)) & 0xff) as u8;
    }
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn license_status_debug() {
        let status = LicenseStatus::Trial { remaining: 2 };
        let debug = format!("{status:?}");
        assert!(debug.contains("Trial"));
        assert!(debug.contains("2"));
    }

    #[test]
    fn expire_reason_debug() {
        let reason = ExpireReason::Exhausted;
        let debug = format!("{reason:?}");
        assert!(debug.contains("Exhausted"));
    }
}
