use crate::error::{LauncherError, LauncherResult};
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;

pub const SHARE_KEY_PREFIX: &str = "TLPACK1:";
const MAX_MODPACK_NAME_CHARS: usize = 80;
const MAX_DECOMPRESSED_SHARE_BYTES: u64 = 512 * 1024;
const INVALID_FILENAME_CHARS: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];
const WINDOWS_RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "k")]
pub enum SharedModSource {
    #[serde(rename = "w")]
    Workshop { id: String },
    #[serde(rename = "g")]
    Github {
        repo: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        url: String,
    },
    #[serde(rename = "u")]
    Url { url: String },
    #[serde(rename = "m")]
    ManualOnly,
}

fn default_true() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedModEntry {
    #[serde(rename = "n")]
    pub internal_name: String,
    #[serde(rename = "d", default, skip_serializing_if = "String::is_empty")]
    pub display_name: String,
    #[serde(rename = "v", default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    #[serde(rename = "s")]
    pub source: SharedModSource,
    #[serde(rename = "e", default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModPackShareKeyPayload {
    pub v: u8,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mods: Vec<SharedModEntry>,
}

pub(crate) fn is_safe_modpack_component(trimmed: &str) -> bool {
    !trimmed.is_empty()
        && !trimmed.contains("..")
        && !trimmed.starts_with('.')
        && !trimmed.ends_with('.')
        && !trimmed
            .chars()
            .any(|c| INVALID_FILENAME_CHARS.contains(&c) || c.is_control())
        && !is_windows_reserved_name(trimmed)
}

pub fn sanitize_modpack_name(raw: &str) -> LauncherResult<String> {
    let trimmed = raw.trim();
    if !is_safe_modpack_component(trimmed) {
        return Err(LauncherError::InvalidInput(
            "Tên ModPack không hợp lệ".to_string(),
        ));
    }

    let capped = match trimmed.char_indices().nth(MAX_MODPACK_NAME_CHARS) {
        Some((idx, _)) => &trimmed[..idx],
        None => trimmed,
    };
    let clean = capped.trim_end_matches(|c: char| c.is_whitespace() || c == '.');
    if !is_safe_modpack_component(clean) {
        return Err(LauncherError::InvalidInput(
            "Tên ModPack không hợp lệ".to_string(),
        ));
    }

    Ok(clean.to_string())
}

pub fn resolve_unique_modpack_name(modpacks_dir: &Path, desired: &str) -> LauncherResult<String> {
    let base = sanitize_modpack_name(desired)?;
    if !modpacks_dir.join(&base).exists() {
        return Ok(base);
    }

    for i in 2..=999 {
        let candidate = format!("{base}_{i}");
        if !modpacks_dir.join(&candidate).exists() {
            return Ok(candidate);
        }
    }

    Err(LauncherError::InvalidInput(format!(
        "Không thể tạo tên ModPack duy nhất cho '{base}'"
    )))
}

pub fn extract_source_hint_from_homepage(homepage: &str) -> Option<SharedModSource> {
    let trimmed = homepage.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (scheme, after_scheme) = split_http_scheme(trimmed)?;
    let (host, rest) = split_host_and_rest(after_scheme);
    let host_no_port = host.split(':').next().unwrap_or(host);
    if host_no_port.is_empty() {
        return None;
    }

    let path_and_query = rest.split('#').next().unwrap_or(rest);
    let (path, query) = path_and_query.split_once('?').unwrap_or((path_and_query, ""));

    if is_steamcommunity_host(host_no_port)
        && contains_ascii_ci(path, "sharedfiles")
        && !query.is_empty()
    {
        if let Some(id) = extract_numeric_query_param(query, "id") {
            return Some(SharedModSource::Workshop { id });
        }
    }

    if scheme.eq_ignore_ascii_case("https") && host_no_port.eq_ignore_ascii_case("github.com") {
        let trimmed_path = path.trim_matches('/');
        if !trimmed_path.to_ascii_lowercase().ends_with(".tmod") {
            if let Some((owner, repo)) = trimmed_path.split_once('/') {
                if !repo.contains('/')
                    && is_valid_github_segment(owner)
                    && is_valid_github_segment(repo)
                {
                    return Some(SharedModSource::Github {
                        repo: format!("{owner}/{repo}"),
                        url: String::new(),
                    });
                }
            }
        }
    }

    if !path.is_empty() && path.to_ascii_lowercase().ends_with(".tmod") {
        return Some(SharedModSource::Url {
            url: trimmed.to_string(),
        });
    }

    None
}

