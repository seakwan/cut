//! 机器指纹：采集平台唯一标识，哈希成 16 位 hex 字符串。
//!
//! macOS 用 IOPlatformUUID（烧在主板 IOKit 注册表，换硬盘不变，换主板才变）。
//! Windows 用 MachineGuid（装系统时生成，重装才变）。

use sha2::{Digest, Sha256};

/// 返回 16 字符 hex 机器指纹，或错误信息。
pub fn machine_fingerprint() -> Result<String, String> {
    let raw = platform_identifier()?;
    let hash = Sha256::digest(raw.as_bytes());
    Ok(hex_encode(&hash[..8]))
}

/// 把指纹格式化成 `XXXXXXXX-XXXXXXXX` 方便用户复制发送。
pub fn display_fingerprint(fingerprint: &str) -> String {
    if fingerprint.len() == 16 {
        format!("{}-{}", &fingerprint[..8], &fingerprint[8..])
    } else {
        fingerprint.to_string()
    }
}

#[cfg(target_os = "macos")]
fn platform_identifier() -> Result<String, String> {
    use std::process::Command;

    let output = Command::new("ioreg")
        .args(["-d", "2", "-k", "IOPlatformUUID"])
        .output()
        .map_err(|e| format!("ioreg 执行失败：{e}"))?;

    if !output.status.success() {
        return Err("ioreg 返回非零状态".to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(pos) = line.find("IOPlatformUUID") {
            let rest = &line[pos..];
            if let Some(eq) = rest.find('=') {
                let val = rest[eq + 1..].trim();
                let val = val.trim_matches('"');
                if !val.is_empty() {
                    return Ok(val.to_string());
                }
            }
        }
    }

    Err("ioreg 输出中找不到 IOPlatformUUID".to_string())
}

#[cfg(windows)]
fn platform_identifier() -> Result<String, String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
        REG_SZ,
    };

    let subkey: Vec<u16> = OsStr::new("SOFTWARE\\Microsoft\\Cryptography")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let value_name: Vec<u16> = OsStr::new("MachineGuid")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        let ret = RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            0,
            KEY_READ,
            &mut hkey,
        );
        if ret != 0 {
            return Err(format!("打开注册表键失败（错误码 {ret}）"));
        }

        let mut buf_type = 0u32;
        let mut buf_len = 256u32;
        let mut buf = vec![0u8; buf_len as usize];

        let ret = RegQueryValueExW(
            hkey,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut buf_type,
            buf.as_mut_ptr(),
            &mut buf_len,
        );
        RegCloseKey(hkey);

        if ret != 0 {
            return Err(format!("读取 MachineGuid 失败（错误码 {ret}）"));
        }

        if buf_type != REG_SZ {
            return Err(format!("MachineGuid 类型不是 REG_SZ（实际 {buf_type}）"));
        }

        let wide: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        let guid = String::from_utf16_lossy(&wide);

        if guid.is_empty() {
            return Err("MachineGuid 为空".to_string());
        }

        Ok(guid)
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_identifier() -> Result<String, String> {
    Err("不支持的平台".to_string())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_fingerprint_format() {
        assert_eq!(
            display_fingerprint("a1b2c3d4e5f67890"),
            "a1b2c3d4-e5f67890"
        );
    }

    #[test]
    fn hex_encode_correct() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0x0a]), "00ff0a");
    }

    #[test]
    fn fingerprint_is_16_chars() {
        let fp = machine_fingerprint().expect("应能采集指纹");
        assert_eq!(fp.len(), 16);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
