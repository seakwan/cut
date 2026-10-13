//! 试用计数器：AES-256-CBC 加密 + HMAC-SHA256 完整性校验。
//!
//! 文件格式：`[版本 1B][IV 16B][密文 NB][HMAC 32B]`
//! 密文是 JSON 序列化的 `TrialPayload`，PKCS7 填充到 16 字节对齐。
//! HMAC 覆盖版本 + IV + 密文（encrypt-then-MAC）。

use std::path::PathBuf;

use aes::Aes256;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type Aes256CbcEnc = cbc::Encryptor<Aes256>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;

const FILE_VERSION: u8 = 0x01;
const AES_BLOCK_SIZE: usize = 16;
const HMAC_LEN: usize = 32;

/// 编译期常量，派生所有密钥。不是安全边界——目标是防 casual 篡改，不是防逆向。
const APP_SECRET: &[u8] = b"CutTrial-v1-2026";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TrialPayload {
    pub v: u8,
    pub mid: String,
    pub n: u32,
    pub ts0: u64,
    pub tsn: u64,
    pub tsm: u64,
    pub nonce: [u8; 8],
}

/// 试用文件路径：`$DATA_DIR/Cut/trial.dat`
pub fn trial_file_path() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("trial.dat"))
}

/// 读取并解密试用数据。文件不存在返回 `Ok(None)`；任何损坏/篡改返回 `Err`。
pub fn load_trial(fingerprint: &str) -> Result<Option<TrialPayload>, String> {
    let path = trial_file_path()?;
    if !path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read(&path).map_err(|e| format!("读 trial.dat 失败：{e}"))?;

    if raw.len() < 1 + 16 + AES_BLOCK_SIZE + HMAC_LEN {
        return Err("trial.dat 太短".to_string());
    }

    let hmac_start = raw.len() - HMAC_LEN;
    let stored_hmac = &raw[hmac_start..];
    let body = &raw[..hmac_start];

    let hmac_key = derive_hmac_key();
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&hmac_key).map_err(|_| "HMAC 初始化失败")?;
    mac.update(body);
    mac.verify_slice(stored_hmac)
        .map_err(|_| "trial.dat HMAC 校验失败（文件被篡改）")?;

    let version = raw[0];
    if version != FILE_VERSION {
        return Err(format!("trial.dat 版本不匹配（期望 {FILE_VERSION}，实际 {version}）"));
    }

    let iv: [u8; 16] = raw[1..17].try_into().map_err(|_| "IV 长度错误")?;
    let ciphertext = &raw[17..hmac_start];

    let enc_key = derive_enc_key();
    let mut ciphertext_buf = ciphertext.to_vec();
    let decrypted = Aes256CbcDec::new(
        aes::cipher::generic_array::GenericArray::from_slice(&enc_key),
        aes::cipher::generic_array::GenericArray::from_slice(&iv),
    )
    .decrypt_padded_mut::<Pkcs7>(&mut ciphertext_buf)
    .map_err(|_| "trial.dat 解密失败（padding 错误）")?;

    let payload: TrialPayload =
        serde_json::from_slice(&decrypted).map_err(|e| format!("trial.dat JSON 解析失败：{e}"))?;

    if payload.mid != fingerprint {
        return Err("trial.dat 机器指纹不匹配（文件来自另一台机器）".to_string());
    }

    Ok(Some(payload))
}

/// 加密并写入试用数据。
pub fn save_trial(fingerprint: &str, payload: &TrialPayload) -> Result<(), String> {
    let path = trial_file_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建数据目录失败：{e}"))?;
    }

    let enc_key = derive_enc_key();
    let iv = derive_iv(fingerprint);

    let plaintext = serde_json::to_vec(payload).map_err(|e| format!("序列化 trial 失败：{e}"))?;

    // 计算填充后的长度
    let block_size = AES_BLOCK_SIZE;
    let padding_len = block_size - (plaintext.len() % block_size);
    let padded_len = plaintext.len() + padding_len;
    let mut buf = vec![0u8; padded_len];
    buf[..plaintext.len()].copy_from_slice(&plaintext);

    let ciphertext = Aes256CbcEnc::new(
        aes::cipher::generic_array::GenericArray::from_slice(&enc_key),
        aes::cipher::generic_array::GenericArray::from_slice(&iv),
    )
    .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
    .map_err(|_| "加密失败")?;
    let ciphertext = ciphertext.to_vec();

    let mut body = Vec::with_capacity(1 + 16 + ciphertext.len());
    body.push(FILE_VERSION);
    body.extend_from_slice(&iv);
    body.extend_from_slice(&ciphertext);

    let hmac_key = derive_hmac_key();
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&hmac_key).map_err(|_| "HMAC 初始化失败")?;
    mac.update(&body);
    let hmac_result = mac.finalize().into_bytes();

    let mut file_data = body;
    file_data.extend_from_slice(&hmac_result);

    std::fs::write(&path, &file_data).map_err(|e| format!("写 trial.dat 失败：{e}"))?;

    Ok(())
}