pub fn validate_share_payload(payload: &ModPackShareKeyPayload) -> LauncherResult<()> {
    if payload.v != 1 {
        return Err(LauncherError::InvalidInput(format!(
            "Phiên bản Share Key không được hỗ trợ: v{}",
            payload.v
        )));
    }

    let _ = sanitize_modpack_name(&payload.name)?;

    if payload.mods.is_empty() {
        return Err(LauncherError::InvalidInput(
            "ModPack Share Key không chứa mod nào".to_string(),
        ));
    }

    for entry in &payload.mods {
        validate_mod_entry(entry)?;
    }

    Ok(())
}

pub fn encode_modpack_share_key(payload: &ModPackShareKeyPayload) -> LauncherResult<String> {
    validate_share_payload(payload)?;

    let json = serde_json::to_vec(payload)?;
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&json)?;
    let mut packed = encoder.finish()?;

    let checksum = Sha256::digest(&packed);
    packed.extend_from_slice(&checksum[..4]);

    let encoded = base64url_encode(&packed);
    Ok(format!("{SHARE_KEY_PREFIX}{encoded}"))
}

pub fn decode_modpack_share_key(raw: &str) -> LauncherResult<ModPackShareKeyPayload> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let body = cleaned.strip_prefix(SHARE_KEY_PREFIX).ok_or_else(|| {
        LauncherError::InvalidFormat("Share Key không đúng tiền tố TLPACK1:".to_string())
    })?;

    let bytes = base64url_decode(body)?;
    if bytes.len() < 5 {
        return Err(LauncherError::InvalidFormat(
            "Share Key quá ngắn hoặc bị cắt cụt".to_string(),
        ));
    }

    let (compressed, checksum) = bytes.split_at(bytes.len() - 4);
    let expected = Sha256::digest(compressed);
    if checksum != &expected[..4] {
        return Err(LauncherError::InvalidFormat(
            "Share Key bị hỏng hoặc sai checksum".to_string(),
        ));
    }

    let mut limited = DeflateDecoder::new(compressed).take(MAX_DECOMPRESSED_SHARE_BYTES + 1);
    let mut json_bytes = Vec::new();
    limited.read_to_end(&mut json_bytes).map_err(|e| {
        LauncherError::InvalidFormat(format!("Không giải nén được Share Key: {e}"))
    })?;
    if (json_bytes.len() as u64) > MAX_DECOMPRESSED_SHARE_BYTES {
        return Err(LauncherError::InvalidFormat(
            "Dữ liệu Share Key vượt quá giới hạn giải nén cho phép".to_string(),
        ));
    }

    let payload: ModPackShareKeyPayload = serde_json::from_slice(&json_bytes)?;
    validate_share_payload(&payload)?;
    Ok(payload)
}

