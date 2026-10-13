//! 激活码格式与验证：Crockford Base32 编码 + HMAC-SHA256 签名。
//!
//! 格式：`CUT-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX`
//! 40 字符 = 200 bit = 25 字节载荷。
//!
//! 载荷结构：
//! ```text
//! [版本 1B][机器指纹前缀 4B][有效期 2B][标志 1B][随机盐 9B][HMAC 截断 8B]
//! ```

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// Crockford Base32 字母表（无 I/L/O/U，避免混淆）。
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 签名密钥（32 字节）。编译进二进制，app 和 keygen 工具共享。
/// **生产环境请替换为随机生成的密钥**。
const SIGNING_KEY: [u8; 32] = [
    0x43, 0x75, 0x74, 0x4c, 0x69, 0x63, 0x65, 0x6e,
    0x73, 0x65, 0x4b, 0x65, 0x79, 0x56, 0x31, 0x2d,
    0x32, 0x30, 0x32, 0x36, 0x2d, 0x52, 0x65, 0x70,
    0x6c, 0x61, 0x63, 0x65, 0x4d, 0x65, 0x21, 0x21,
];

const KEY_VERSION: u8 = 0x01;
const EPOCH_DAYS: u64 = 19723; // 2024-01-01 距 1970-01-01 的天数

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LicenseInfo {
    pub permanent: bool,
    pub expires: Option<String>,
}

/// 生成激活码。`expires_days` 为 `None` 表示永久，`Some(n)` 表示从 2024-01-01 起 n 天后过期。
pub fn generate_license_key(fingerprint: &str, expires_days: Option<u16>) -> String {
    let fp_hash = Sha256::digest(fingerprint.as_bytes());
    let machine_prefix: [u8; 4] = fp_hash[..4].try_into().unwrap();

    let expires = expires_days.unwrap_or(0);
    let flags: u8 = 0x00;

    let mut nonce = [0u8; 9];
    for (i, b) in nonce.iter_mut().enumerate() {
        *b = ((i as u64 + 1) * 0x5a) as u8;
    }

    let mut payload = [0u8; 25];
    payload[0] = KEY_VERSION;
    payload[1..5].copy_from_slice(&machine_prefix);
    payload[5..7].copy_from_slice(&expires.to_le_bytes());
    payload[7] = flags;
    payload[8..17].copy_from_slice(&nonce);

    let mut mac = Hmac::<Sha256>::new_from_slice(&SIGNING_KEY).unwrap();
    mac.update(&payload[..17]);
    let tag = mac.finalize().into_bytes();
    payload[17..25].copy_from_slice(&tag[..8]);

    let encoded = base32_encode(&payload);
    format_key(&encoded)
}

/// 验证激活码。返回 `Ok(LicenseInfo)` 表示有效，`Err` 表示无效或过期。
pub fn verify_license_key(fingerprint: &str, key: &str) -> Result<LicenseInfo, String> {
    let stripped = key.trim();
    if !stripped.to_uppercase().starts_with("CUT-") {
        return Err("激活码格式错误（应以 CUT- 开头）".to_string());
    }

    let raw: String = stripped[4..]
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '-')
        .collect();

    if raw.len() != 40 {
        return Err(format!("激活码长度错误（期望 40 字符，实际 {}）", raw.len()));
    }

    let payload = base32_decode(&raw.to_uppercase())?;

    let mut mac = Hmac::<Sha256>::new_from_slice(&SIGNING_KEY)
        .map_err(|e| format!("HMAC 初始化失败：{e}"))?;
    mac.update(&payload[..17]);
    let expected_tag = mac.finalize().into_bytes();

    if !constant_time_eq(&payload[17..25], &expected_tag[..8]) {
        return Err("激活码签名无效".to_string());
    }

    let version = payload[0];
    if version != KEY_VERSION {
        return Err(format!("激活码版本不匹配（期望 {KEY_VERSION}，实际 {version}）"));
    }

    let fp_hash = Sha256::digest(fingerprint.as_bytes());
    let machine_prefix: [u8; 4] = fp_hash[..4]
        .try_into()
        .map_err(|_| "内部错误：哈希前缀长度异常")?;
    if payload[1..5] != machine_prefix {
        return Err("激活码与本机不匹配".to_string());
    }

    let expires_days = u16::from_le_bytes(
        payload[5..7]
            .try_into()
            .map_err(|_| "内部错误：有效期字段长度异常")?,
    );
    let permanent = expires_days == 0;

    let expires = if permanent {
        None
    } else {
        let days_since_epoch = EPOCH_DAYS + expires_days as u64;
        Some(days_to_date(days_since_epoch))
    };

    if !permanent {
        let expires_days_abs = EPOCH_DAYS + expires_days as u64;
        let now_days = now_days_since_epoch();
        if now_days > expires_days_abs {
            let date = expires.unwrap_or_default();
            return Err(format!("激活码已过期（到期日：{date}）"));
        }
    }

    Ok(LicenseInfo { permanent, expires })
}