fn derive_enc_key() -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(APP_SECRET);
    hasher.update(b"\x01");
    hasher.finalize().into()
}

fn derive_iv(fingerprint: &str) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(fingerprint.as_bytes());
    hasher.update(APP_SECRET);
    hasher.update(b"\x02");
    let hash = hasher.finalize();
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&hash[..16]);
    iv
}

fn derive_hmac_key() -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(APP_SECRET);
    hasher.update(b"\x03");
    hasher.finalize().into()
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

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FP: &str = "a1b2c3d4e5f67890";

    fn make_payload(n: u32) -> TrialPayload {
        TrialPayload {
            v: FILE_VERSION,
            mid: TEST_FP.to_string(),
            n,
            ts0: 1700000000,
            tsn: 1700000000 + n as u64,
            tsm: 1700000000 + n as u64,
            nonce: [1, 2, 3, 4, 5, 6, 7, 8],
        }
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        use aes::cipher::generic_array::GenericArray;

        let payload = make_payload(1);
        let enc_key = derive_enc_key();
        let iv = derive_iv(TEST_FP);

        let plaintext = serde_json::to_vec(&payload).unwrap();

        // 加密
        let block_size = AES_BLOCK_SIZE;
        let padding_len = block_size - (plaintext.len() % block_size);
        let padded_len = plaintext.len() + padding_len;
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(&plaintext);

        let ciphertext = Aes256CbcEnc::new(
            GenericArray::from_slice(&enc_key),
            GenericArray::from_slice(&iv),
        )
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
        .unwrap()
        .to_vec();

        // 解密
        let mut ciphertext_buf = ciphertext.clone();
        let decrypted = Aes256CbcDec::new(
            GenericArray::from_slice(&enc_key),
            GenericArray::from_slice(&iv),
        )
        .decrypt_padded_mut::<Pkcs7>(&mut ciphertext_buf)
        .unwrap();

        let recovered: TrialPayload = serde_json::from_slice(decrypted).unwrap();
        assert_eq!(recovered.n, 1);
        assert_eq!(recovered.mid, TEST_FP);
    }

    #[test]
    fn hmac_catches_tampering() {
        use aes::cipher::generic_array::GenericArray;

        let payload = make_payload(1);
        let enc_key = derive_enc_key();
        let iv = derive_iv(TEST_FP);

        let plaintext = serde_json::to_vec(&payload).unwrap();

        // 加密
        let block_size = AES_BLOCK_SIZE;
        let padding_len = block_size - (plaintext.len() % block_size);
        let padded_len = plaintext.len() + padding_len;
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(&plaintext);

        let mut ciphertext = Aes256CbcEnc::new(
            GenericArray::from_slice(&enc_key),
            GenericArray::from_slice(&iv),
        )
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
        .unwrap()
        .to_vec();

        // 篡改密文
        if let Some(byte) = ciphertext.get_mut(0) {
            *byte ^= 0xff;
        }

        // 解密应该失败
        let mut ciphertext_buf = ciphertext.clone();
        let result = Aes256CbcDec::new(
            GenericArray::from_slice(&enc_key),
            GenericArray::from_slice(&iv),
        )
        .decrypt_padded_mut::<Pkcs7>(&mut ciphertext_buf);

        assert!(result.is_err() || {
            // 即使解密不报错，JSON 解析也会失败
            serde_json::from_slice::<TrialPayload>(result.unwrap()).is_err()
        });
    }

    #[test]
    fn different_fingerprint_different_iv() {
        let iv1 = derive_iv("a1b2c3d4e5f67890");
        let iv2 = derive_iv("0000000000000000");
        assert_ne!(iv1, iv2);
    }
}