fn validate_mod_entry(entry: &SharedModEntry) -> LauncherResult<()> {
    let internal = entry.internal_name.trim();
    if internal.is_empty()
        || internal.contains(['/', '\\'])
        || internal.contains("..")
        || internal.chars().any(char::is_control)
    {
        return Err(LauncherError::InvalidInput(
            "Tên nội bộ của mod trong Share Key không hợp lệ".to_string(),
        ));
    }

    match &entry.source {
        SharedModSource::Workshop { id } => {
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
                return Err(LauncherError::InvalidInput(format!(
                    "Workshop ID không hợp lệ cho mod '{internal}'"
                )));
            }
        }
        SharedModSource::Github { repo, url } => {
            if !is_valid_github_repo(repo) {
                return Err(LauncherError::InvalidInput(format!(
                    "Repo GitHub không hợp lệ cho mod '{internal}'"
                )));
            }
            if !url.is_empty() && !is_valid_http_url(url) {
                return Err(LauncherError::InvalidInput(format!(
                    "URL phát hành GitHub không hợp lệ cho mod '{internal}'"
                )));
            }
        }
        SharedModSource::Url { url } => {
            if !is_valid_http_url(url) {
                return Err(LauncherError::InvalidInput(format!(
                    "URL tải mod không hợp lệ cho mod '{internal}'"
                )));
            }
        }
        SharedModSource::ManualOnly => {}
    }

    Ok(())
}

fn is_windows_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    WINDOWS_RESERVED_NAMES
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
}

fn split_http_scheme(url: &str) -> Option<(&str, &str)> {
    if url.len() >= 8 && url[..8].eq_ignore_ascii_case("https://") {
        Some((&url[..5], &url[8..]))
    } else if url.len() >= 7 && url[..7].eq_ignore_ascii_case("http://") {
        Some((&url[..4], &url[7..]))
    } else {
        None
    }
}

fn split_host_and_rest(after_scheme: &str) -> (&str, &str) {
    match after_scheme.find(['/', '?', '#']) {
        Some(idx) => (&after_scheme[..idx], &after_scheme[idx..]),
        None => (after_scheme, ""),
    }
}

fn is_steamcommunity_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("steamcommunity.com")
        || host.eq_ignore_ascii_case("www.steamcommunity.com")
}

fn contains_ascii_ci(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn extract_numeric_query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key && !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn is_valid_github_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg != "."
        && seg != ".."
        && !seg.contains("..")
        && seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn is_valid_github_repo(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    !name.contains('/') && is_valid_github_segment(owner) && is_valid_github_segment(name)
}

fn is_valid_http_url(url: &str) -> bool {
    if url.chars().any(char::is_control) {
        return false;
    }
    let Some((_, after_scheme)) = split_http_scheme(url) else {
        return false;
    };
    let (host, _) = split_host_and_rest(after_scheme);
    !host.is_empty()
}

pub(crate) fn base64url_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);

    for chunk in input.chunks(3) {
        match chunk {
            [b0, b1, b2] => {
                let n = ((u32::from(*b0)) << 16) | ((u32::from(*b1)) << 8) | u32::from(*b2);
                out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
                out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
                out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
                out.push(TABLE[(n & 0x3F) as usize] as char);
            }
            [b0, b1] => {
                let n = ((u32::from(*b0)) << 16) | ((u32::from(*b1)) << 8);
                out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
                out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
                out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
            }
            [b0] => {
                let n = (u32::from(*b0)) << 16;
                out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
                out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
            }
            _ => {}
        }
    }

    out
}

fn base64url_decode_char(b: u8) -> LauncherResult<u8> {
    match b {
        b'A'..=b'Z' => Ok(b - b'A'),
        b'a'..=b'z' => Ok(b - b'a' + 26),
        b'0'..=b'9' => Ok(b - b'0' + 52),
        b'-' => Ok(62),
        b'_' => Ok(63),
        _ => Err(LauncherError::InvalidFormat(
            "Share Key chứa ký tự Base64URL không hợp lệ".to_string(),
        )),
    }
}