fn base32_encode(data: &[u8; 25]) -> String {
    let mut result = String::with_capacity(40);
    let mut buffer = 0u64;
    let mut bits_in_buffer = 0;

    for &byte in data.iter() {
        buffer = (buffer << 8) | byte as u64;
        bits_in_buffer += 8;

        while bits_in_buffer >= 5 {
            bits_in_buffer -= 5;
            let idx = ((buffer >> bits_in_buffer) & 0x1f) as usize;
            result.push(ALPHABET[idx] as char);
        }
    }

    // 处理剩余的位
    if bits_in_buffer > 0 {
        let idx = ((buffer << (5 - bits_in_buffer)) & 0x1f) as usize;
        result.push(ALPHABET[idx] as char);
    }

    result
}

fn base32_decode(encoded: &str) -> Result<[u8; 25], String> {
    let mut result = [0u8; 25];
    let mut buffer = 0u64;
    let mut bits_in_buffer = 0;
    let mut byte_index = 0;

    for c in encoded.chars() {
        let val = ALPHABET
            .iter()
            .position(|&a| a as char == c)
            .ok_or_else(|| format!("无效字符：{c}"))?;

        buffer = (buffer << 5) | val as u64;
        bits_in_buffer += 5;

        if bits_in_buffer >= 8 {
            bits_in_buffer -= 8;
            if byte_index < 25 {
                result[byte_index] = ((buffer >> bits_in_buffer) & 0xff) as u8;
                byte_index += 1;
            }
        }
    }

    if byte_index != 25 {
        return Err(format!("解码后长度错误（期望 25 字节，实际 {byte_index}）"));
    }

    Ok(result)
}

fn format_key(raw: &str) -> String {
    let mut result = String::from("CUT-");
    for (i, c) in raw.chars().enumerate() {
        if i > 0 && i % 5 == 0 {
            result.push('-');
        }
        result.push(c);
    }
    result
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn now_days_since_epoch() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    secs / 86400
}

fn days_to_date(days: u64) -> String {
    let days_in_year = |y: u64| -> u64 {
        if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        }
    };

    let mut year = 1970;
    let mut remaining = days;
    loop {
        let dy = days_in_year(year);
        if remaining < dy {
            break;
        }
        remaining -= dy;
        year += 1;
    }

    let is_leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let feb = if is_leap { 29 } else { 28 };
    let month_days = [31, feb, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

    let mut month = 0;
    for (i, &d) in month_days.iter().enumerate() {
        if remaining < d {
            month = i + 1;
            break;
        }
        remaining -= d;
    }

    let day = remaining + 1;
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FP: &str = "a1b2c3d4e5f67890";

    #[test]
    fn generate_and_verify_permanent() {
        let key = generate_license_key(TEST_FP, None);
        assert!(key.starts_with("CUT-"));

        let info = verify_license_key(TEST_FP, &key).unwrap();
        assert!(info.permanent);
        assert!(info.expires.is_none());
    }

    #[test]
    fn generate_and_verify_with_expiry() {
        // 用 2000 天（约 5.5 年），确保测试时未过期
        let key = generate_license_key(TEST_FP, Some(2000));
        let info = verify_license_key(TEST_FP, &key).unwrap();
        assert!(!info.permanent);
        assert!(info.expires.is_some());
    }

    #[test]
    fn wrong_fingerprint_rejected() {
        let key = generate_license_key(TEST_FP, None);
        let result = verify_license_key("0000000000000000", &key);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("不匹配"));
    }

    #[test]
    fn tampered_key_rejected() {
        let key = generate_license_key(TEST_FP, None);
        let chars: Vec<char> = key.chars().collect();
        let mut modified = chars.clone();
        if let Some(pos) = modified.iter().position(|c| c.is_ascii_alphanumeric() && *c != '0') {
            modified[pos] = '0';
        }
        let modified_key: String = modified.into_iter().collect();
        if modified_key != key {
            assert!(verify_license_key(TEST_FP, &modified_key).is_err());
        }
    }

    #[test]
    fn base32_roundtrip() {
        let data = [0x42u8; 25];
        let encoded = base32_encode(&data);
        let decoded = base32_decode(&encoded).unwrap();
        assert_eq!(data, decoded);
    }

    #[test]
    fn days_to_date_known_values() {
        assert_eq!(days_to_date(0), "1970-01-01");
        assert_eq!(days_to_date(1), "1970-01-02");
        assert_eq!(days_to_date(365), "1971-01-01");
        assert_eq!(days_to_date(366), "1971-01-02");
    }
}