fn base64url_decode(input: &str) -> LauncherResult<Vec<u8>> {
    let bytes = input.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 == 1 {
        return Err(LauncherError::InvalidFormat(
            "Độ dài chuỗi Base64URL của Share Key không hợp lệ".to_string(),
        ));
    }

    let mut out = Vec::with_capacity((bytes.len() * 3) / 4);
    for chunk in bytes.chunks(4) {
        match chunk {
            [c0, c1, c2, c3] => {
                let v0 = base64url_decode_char(*c0)?;
                let v1 = base64url_decode_char(*c1)?;
                let v2 = base64url_decode_char(*c2)?;
                let v3 = base64url_decode_char(*c3)?;
                out.push((v0 << 2) | (v1 >> 4));
                out.push(((v1 & 0x0F) << 4) | (v2 >> 2));
                out.push(((v2 & 0x03) << 6) | v3);
            }
            [c0, c1, c2] => {
                let v0 = base64url_decode_char(*c0)?;
                let v1 = base64url_decode_char(*c1)?;
                let v2 = base64url_decode_char(*c2)?;
                if (v2 & 0x03) != 0 {
                    return Err(LauncherError::InvalidFormat(
                        "Share Key Base64URL có bit đệm không hợp lệ".to_string(),
                    ));
                }
                out.push((v0 << 2) | (v1 >> 4));
                out.push(((v1 & 0x0F) << 4) | (v2 >> 2));
            }
            [c0, c1] => {
                let v0 = base64url_decode_char(*c0)?;
                let v1 = base64url_decode_char(*c1)?;
                if (v1 & 0x0F) != 0 {
                    return Err(LauncherError::InvalidFormat(
                        "Share Key Base64URL có bit đệm không hợp lệ".to_string(),
                    ));
                }
                out.push((v0 << 2) | (v1 >> 4));
            }
            _ => unreachable!("chunk size constrained by len % 4 != 1"),
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_mod_entry_enabled_flag_omits_true_and_preserves_false() {
        let mixed_payload = ModPackShareKeyPayload {
            v: 1,
            name: "MixedPack".to_string(),
            mods: vec![
                SharedModEntry {
                    internal_name: "EnabledMod".to_string(),
                    display_name: "Enabled Mod".to_string(),
                    version: "1.0.0".to_string(),
                    source: SharedModSource::Workshop {
                        id: "123456".to_string(),
                    },
                    enabled: true,
                },
                SharedModEntry {
                    internal_name: "DisabledMod".to_string(),
                    display_name: String::new(),
                    version: "2.0.0".to_string(),
                    source: SharedModSource::ManualOnly,
                    enabled: false,
                },
            ],
        };

        let json = serde_json::to_string(&mixed_payload).unwrap();
        assert!(!json.contains(r#""e":true"#));
        assert!(json.contains(r#""e":false"#));

        let legacy_json =
            r#"{"v":1,"name":"LegacyPack","mods":[{"n":"LegacyMod","s":{"k":"m"}}]}"#;
        let legacy_parsed: ModPackShareKeyPayload = serde_json::from_str(legacy_json).unwrap();
        assert!(legacy_parsed.mods[0].enabled);

        let mixed_key = encode_modpack_share_key(&mixed_payload).unwrap();
        let decoded_mixed = decode_modpack_share_key(&mixed_key).unwrap();
        assert_eq!(decoded_mixed, mixed_payload);
        assert!(decoded_mixed.mods[0].enabled);
        assert!(!decoded_mixed.mods[1].enabled);

        let all_disabled = ModPackShareKeyPayload {
            v: 1,
            name: "AllDisabled".to_string(),
            mods: vec![
                SharedModEntry {
                    internal_name: "ModOne".to_string(),
                    display_name: String::new(),
                    version: String::new(),
                    source: SharedModSource::ManualOnly,
                    enabled: false,
                },
                SharedModEntry {
                    internal_name: "ModTwo".to_string(),
                    display_name: String::new(),
                    version: "1.2.3".to_string(),
                    source: SharedModSource::Url {
                        url: "https://example.com/ModTwo.tmod".to_string(),
                    },
                    enabled: false,
                },
            ],
        };
        let disabled_key = encode_modpack_share_key(&all_disabled).unwrap();
        let decoded_disabled = decode_modpack_share_key(&disabled_key).unwrap();
        assert_eq!(decoded_disabled, all_disabled);
        assert!(decoded_disabled.mods.iter().all(|m| !m.enabled));
    }
}
