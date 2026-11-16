use crate::game_runner::install_xna_framework_elevated;
use crate::http_client::{
    build_http_client, build_http_client_with_options, ensure_rustls_provider,
    normalize_proxy_url, GITHUB_API_VERSION, LAUNCHER_USER_AGENT,
};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tl_core::paths::{detect_dotnet8_runtime, detect_exact_bundled_dotnet_runtime, find_steamcmd};
use tl_core::{LauncherError, LauncherPaths, LauncherResult};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use zip::ZipArchive;

pub const TMODLOADER_GITHUB_REPO: &str = "tModLoader/tModLoader";
pub const XNA_40_OFFICIAL_MSI_URL: &str =
    "https://download.microsoft.com/download/a/c/2/ac2c903b-e6e8-42c2-9fd7-bebac362a930/xnafx40_redist.msi";
pub const STEAMCMD_OFFICIAL_ZIP_URL: &str =
    "https://steamcdn-a.akamaihd.net/client/installer/steamcmd.zip";
pub const TERRARIA_MANIFEST_FILENAME: &str = "terraria-manifest.json";

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TERRARIA_RECEIPT_FILENAME: &str = "terraria-installed.json";
const DOWNLOAD_PROGRESS_STEP_BYTES: u64 = 64 * 1024;
const PRESERVED_TOP_DIRS: &[&str] = &[
    "content",
    "libraries",
    "launchutils",
    "dedicatedserverutils",
    "tmodporter",
    "__redist",
    "dotnet",
    "host",
    "shared",
    "packs",
    "custom content",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallTarget {
    Terraria,
    TModLoader,
    DotNet8,
    Xna40,
    SteamCmd,
}

impl InstallTarget {
    pub fn label(self) -> &'static str {
        match self {
            Self::Terraria => "Terraria",
            Self::TModLoader => "tModLoader",
            Self::DotNet8 => ".NET 8 Runtime",
            Self::Xna40 => "XNA Framework 4.0",
            Self::SteamCmd => "SteamCMD",
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallProgress {
    pub target: InstallTarget,
    pub stage: String,
    pub current: u64,
    pub total: Option<u64>,
}

impl InstallProgress {
    pub fn fraction(&self) -> Option<f32> {
        self.total.and_then(|t| {
            if t > 0 {
                Some((self.current as f32 / t as f32).clamp(0.0, 1.0))
            } else {
                None
            }
        })
    }
}

pub type ProgressCallback = Arc<dyn Fn(InstallProgress) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct TmodLoaderReleaseInfo {
    pub tag_name: String,
    pub name: String,
    pub prerelease: bool,
    pub published_at: String,
    pub asset_name: String,
    pub download_url: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerrariaManifestPart {
    pub file: String,
    pub size: u64,
    #[serde(default)]
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerrariaBranchManifest {
    pub version: String,
    #[serde(default)]
    pub archive_name: String,
    pub total_size: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub chunk_size: u64,
    pub parts: Vec<TerrariaManifestPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct InstalledTerrariaReceipt {
    pub version: String,
    pub sha256: String,
    pub source: String,
    #[serde(default)]
    pub install_dir: String,
}

struct TempFileGuard {
    path: Option<PathBuf>,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if let Some(ref p) = self.path {
            let _ = fs::remove_file(p);
        }
    }
}

fn emit_progress(
    on_progress: &ProgressCallback,
    target: InstallTarget,
    stage: impl Into<String>,
    current: u64,
    total: Option<u64>,
) {
    on_progress(InstallProgress {
        target,
        stage: stage.into(),
        current,
        total,
    });
}

fn build_download_client_with_mode(
    proxy_url: Option<&str>,
    bypass_system_proxy: bool,
) -> LauncherResult<Client> {
    ensure_rustls_provider();

    let mut builder = Client::builder()
        .user_agent(LAUNCHER_USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60));

    if let Some(proxy_str) = proxy_url.and_then(normalize_proxy_url) {
        let proxy = reqwest::Proxy::all(&proxy_str)
            .map_err(|e| LauncherError::InvalidInput(format!("Proxy không hợp lệ: {e}")))?;
        builder = builder.proxy(proxy);
    } else if bypass_system_proxy {
        builder = builder.no_proxy();
    }

    builder
        .build()
        .map_err(|e| LauncherError::Network(format!("Không thể khởi tạo HTTP client: {e}")))
}

fn build_download_client(proxy_url: Option<&str>) -> LauncherResult<Client> {
    build_download_client_with_mode(proxy_url, false)
}

#[derive(Clone, Copy)]
struct DownloadSpec<'a> {
    url: &'a str,
    dest_part: &'a Path,
    target: InstallTarget,
    stage: &'a str,
    fallback_total: Option<u64>,
    accept_header: Option<&'a str>,
    bearer_token: Option<&'a str>,
}

fn should_retry_without_system_proxy(err: &LauncherError) -> bool {
    let LauncherError::Network(msg) = err else {
        return false;
    };
    msg.starts_with("Lỗi kết nối")
        || msg.starts_with("Lỗi đọc dữ liệu")
        || [
            "HTTP 403",
            "HTTP 407",
            "HTTP 429",
            "HTTP 502",
            "HTTP 503",
            "HTTP 504",
        ]
        .iter()
        .any(|code| msg.contains(code))
}

async fn download_to_part_file(
    client: &Client,
    spec: DownloadSpec<'_>,
    on_progress: &ProgressCallback,
) -> LauncherResult<()> {
    if let Some(parent) = spec.dest_part.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut req = client.get(spec.url);
    if let Some(accept) = spec.accept_header {
        req = req.header("Accept", accept);
    }
    if let Some(token) = spec.bearer_token {
        req = req.bearer_auth(token);
    }

    let resp = req.send().await.map_err(|e| {
        LauncherError::Network(format!(
            "Lỗi kết nối khi tải {}: {}",
            spec.target.label(),
            format_reqwest_error_chain(&e)
        ))
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let host = resp.url().host_str().unwrap_or("unknown");
        return Err(LauncherError::Network(format!(
            "Máy chủ {host} trả về HTTP {status} khi tải {}",
            spec.target.label()
        )));
    }

    let total_bytes = resp
        .content_length()
        .filter(|&n| n > 0)
        .or_else(|| spec.fallback_total.filter(|&n| n > 0));

    emit_progress(on_progress, spec.target, spec.stage, 0, total_bytes);

    let mut file = tokio::fs::File::create(spec.dest_part).await?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut last_reported: u64 = 0;

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.map_err(|e| {
            LauncherError::Network(format!(
                "Lỗi đọc dữ liệu khi tải {}: {}",
                spec.target.label(),
                format_reqwest_error_chain(&e)
            ))
        })?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;

        let reached_total = total_bytes.is_some_and(|t| downloaded >= t);
        if downloaded.saturating_sub(last_reported) >= DOWNLOAD_PROGRESS_STEP_BYTES || reached_total {
            last_reported = downloaded;
            emit_progress(on_progress, spec.target, spec.stage, downloaded, total_bytes);
        }
    }

    file.flush().await?;
    emit_progress(
        on_progress,
        spec.target,
        spec.stage,
        downloaded,
        total_bytes.or(Some(downloaded)),
    );
    Ok(())
}

async fn download_to_part_file_resilient(
    proxy_url: Option<&str>,
    spec: DownloadSpec<'_>,
    on_progress: &ProgressCallback,
) -> LauncherResult<()> {
    let client = build_download_client_with_mode(proxy_url, false)?;
    match download_to_part_file(&client, spec, on_progress).await {
        Ok(()) => Ok(()),
        Err(err)
            if proxy_url.and_then(normalize_proxy_url).is_none()
                && should_retry_without_system_proxy(&err) =>
        {
            let _ = fs::remove_file(spec.dest_part);
            let direct_client = build_download_client_with_mode(None, true)?;
            download_to_part_file(&direct_client, spec, on_progress).await
        }
        Err(err) => Err(err),
    }
}

struct ApiAttemptError {
    message: String,
    retry_without_proxy: bool,
}

fn is_retryable_http_status(status: reqwest::StatusCode) -> bool {
    let code = status.as_u16();
    code == 403 || code == 407 || code == 429 || status.is_server_error()
}

fn parse_tmodloader_releases_json(json: &Value) -> LauncherResult<Vec<TmodLoaderReleaseInfo>> {
    let releases = json.as_array().ok_or_else(|| {
        LauncherError::InvalidFormat("Dữ liệu releases từ GitHub không phải mảng JSON.".to_string())
    })?;

    let mut out = Vec::new();
    for rel in releases {
        let Some(tag_name) = rel["tag_name"].as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(assets) = rel["assets"].as_array() else {
            continue;
        };
        let Some(asset) = assets.iter().find(|a| {
            a["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case("tmodloader.zip"))
        }) else {
            continue;
        };
        let Some(asset_name) = asset["name"].as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(download_url) = asset["browser_download_url"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };

        let name = rel["name"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(tag_name)
            .to_string();
        let prerelease = rel["prerelease"].as_bool().unwrap_or(false);
        let published_at = rel["published_at"].as_str().unwrap_or_default().to_string();
        let size = asset["size"].as_u64().unwrap_or(0);

        out.push(TmodLoaderReleaseInfo {
            tag_name: tag_name.to_string(),
            name,
            prerelease,
            published_at,
            asset_name: asset_name.to_string(),
            download_url: download_url.to_string(),
            size,
        });
    }

    Ok(out)
}

async fn send_github_api_get(
    client: &Client,
    url: &str,
    token: Option<&str>,
) -> Result<reqwest::Response, ApiAttemptError> {
    let mut req = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION);
    if let Some(tok) = token {
        req = req.bearer_auth(tok);
    }

    req.send().await.map_err(|e| ApiAttemptError {
        message: format!("Lỗi kết nối GitHub API: {}", format_reqwest_error_chain(&e)),
        retry_without_proxy: true,
    })
}

async fn fetch_tmodloader_releases_api_once(
    client: &Client,
    url: &str,
    token: Option<&str>,
) -> Result<Vec<TmodLoaderReleaseInfo>, ApiAttemptError> {
    let mut resp = send_github_api_get(client, url, token).await?;
    if token.is_some() && matches!(resp.status().as_u16(), 401 | 403) {
        resp = send_github_api_get(client, url, None).await?;
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let detail = if status.as_u16() == 403 {
            " (có thể đã vượt giới hạn tốc độ GitHub API)"
        } else {
            ""
        };
        return Err(ApiAttemptError {
            message: format!("GitHub API trả về HTTP {status}{detail}"),
            retry_without_proxy: is_retryable_http_status(status),
        });
    }

    let bytes = resp.bytes().await.map_err(|e| ApiAttemptError {
        message: format!(
            "Lỗi đọc dữ liệu GitHub API: {}",
            format_reqwest_error_chain(&e)
        ),
        retry_without_proxy: true,
    })?;

    let json: Value = serde_json::from_slice(&bytes).map_err(|e| ApiAttemptError {
        message: format!("Phản hồi JSON từ GitHub không hợp lệ: {e}"),
        retry_without_proxy: false,
    })?;

    let list = parse_tmodloader_releases_json(&json).map_err(|e| ApiAttemptError {
        message: e.to_string(),
        retry_without_proxy: false,
    })?;

    if list.is_empty() {
        return Err(ApiAttemptError {
            message: "GitHub API không trả về bản phát hành tModLoader nào có tệp tModLoader.zip."
                .to_string(),
            retry_without_proxy: false,
        });
    }

    Ok(list)
}

fn is_valid_release_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag != "."
        && tag != ".."
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

fn extract_release_tag_from_location(location: &str) -> Option<String> {
    let trimmed = location.trim();
    let without_query = trimmed
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');

    let rest = without_query
        .split("/releases/download/")
        .nth(1)
        .or_else(|| without_query.split("/releases/tag/").nth(1))?;
    let candidate = rest.split('/').next().unwrap_or("");

    if is_valid_release_tag(candidate) {
        Some(candidate.to_string())
    } else {
        None
    }
}

async fn resolve_latest_stable_tml_tag_via_web(
    web_base: &str,
    proxy_url: Option<&str>,
) -> Option<String> {
    let web_base = web_base.trim().trim_end_matches('/');
    let allow_bypass = proxy_url.and_then(normalize_proxy_url).is_none();
    let modes: &[bool] = if allow_bypass { &[false, true] } else { &[false] };
    let urls = [
        format!("{web_base}/{TMODLOADER_GITHUB_REPO}/releases/latest/download/tModLoader.zip"),
        format!("{web_base}/{TMODLOADER_GITHUB_REPO}/releases/latest"),
    ];

    for &bypass in modes {
        let Ok(client) = build_http_client_with_options(proxy_url, bypass, false) else {
            continue;
        };
        for url in &urls {
            let Ok(resp) = client.get(url).send().await else {
                continue;
            };
            if resp.status().is_redirection() {
                if let Some(tag) = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(extract_release_tag_from_location)
                {
                    return Some(tag);
                }
            }
        }
    }

    None
}

async fn fetch_tmodloader_releases_html(
    web_base: &str,
    proxy_url: Option<&str>,
) -> Result<String, String> {
    let web_base = web_base.trim().trim_end_matches('/');
    let url = format!("{web_base}/{TMODLOADER_GITHUB_REPO}/releases");
    let allow_bypass = proxy_url.and_then(normalize_proxy_url).is_none();
    let modes: &[bool] = if allow_bypass { &[false, true] } else { &[false] };
    let mut last_err = String::new();

    for &bypass in modes {
        let client = match build_http_client_with_options(proxy_url, bypass, true) {
            Ok(c) => c,
            Err(e) => {
                last_err = e.to_string();
                continue;
            }
        };
        let resp = match client
            .get(&url)
            .header("Accept", "text/html,application/xhtml+xml")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_err = format!(
                    "Lỗi kết nối trang GitHub Releases: {}",
                    format_reqwest_error_chain(&e)
                );
                continue;
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let host = resp.url().host_str().unwrap_or("unknown");
            last_err = format!("Máy chủ {host} trả về HTTP {status}");
            continue;
        }
        match resp.text().await {
            Ok(text) => return Ok(text),
            Err(e) => {
                last_err = format!(
                    "Lỗi đọc HTML từ GitHub Releases: {}",
                    format_reqwest_error_chain(&e)
                );
            }
        }
    }

    Err(last_err)
}

fn find_open_tag_end(html: &str, start: usize, max_len: usize) -> Option<usize> {
    let bytes = html.as_bytes();
    let limit = start.saturating_add(max_len).min(bytes.len());
    let mut quote: Option<u8> = None;
    let mut i = start;
    while i < limit {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i),
            None => {}
        }
        i += 1;
    }
    None
}

fn extract_href_from_open_tag(open_tag: &str) -> Option<&str> {
    let bytes = open_tag.as_bytes();
    let mut i = 2;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            quote = Some(b);
            i += 1;
            continue;
        }
        if bytes[i - 1].is_ascii_whitespace()
            && bytes
                .get(i..i + 4)
                .is_some_and(|s| s.eq_ignore_ascii_case(b"href"))
        {
            let mut j = i + 4;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if bytes.get(j) == Some(&b'=') {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if let Some(&q) = bytes.get(j) {
                    if q == b'"' || q == b'\'' {
                        let val_start = j + 1;
                        if let Some(rel_end) = bytes[val_start..].iter().position(|&c| c == q) {
                            return Some(&open_tag[val_start..val_start + rel_end]);
                        }
                    }
                }
            }
        }
        i += 1;
    }
    None
}

fn extract_tag_from_release_href(href_val: &str) -> Option<String> {
    let trimmed = href_val.trim();
    if trimmed.contains("releases.atom") {
        return None;
    }
    let marker = format!("/{TMODLOADER_GITHUB_REPO}/releases/tag/");
    let idx = trimmed.find(&marker)?;
    let prefix = &trimmed[..idx];
    let valid_prefix =
        prefix.is_empty() || prefix.starts_with("http://") || prefix.starts_with("https://");
    if !valid_prefix || prefix.contains(char::is_whitespace) {
        return None;
    }
    let after = &trimmed[idx + marker.len()..];
    let tag = after
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    if is_valid_release_tag(tag) {
        Some(tag.to_string())
    } else {
        None
    }
}

fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn decode_html_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp_idx) = rest.find('&') {
        out.push_str(&rest[..amp_idx]);
        let after_amp = &rest[amp_idx..];
        if let Some(trimmed) = after_amp.strip_prefix("&amp;") {
            out.push('&');
            rest = trimmed;
        } else if let Some(trimmed) = after_amp.strip_prefix("&lt;") {
            out.push('<');
            rest = trimmed;
        } else if let Some(trimmed) = after_amp.strip_prefix("&gt;") {
            out.push('>');
            rest = trimmed;
        } else if let Some(trimmed) = after_amp.strip_prefix("&quot;") {
            out.push('"');
            rest = trimmed;
        } else if let Some(trimmed) = after_amp.strip_prefix("&#39;") {
            out.push('\'');
            rest = trimmed;
        } else if let Some(trimmed) = after_amp.strip_prefix("&#x27;") {
            out.push('\'');
            rest = trimmed;
        } else {
            out.push('&');
            rest = &after_amp[1..];
        }
    }
    out.push_str(rest);
    out
}

fn extract_first_datetime_attr(slice: &str) -> String {
    let Some(idx) = slice.find("datetime=\"") else {
        return String::new();
    };
    let rest = &slice[idx + "datetime=\"".len()..];
    let Some(end) = rest.find('"') else {
        return String::new();
    };
    rest[..end].trim().to_string()
}

fn tmodloader_zip_download_url(web_base: &str, tag: &str) -> String {
    format!("{web_base}/{TMODLOADER_GITHUB_REPO}/releases/download/{tag}/tModLoader.zip")
}

fn make_fallback_tmodloader_release(tag: &str, web_base: &str) -> TmodLoaderReleaseInfo {
    TmodLoaderReleaseInfo {
        tag_name: tag.to_string(),
        name: format!("tModLoader {tag}"),
        prerelease: false,
        published_at: String::new(),
        asset_name: "tModLoader.zip".to_string(),
        download_url: tmodloader_zip_download_url(web_base, tag),
        size: 0,
    }
}

struct HtmlReleaseLink {
    start: usize,
    tag: String,
    display_name: Option<String>,
}

fn parse_tmodloader_releases_html(
    html: &str,
    web_base: &str,
    latest_stable_tag: Option<&str>,
) -> Vec<TmodLoaderReleaseInfo> {
    let web_base = web_base.trim().trim_end_matches('/');
    let normalized_latest = latest_stable_tag
        .map(str::trim)
        .filter(|t| is_valid_release_tag(t));

    let bytes = html.as_bytes();
    let mut matches = Vec::new();
    let mut pos = 0;

    while pos + 2 < bytes.len() {
        let Some(rel_lt) = bytes[pos..]
            .windows(2)
            .position(|w| w[0] == b'<' && (w[1] == b'a' || w[1] == b'A'))
        else {
            break;
        };
        let lt_pos = pos + rel_lt;
        if !bytes.get(lt_pos + 2).is_some_and(|b| b.is_ascii_whitespace()) {
            pos = lt_pos + 2;
            continue;
        }
        let Some(gt_pos) = find_open_tag_end(html, lt_pos, 2048) else {
            pos = lt_pos + 2;
            continue;
        };
        let open_tag = &html[lt_pos..=gt_pos];
        if let Some(tag) = extract_href_from_open_tag(open_tag).and_then(extract_tag_from_release_href) {
            let after_open = &html[gt_pos + 1..];
            let display_name = after_open
                .as_bytes()
                .windows(4)
                .take(4096)
                .position(|w| w.eq_ignore_ascii_case(b"</a>"))
                .and_then(|close_rel| {
                    let raw = &after_open[..close_rel];
                    let cleaned: String = decode_html_entities(&strip_html_tags(raw))
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    if cleaned.is_empty() {
                        None
                    } else {
                        Some(cleaned)
                    }
                });
            matches.push(HtmlReleaseLink {
                start: lt_pos,
                tag,
                display_name,
            });
        }
        pos = gt_pos + 1;
    }

    let mut seen = BTreeSet::new();
    let mut out = Vec::new();

    for (i, item) in matches.iter().enumerate() {
        if !seen.insert(item.tag.clone()) {
            continue;
        }

        let raw_end = matches[i + 1..]
            .iter()
            .find(|m| m.tag != item.tag)
            .map_or(html.len(), |m| m.start);
        let mut end = raw_end.min(item.start.saturating_add(32_768)).min(html.len());
        while end > item.start && !html.is_char_boundary(end) {
            end -= 1;
        }
        let card_slice = &html[item.start..end];

        let name = matches[i..]
            .iter()
            .take_while(|m| m.start < end)
            .find(|m| m.tag == item.tag && m.display_name.is_some())
            .and_then(|m| m.display_name.clone())
            .unwrap_or_else(|| item.tag.clone());
        let published_at = extract_first_datetime_attr(card_slice);
        let prerelease = (Some(item.tag.as_str()) != normalized_latest)
            && (card_slice.contains(">Pre-release<") || card_slice.contains("Pre-release</span>"));

        out.push(TmodLoaderReleaseInfo {
            tag_name: item.tag.clone(),
            name,
            prerelease,
            published_at,
            asset_name: "tModLoader.zip".to_string(),
            download_url: tmodloader_zip_download_url(web_base, &item.tag),
            size: 0,
        });

        if out.len() >= 10 {
            break;
        }
    }

    if let Some(stable) = normalized_latest {
        if let Some(existing) = out.iter_mut().find(|r| r.tag_name == stable) {
            existing.prerelease = false;
        } else {
            out.insert(0, make_fallback_tmodloader_release(stable, web_base));
            if out.len() > 10 {
                out.truncate(10);
            }
        }
    }

    out
}

async fn fetch_tmodloader_releases_with_bases(
    api_base: &str,
    web_base: &str,
    proxy_url: Option<&str>,
    github_token: Option<&str>,
) -> LauncherResult<Vec<TmodLoaderReleaseInfo>> {
    let api_base = api_base.trim().trim_end_matches('/');
    let web_base = web_base.trim().trim_end_matches('/');
    let token = github_token.map(str::trim).filter(|s| !s.is_empty());
    let has_explicit_proxy = proxy_url.and_then(normalize_proxy_url).is_some();

    let api_url = format!("{api_base}/repos/{TMODLOADER_GITHUB_REPO}/releases?per_page=10");
    let api_client = build_http_client_with_options(proxy_url, false, true)?;

    let api_err = match fetch_tmodloader_releases_api_once(&api_client, &api_url, token).await {
        Ok(list) => return Ok(list),
        Err(err) if !has_explicit_proxy && err.retry_without_proxy => {
            let direct_client = build_http_client_with_options(None, true, true)?;
            match fetch_tmodloader_releases_api_once(&direct_client, &api_url, token).await {
                Ok(list) => return Ok(list),
                Err(err2) => err2.message,
            }
        }
        Err(err) => err.message,
    };

    let latest_stable_tag = resolve_latest_stable_tml_tag_via_web(web_base, proxy_url).await;
    let web_err = match fetch_tmodloader_releases_html(web_base, proxy_url).await {
        Ok(html) => {
            let parsed =
                parse_tmodloader_releases_html(&html, web_base, latest_stable_tag.as_deref());
            if !parsed.is_empty() {
                return Ok(parsed);
            }
            "Không tìm thấy bản phát hành nào trên trang GitHub Releases.".to_string()
        }
        Err(err) => err,
    };

    if let Some(stable) = latest_stable_tag.filter(|t| is_valid_release_tag(t)) {
        return Ok(vec![make_fallback_tmodloader_release(&stable, web_base)]);
    }

    Err(LauncherError::Network(format!(
        "{api_err}; dự phòng trang GitHub Releases cũng thất bại: {web_err}"
    )))
}

pub async fn fetch_tmodloader_releases(
    proxy_url: Option<&str>,
    github_token: Option<&str>,
) -> LauncherResult<Vec<TmodLoaderReleaseInfo>> {
    fetch_tmodloader_releases_with_bases(
        "https://api.github.com",
        "https://github.com",
        proxy_url,
        github_token,
    )
    .await
}

fn is_valid_dotnet_version(raw: &str) -> bool {
    let ver = raw.trim();
    if ver.is_empty() {
        return false;
    }
    if !ver
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return false;
    }
    let main_part = ver.split('-').next().unwrap_or("");
    let segments: Vec<&str> = main_part.split('.').collect();
    segments.len() >= 2
        && segments
            .iter()
            .all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()))
}

pub fn parse_dotnet_runtime_version(runtimeconfig_json: &str) -> LauncherResult<String> {
    let clean = runtimeconfig_json.trim_start_matches('\u{feff}');
    let root: Value = serde_json::from_str(clean)?;
    let runtime_options = root.get("runtimeOptions").ok_or_else(|| {
        LauncherError::InvalidFormat("Thiếu mục runtimeOptions trong runtimeconfig.json.".to_string())
    })?;

    let from_single = runtime_options.get("framework").and_then(|fw| {
        let name_ok = fw
            .get("name")
            .and_then(|n| n.as_str())
            .is_none_or(|n| n.eq_ignore_ascii_case("Microsoft.NETCore.App"));
        if name_ok {
            fw.get("version").and_then(|v| v.as_str())
        } else {
            None
        }
    });

    let from_array = runtime_options
        .get("frameworks")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|fw| {
                let is_netcore = fw
                    .get("name")
                    .and_then(|n| n.as_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case("Microsoft.NETCore.App"));
                if is_netcore {
                    fw.get("version").and_then(|v| v.as_str())
                } else {
                    None
                }
            })
        });

    let raw_version = from_single.or(from_array).ok_or_else(|| {
        LauncherError::InvalidFormat(
            "Không tìm thấy phiên bản Microsoft.NETCore.App trong runtimeconfig.json.".to_string(),
        )
    })?;

    let trimmed = raw_version.trim();
    if !is_valid_dotnet_version(trimmed) {
        return Err(LauncherError::InvalidFormat(format!(
            "Phiên bản .NET runtime không hợp lệ: '{trimmed}'"
        )));
    }

    Ok(trimmed.to_string())
}

pub(crate) fn read_declared_tml_dotnet_version(paths: &LauncherPaths) -> Option<String> {
    let runtimeconfig_path = paths.tmodloader_dir.join("tModLoader.runtimeconfig.json");
    fs::read_to_string(runtimeconfig_path)
        .ok()
        .and_then(|json| parse_dotnet_runtime_version(&json).ok())
}

pub fn read_required_tml_dotnet_version(paths: &LauncherPaths) -> String {
    read_declared_tml_dotnet_version(paths).unwrap_or_else(|| "8.0.0".to_string())
}

fn tmodloader_needs_dotnet8_install_with<FExact, FLocal, FSys, FAny>(
    paths: &LauncherPaths,
    detect_exact: FExact,
    query_local: FLocal,
    query_system: FSys,
    detect_any: FAny,
) -> bool
where
    FExact: Fn(&LauncherPaths, &str) -> bool,
    FLocal: Fn(&LauncherPaths) -> Option<String>,
    FSys: Fn() -> Option<String>,
    FAny: Fn(&LauncherPaths) -> bool,
{
    match read_declared_tml_dotnet_version(paths) {
        Some(req) => {
            !detect_exact(paths, &req)
                && !query_local(paths).is_some_and(|v| tl_core::version_at_least_patch(&v, &req))
                && !query_system().is_some_and(|v| tl_core::version_at_least_patch(&v, &req))
        }
        None => !detect_any(paths),
    }
}

fn tmodloader_needs_dotnet8_install(paths: &LauncherPaths) -> bool {
    tmodloader_needs_dotnet8_install_with(
        paths,
        |p, req| {
            detect_exact_bundled_dotnet_runtime(p, req)
                && tl_core::query_local_dotnet8_runtime(p)
                    .is_some_and(|v| tl_core::version_at_least_patch(&v, req))
        },
        tl_core::query_local_dotnet8_runtime,
        tl_core::query_system_dotnet8_runtime,
        |p| detect_dotnet8_runtime(p).is_some(),
    )
}

pub async fn install_dotnet8_portable(
    paths: &LauncherPaths,
    proxy_url: Option<&str>,
    on_progress: ProgressCallback,
) -> LauncherResult<String> {
    let runtimeconfig_path = paths.tmodloader_dir.join("tModLoader.runtimeconfig.json");
    if !runtimeconfig_path.is_file() {
        return Err(LauncherError::NotFound(
            "Cần cài tModLoader trước để xác định chính xác phiên bản .NET từ tModLoader.runtimeconfig.json"
                .to_string(),
        ));
    }

    let runtimeconfig_json = fs::read_to_string(&runtimeconfig_path)?;
    let ver = parse_dotnet_runtime_version(&runtimeconfig_json)?;

    let urls = [
        format!("https://builds.dotnet.microsoft.com/dotnet/Runtime/{ver}/dotnet-runtime-{ver}-win-x64.zip"),
        format!("https://dotnetcli.azureedge.net/dotnet/Runtime/{ver}/dotnet-runtime-{ver}-win-x64.zip"),
    ];

    let cache_dir = paths.data_dir.join("cache");
    fs::create_dir_all(&cache_dir)?;
    let part_path = cache_dir.join(format!(
        "dotnet-runtime-{ver}-win-x64-{}.zip.part",
        uuid::Uuid::new_v4()
    ));
    let _part_guard = TempFileGuard::new(part_path.clone());

    let stage = format!("Đang tải .NET {ver} portable...");
    let mut last_err: Option<LauncherError> = None;

    for url in &urls {
        let _ = fs::remove_file(&part_path);
        match download_to_part_file_resilient(
            proxy_url,
            DownloadSpec {
                url,
                dest_part: &part_path,
                target: InstallTarget::DotNet8,
                stage: &stage,
                fallback_total: None,
                accept_header: None,
                bearer_token: None,
            },
            &on_progress,
        )
        .await
        {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(err) => {
                last_err = Some(err);
            }
        }
    }

    if let Some(err) = last_err {
        return Err(err);
    }

    let dotnet_dir = paths.tmodloader_dir.join("dotnet");
    extract_zip_safely_blocking(
        part_path,
        dotnet_dir.clone(),
        InstallTarget::DotNet8,
        format!("Đang giải nén .NET {ver} portable..."),
        on_progress,
    )
    .await?;

    if !dotnet_dir.join("dotnet.exe").is_file() {
        return Err(LauncherError::InvalidFormat(
            "Gói .NET giải nén xong nhưng không tìm thấy dotnet.exe trong tModLoader\\dotnet."
                .to_string(),
        ));
    }

    Ok(format!("Đã cài đặt .NET {ver} portable vào tModLoader\\dotnet."))
}

pub async fn install_tmodloader_release(
    paths: &LauncherPaths,
    release: &TmodLoaderReleaseInfo,
    proxy_url: Option<&str>,
    also_install_dotnet: bool,
    on_progress: ProgressCallback,
) -> LauncherResult<String> {
    let url = release.download_url.trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(LauncherError::InvalidInput(
            "URL tải tModLoader không hợp lệ.".to_string(),
        ));
    }

    let cache_dir = paths.data_dir.join("cache");
    fs::create_dir_all(&cache_dir)?;
    let part_path = cache_dir.join(format!("tModLoader-{}.zip.part", uuid::Uuid::new_v4()));
    let _part_guard = TempFileGuard::new(part_path.clone());

    let fallback_size = if release.size > 0 { Some(release.size) } else { None };
    let dl_stage = format!("Đang tải tModLoader {}...", release.tag_name);

    let mut candidate_urls = vec![url.to_string()];
    if !release.prerelease && url.starts_with("https://github.com/") {
        let latest_url = format!(
            "https://github.com/{TMODLOADER_GITHUB_REPO}/releases/latest/download/tModLoader.zip"
        );
        if latest_url != url {
            candidate_urls.push(latest_url);
        }
    }

    let mut last_err: Option<LauncherError> = None;
    for candidate_url in &candidate_urls {
        let _ = fs::remove_file(&part_path);
        match download_to_part_file_resilient(
            proxy_url,
            DownloadSpec {
                url: candidate_url,
                dest_part: &part_path,
                target: InstallTarget::TModLoader,
                stage: &dl_stage,
                fallback_total: fallback_size,
                accept_header: None,
                bearer_token: None,
            },
            &on_progress,
        )
        .await
        {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(err) => {
                last_err = Some(err);
            }
        }
    }

    if let Some(err) = last_err {
        return Err(err);
    }

    verify_zip_contains_required_file_blocking(part_path.clone(), "tmodloader.dll").await?;

    let extracted_top = extract_zip_safely_blocking(
        part_path,
        paths.tmodloader_dir.clone(),
        InstallTarget::TModLoader,
        format!("Đang giải nén tModLoader {}...", release.tag_name),
        on_progress.clone(),
    )
    .await?;

    if !paths.tmodloader_dll().is_file() {
        let preview = format_top_level_preview(&extracted_top);
        return Err(LauncherError::InvalidFormat(format!(
            "Giải nén hoàn tất nhưng không thấy tModLoader.dll. Thư mục gốc trong file nén: {preview}"
        )));
    }

    if also_install_dotnet && tmodloader_needs_dotnet8_install(paths) {
        let dotnet_msg = install_dotnet8_portable(paths, proxy_url, on_progress).await?;
        return Ok(format!(
            "Đã cài đặt tModLoader {} ({dotnet_msg})",
            release.tag_name
        ));
    }

    Ok(format!(
        "Đã cài đặt tModLoader {} vào {}.",
        release.tag_name,
        paths.tmodloader_dir.display()
    ))
}

pub fn parse_github_release_spec(input: &str) -> Option<(String, Option<String>)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.contains("://") {
        return parse_github_release_url(trimmed);
    }

    parse_github_shorthand(trimmed)
}

fn parse_github_release_url(raw: &str) -> Option<(String, Option<String>)> {
    let without_scheme = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))?;

    let path_and_query = without_scheme.split(['?', '#']).next()?;
    let (host, rest) = path_and_query.split_once('/')?;
    let host_clean = host.strip_prefix("www.").unwrap_or(host);
    if !host_clean.eq_ignore_ascii_case("github.com") {
        return None;
    }

    let segments: Vec<&str> = rest
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    match segments.as_slice() {
        [owner, repo] if is_valid_github_owner_repo(owner, repo) => {
            Some((format!("{owner}/{repo}"), None))
        }
        [owner, repo, releases]
            if releases.eq_ignore_ascii_case("releases") && is_valid_github_owner_repo(owner, repo) =>
        {
            Some((format!("{owner}/{repo}"), None))
        }
        [owner, repo, releases, latest]
            if releases.eq_ignore_ascii_case("releases")
                && latest.eq_ignore_ascii_case("latest")
                && is_valid_github_owner_repo(owner, repo) =>
        {
            Some((format!("{owner}/{repo}"), None))
        }
        [owner, repo, releases, tag_kw, tag @ ..]
            if releases.eq_ignore_ascii_case("releases")
                && tag_kw.eq_ignore_ascii_case("tag")
                && !tag.is_empty()
                && is_valid_github_owner_repo(owner, repo) =>
        {
            let tag_str = tag.join("/");
            if tag_str.is_empty() {
                None
            } else {
                Some((format!("{owner}/{repo}"), Some(tag_str)))
            }
        }
        _ => None,
    }
}

fn parse_github_shorthand(raw: &str) -> Option<(String, Option<String>)> {
    if raw.starts_with('/') || raw.contains('\\') || raw.chars().any(char::is_whitespace) {
        return None;
    }

    let (repo_part, tag_opt) = match raw.split_once('@') {
        Some((r, t)) => {
            if t.is_empty() || t.contains('@') {
                return None;
            }
            (r, Some(t.to_string()))
        }
        None => (raw, None),
    };

    let mut parts = repo_part.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    if !is_valid_github_owner_repo(owner, repo) {
        return None;
    }
    if repo.to_ascii_lowercase().ends_with(".zip") {
        return None;
    }

    Some((format!("{owner}/{repo}"), tag_opt))
}

fn is_valid_github_owner_repo(owner: &str, repo: &str) -> bool {
    let valid_part = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && !s.contains("..")
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    valid_part(owner) && valid_part(repo)
}

fn is_valid_branch_name(branch: &str) -> bool {
    !branch.is_empty()
        && !branch.starts_with('/')
        && !branch.ends_with('/')
        && !branch.contains("//")
        && !branch.contains("..")
        && !branch.contains('\\')
        && !branch.contains(':')
        && !branch.contains(['@', '?', '#'])
        && !branch.chars().any(char::is_whitespace)
        && branch
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != ".." && !seg.chars().any(char::is_control))
}

fn is_valid_relative_subdir(subdir: &str) -> bool {
    !subdir.is_empty()
        && !subdir.contains("..")
        && !subdir.contains('\\')
        && !subdir.contains(':')
        && !subdir.contains(['@', '?', '#'])
        && !subdir.chars().any(char::is_whitespace)
        && subdir
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != ".." && !seg.chars().any(char::is_control))
}

pub fn parse_github_branch_spec(input: &str) -> Option<(String, String, Option<String>)> {
    let trimmed = input.trim();
    if trimmed.is_empty()
        || trimmed.chars().any(char::is_whitespace)
        || trimmed.contains(['@', '?', '\\'])
    {
        return None;
    }

    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        return parse_github_branch_url(trimmed);
    }

    parse_github_branch_shorthand(trimmed)
}

fn parse_github_branch_url(raw: &str) -> Option<(String, String, Option<String>)> {
    let without_scheme = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))?;

    let path_only = without_scheme.split('#').next()?;
    let (host, rest) = path_only.split_once('/')?;
    let host_clean = host.strip_prefix("www.").unwrap_or(host);
    if !host_clean.eq_ignore_ascii_case("github.com") {
        return None;
    }

    let trimmed_rest = rest.trim_matches('/');
    if trimmed_rest.is_empty() {
        return None;
    }
    let segments: Vec<&str> = trimmed_rest.split('/').collect();

    match segments.as_slice() {
        [owner, repo, tree_kw, branch_parts @ ..]
            if tree_kw.eq_ignore_ascii_case("tree")
                && !branch_parts.is_empty()
                && is_valid_github_owner_repo(owner, repo) =>
        {
            let branch = branch_parts.join("/");
            if is_valid_branch_name(&branch) {
                Some((format!("{owner}/{repo}"), branch, None))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn parse_github_branch_shorthand(raw: &str) -> Option<(String, String, Option<String>)> {
    if raw.starts_with('/') {
        return None;
    }

    let (repo_part, branch_and_subdir) = raw.split_once('#')?;
    if branch_and_subdir.contains('#') {
        return None;
    }

    let mut repo_segments = repo_part.split('/');
    let owner = repo_segments.next()?;
    let repo = repo_segments.next()?;
    if repo_segments.next().is_some()
        || !is_valid_github_owner_repo(owner, repo)
        || repo.to_ascii_lowercase().ends_with(".zip")
    {
        return None;
    }

    let (branch_raw, subdir_opt) = match branch_and_subdir.split_once(':') {
        Some((branch_part, subdir_part)) => {
            let clean_subdir = subdir_part.trim_matches('/');
            if clean_subdir.is_empty() {
                (branch_part, None)
            } else if is_valid_relative_subdir(clean_subdir) {
                (branch_part, Some(clean_subdir.to_string()))
            } else {
                return None;
            }
        }
        None => (branch_and_subdir, None),
    };

    if !is_valid_branch_name(branch_raw) {
        return None;
    }

    Some((format!("{owner}/{repo}"), branch_raw.to_string(), subdir_opt))
}

fn encode_url_component(raw: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(*byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0F) as usize] as char);
        }
    }
    out
}

fn encode_url_path(path: &str) -> String {
    path.split('/')
        .map(encode_url_component)
        .collect::<Vec<_>>()
        .join("/")
}

fn parse_direct_manifest_url(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return None;
    }
    let scheme_len = if trimmed.starts_with("https://") {
        "https://".len()
    } else if trimmed.starts_with("http://") {
        "http://".len()
    } else {
        return None;
    };

    let url_no_frag = trimmed.split('#').next()?;
    let without_scheme = &url_no_frag[scheme_len..];
    let path_without_query = without_scheme.split('?').next()?;
    let (authority, rel_path) = path_without_query.split_once('/')?;
    if authority.is_empty() {
        return None;
    }

    let suffix_slash = format!("/{TERRARIA_MANIFEST_FILENAME}");
    if rel_path != TERRARIA_MANIFEST_FILENAME && !rel_path.ends_with(&suffix_slash) {
        return None;
    }

    let url_before_query = url_no_frag.split('?').next()?;
    let base_url = url_before_query
        .strip_suffix(TERRARIA_MANIFEST_FILENAME)?
        .to_string();
    Some((url_no_frag.to_string(), base_url))
}

fn extract_http_host(raw: &str) -> &str {
    let rest = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))
        .unwrap_or("");
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    host.split(':').next().unwrap_or("")
}

fn is_github_url(raw: &str) -> bool {
    let host = extract_http_host(raw);
    host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("api.github.com")
}

fn is_github_manifest_auth_host(raw: &str) -> bool {
    let host = extract_http_host(raw);
    is_github_url(raw) || host.eq_ignore_ascii_case("raw.githubusercontent.com")
}

fn is_github_api_asset_url(raw: &str) -> bool {
    extract_http_host(raw).eq_ignore_ascii_case("api.github.com")
        && raw.contains("/releases/assets/")
}

enum ManifestOrigin {
    GitHubBranch {
        owner_repo: String,
        branch: String,
        subdir: Option<String>,
    },
    DirectManifestUrl {
        manifest_url: String,
        base_url: String,
    },
}

struct ManifestRequestSpec {
    url: String,
    accept_header: Option<&'static str>,
    bearer_token: Option<String>,
}

impl ManifestOrigin {
    fn resolve(source: &str) -> Option<Self> {
        if let Some((owner_repo, branch, subdir)) = parse_github_branch_spec(source) {
            return Some(Self::GitHubBranch {
                owner_repo,
                branch,
                subdir,
            });
        }
        parse_direct_manifest_url(source).map(|(manifest_url, base_url)| Self::DirectManifestUrl {
            manifest_url,
            base_url,
        })
    }

    fn github_file_request(
        owner_repo: &str,
        branch: &str,
        subdir: Option<&str>,
        filename: &str,
        token: Option<&str>,
    ) -> ManifestRequestSpec {
        let rel_path = match subdir {
            Some(dir) => format!("{dir}/{filename}"),
            None => filename.to_string(),
        };
        if let Some(tok) = token {
            ManifestRequestSpec {
                url: format!(
                    "https://api.github.com/repos/{owner_repo}/contents/{}?ref={}",
                    encode_url_path(&rel_path),
                    encode_url_component(branch)
                ),
                accept_header: Some("application/vnd.github.raw+json"),
                bearer_token: Some(tok.to_string()),
            }
        } else {
            ManifestRequestSpec {
                url: format!(
                    "https://raw.githubusercontent.com/{owner_repo}/{}/{}",
                    encode_url_path(branch),
                    encode_url_path(&rel_path)
                ),
                accept_header: None,
                bearer_token: None,
            }
        }
    }

    fn manifest_request(&self, token: Option<&str>) -> ManifestRequestSpec {
        match self {
            Self::GitHubBranch {
                owner_repo,
                branch,
                subdir,
            } => Self::github_file_request(
                owner_repo,
                branch,
                subdir.as_deref(),
                TERRARIA_MANIFEST_FILENAME,
                token,
            ),
            Self::DirectManifestUrl { manifest_url, .. } => {
                let bearer_token = token
                    .filter(|_| is_github_manifest_auth_host(manifest_url))
                    .map(str::to_string);
                ManifestRequestSpec {
                    url: manifest_url.clone(),
                    accept_header: None,
                    bearer_token,
                }
            }
        }
    }

    fn part_request(&self, part_file: &str, token: Option<&str>) -> ManifestRequestSpec {
        match self {
            Self::GitHubBranch {
                owner_repo,
                branch,
                subdir,
            } => Self::github_file_request(
                owner_repo,
                branch,
                subdir.as_deref(),
                part_file,
                token,
            ),
            Self::DirectManifestUrl { base_url, .. } => {
                let url = format!("{base_url}{}", encode_url_component(part_file));
                let bearer_token = token
                    .filter(|_| is_github_manifest_auth_host(&url))
                    .map(str::to_string);
                ManifestRequestSpec {
                    url,
                    accept_header: None,
                    bearer_token,
                }
            }
        }
    }
}

fn is_valid_sha256_hex(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.len() == 64 && trimmed.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_safe_manifest_part_filename(file: &str) -> bool {
    !file.is_empty()
        && !file.trim().is_empty()
        && file.trim() == file
        && file != "."
        && file != ".."
        && !file.contains('/')
        && !file.contains('\\')
        && !file.contains("..")
        && !file.chars().any(char::is_control)
}

fn validate_branch_manifest(manifest: &TerrariaBranchManifest) -> LauncherResult<()> {
    if manifest.version.trim().is_empty() {
        return Err(LauncherError::InvalidFormat(
            "Manifest thiếu thông tin phiên bản (version).".to_string(),
        ));
    }
    if manifest.total_size == 0 {
        return Err(LauncherError::InvalidFormat(
            "Manifest có tổng dung lượng (total_size) bằng 0, không hợp lệ.".to_string(),
        ));
    }
    let full_sha = manifest.sha256.trim();
    if !full_sha.is_empty() && !is_valid_sha256_hex(full_sha) {
        return Err(LauncherError::InvalidFormat(
            "Mã SHA-256 tổng thể trong manifest không đúng định dạng 64 ký tự hex.".to_string(),
        ));
    }
    if manifest.parts.is_empty() {
        return Err(LauncherError::InvalidFormat(
            "Manifest không chứa danh sách phần tải nào (parts rỗng).".to_string(),
        ));
    }

    for (idx, part) in manifest.parts.iter().enumerate() {
        if !is_safe_manifest_part_filename(&part.file) {
            return Err(LauncherError::InvalidFormat(format!(
                "Tên tệp phần thứ {} trong manifest không hợp lệ hoặc không an toàn: '{}'.",
                idx + 1,
                part.file
            )));
        }
        if part.size == 0 {
            return Err(LauncherError::InvalidFormat(format!(
                "Phần '{}' trong manifest có kích thước bằng 0, không hợp lệ.",
                part.file
            )));
        }
        let part_sha = part.sha256.trim();
        if !part_sha.is_empty() && !is_valid_sha256_hex(part_sha) {
            return Err(LauncherError::InvalidFormat(format!(
                "Mã SHA-256 của phần '{}' trong manifest không đúng định dạng 64 ký tự hex.",
                part.file
            )));
        }
    }

    if !manifest.parts.is_empty() {
        let sum = manifest
            .parts
            .iter()
            .try_fold(0u64, |acc, p| acc.checked_add(p.size))
            .ok_or_else(|| {
                LauncherError::InvalidFormat(
                    "Tổng kích thước các phần trong manifest vượt quá giới hạn u64.".to_string(),
                )
            })?;
        if sum != manifest.total_size {
            return Err(LauncherError::InvalidFormat(format!(
                "Tổng kích thước các phần trong manifest ({sum} byte) không khớp với total_size ({} byte).",
                manifest.total_size
            )));
        }
    }

    Ok(())
}

fn format_manifest_http_error(status: reqwest::StatusCode, context: &str) -> LauncherError {
    match status.as_u16() {
        401 => LauncherError::Network(format!(
            "Không có quyền xác thực khi tải {context} (HTTP 401). Vui lòng kiểm tra lại GitHub Token."
        )),
        403 => LauncherError::Network(format!(
            "Bị từ chối truy cập hoặc đã vượt giới hạn tốc độ khi tải {context} (HTTP 403). Nếu là repo private hoặc hết rate limit, hãy kiểm tra/thêm GitHub Token."
        )),
        404 => LauncherError::NotFound(format!(
            "Không tìm thấy {context} (HTTP 404). Kiểm tra lại tên repo, nhánh hoặc đường dẫn manifest."
        )),
        _ => LauncherError::Network(format!(
            "Máy chủ trả về HTTP {status} khi tải {context}."
        )),
    }
}

async fn fetch_manifest_from_origin(
    origin: &ManifestOrigin,
    token: Option<&str>,
    proxy_url: Option<&str>,
) -> LauncherResult<TerrariaBranchManifest> {
    let client = build_http_client(proxy_url)?;
    let spec = origin.manifest_request(token);
    let mut req = client.get(&spec.url);
    if let Some(accept) = spec.accept_header {
        req = req.header("Accept", accept);
    }
    if let Some(tok) = spec.bearer_token {
        req = req.bearer_auth(tok);
    }

    let resp = req.send().await.map_err(|e| {
        LauncherError::Network(format!(
            "Lỗi kết nối khi tải {TERRARIA_MANIFEST_FILENAME}: {e}"
        ))
    })?;

    if !resp.status().is_success() {
        return Err(format_manifest_http_error(
            resp.status(),
            TERRARIA_MANIFEST_FILENAME,
        ));
    }

    let raw_text = resp.text().await.map_err(|e| {
        LauncherError::Network(format!(
            "Không thể đọc nội dung {TERRARIA_MANIFEST_FILENAME}: {e}"
        ))
    })?;

    let manifest: TerrariaBranchManifest =
        serde_json::from_str(raw_text.trim_start_matches('\u{feff}')).map_err(|e| {
            LauncherError::InvalidFormat(format!(
                "Nội dung {TERRARIA_MANIFEST_FILENAME} không phải JSON hợp lệ: {e}"
            ))
        })?;

    validate_branch_manifest(&manifest)?;
    Ok(manifest)
}

pub async fn fetch_terraria_branch_manifest(
    source: &str,
    github_token: Option<&str>,
    proxy_url: Option<&str>,
) -> LauncherResult<TerrariaBranchManifest> {
    let trimmed = source.trim();
    let origin = ManifestOrigin::resolve(trimmed).ok_or_else(|| {
        LauncherError::InvalidInput(
            "Nguồn manifest không hợp lệ (hỗ trợ owner/repo#branch[:subdir], GitHub /tree/branch hoặc URL kết thúc bằng terraria-manifest.json)."
                .to_string(),
        )
    })?;
    let token = github_token.map(str::trim).filter(|s| !s.is_empty());
    fetch_manifest_from_origin(&origin, token, proxy_url).await
}

fn installed_terraria_receipt_path(paths: &LauncherPaths) -> PathBuf {
    paths.data_dir.join(TERRARIA_RECEIPT_FILENAME)
}

fn normalize_terraria_install_dir(raw: &str) -> String {
    let slash_norm = raw.trim().replace('\\', "/");
    let trimmed = slash_norm.trim_end_matches('/');
    let mut out = if trimmed.is_empty() && slash_norm.starts_with('/') {
        "/".to_string()
    } else if trimmed.len() == 2 && trimmed.as_bytes()[1] == b':' && slash_norm.len() > 2 {
        format!("{trimmed}/")
    } else {
        trimmed.to_string()
    };
    #[cfg(windows)]
    out.make_ascii_lowercase();
    out
}

pub fn load_installed_terraria_receipt(paths: &LauncherPaths) -> Option<InstalledTerrariaReceipt> {
    if !paths.terraria_exe().is_file() {
        return None;
    }
    let raw = fs::read_to_string(installed_terraria_receipt_path(paths)).ok()?;
    let receipt =
        serde_json::from_str::<InstalledTerrariaReceipt>(raw.trim_start_matches('\u{feff}')).ok()?;
    let current_norm = normalize_terraria_install_dir(&paths.terraria_dir.to_string_lossy());
    if !receipt.install_dir.trim().is_empty() {
        if normalize_terraria_install_dir(&receipt.install_dir) != current_norm {
            return None;
        }
    } else {
        let default_norm =
            normalize_terraria_install_dir(&paths.base_dir.join("Terraria").to_string_lossy());
        if current_norm != default_norm {
            return None;
        }
    }
    Some(receipt)
}

fn save_installed_terraria_receipt(
    paths: &LauncherPaths,
    receipt: &InstalledTerrariaReceipt,
) -> LauncherResult<()> {
    fs::create_dir_all(&paths.data_dir)?;
    let mut stored = receipt.clone();
    stored.install_dir = paths.terraria_dir.to_string_lossy().into_owned();
    let json = serde_json::to_string_pretty(&stored)?;
    tl_core::config::write_atomic(&installed_terraria_receipt_path(paths), json.as_bytes())?;
    Ok(())
}

fn remove_installed_terraria_receipt(paths: &LauncherPaths) {
    let _ = fs::remove_file(installed_terraria_receipt_path(paths));
}

fn sanitize_archive_stem(archive_name: &str) -> String {
    let base = archive_name
        .trim()
        .strip_suffix(".zip")
        .or_else(|| archive_name.trim().strip_suffix(".ZIP"))
        .unwrap_or_else(|| archive_name.trim());
    let cleaned: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    if cleaned.is_empty() {
        "Terraria".to_string()
    } else {
        cleaned
    }
}

async fn assemble_multipart_stream<S, B, F, Fut>(
    manifest: &TerrariaBranchManifest,
    dest_part: &Path,
    on_progress: &ProgressCallback,
    mut open_part_stream: F,
) -> LauncherResult<String>
where
    S: futures_util::Stream<Item = LauncherResult<B>>,
    B: AsRef<[u8]>,
    F: FnMut(usize, &TerrariaManifestPart) -> Fut,
    Fut: std::future::Future<Output = LauncherResult<S>>,
{
    validate_branch_manifest(manifest)?;

    if let Some(parent) = dest_part.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut out_file = tokio::fs::File::create(dest_part).await?;
    let mut full_hasher = Sha256::new();
    let total_size = manifest.total_size;
    let total_parts = manifest.parts.len();
    let version = manifest.version.trim();
    let mut base_downloaded: u64 = 0;
    let mut last_reported: u64 = 0;

    for (idx, part) in manifest.parts.iter().enumerate() {
        let stage = format!("Đang tải Terraria {version} ({}/{total_parts})...", idx + 1);
        emit_progress(
            on_progress,
            InstallTarget::Terraria,
            &stage,
            base_downloaded,
            Some(total_size),
        );

        let stream = open_part_stream(idx, part).await?;
        tokio::pin!(stream);

        let mut part_hasher = Sha256::new();
        let mut part_written: u64 = 0;

        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res?;
            let bytes = chunk.as_ref();
            if bytes.is_empty() {
                continue;
            }
            out_file.write_all(bytes).await?;
            part_hasher.update(bytes);
            full_hasher.update(bytes);
            part_written += bytes.len() as u64;

            let current_total = base_downloaded + part_written;
            if current_total.saturating_sub(last_reported) >= DOWNLOAD_PROGRESS_STEP_BYTES
                || current_total >= total_size
            {
                last_reported = current_total;
                emit_progress(
                    on_progress,
                    InstallTarget::Terraria,
                    &stage,
                    current_total,
                    Some(total_size),
                );
            }
        }

        if part_written != part.size {
            return Err(LauncherError::InvalidFormat(format!(
                "Kích thước phần '{}' không khớp manifest (nhận {part_written} byte, kỳ vọng {} byte).",
                part.file, part.size
            )));
        }

        let expected_part_sha = part.sha256.trim();
        if !expected_part_sha.is_empty() {
            let actual_part_sha = hex::encode(part_hasher.finalize());
            if !actual_part_sha.eq_ignore_ascii_case(expected_part_sha) {
                return Err(LauncherError::InvalidFormat(format!(
                    "Mã kiểm tra SHA-256 của phần '{}' không khớp (nhận {actual_part_sha}, kỳ vọng {}).",
                    part.file,
                    expected_part_sha.to_ascii_lowercase()
                )));
            }
        }

        base_downloaded += part_written;
    }

    out_file.flush().await?;
    drop(out_file);

    emit_progress(
        on_progress,
        InstallTarget::Terraria,
        format!("Đang xác minh gói Terraria {version}..."),
        base_downloaded,
        Some(total_size.max(base_downloaded)),
    );

    let full_hash_hex = hex::encode(full_hasher.finalize());
    let expected_full_sha = manifest.sha256.trim();
    if !expected_full_sha.is_empty() && !full_hash_hex.eq_ignore_ascii_case(expected_full_sha) {
        return Err(LauncherError::InvalidFormat(format!(
            "Mã kiểm tra SHA-256 tổng thể của gói '{}' không khớp (nhận {full_hash_hex}, kỳ vọng {}).",
            manifest.archive_name,
            expected_full_sha.to_ascii_lowercase()
        )));
    }

    Ok(full_hash_hex)
}

async fn install_terraria_from_manifest_streams<S, B, F, Fut>(
    paths: &LauncherPaths,
    source: &str,
    manifest: &TerrariaBranchManifest,
    on_progress: ProgressCallback,
    open_part_stream: F,
) -> LauncherResult<String>
where
    S: futures_util::Stream<Item = LauncherResult<B>>,
    B: AsRef<[u8]>,
    F: FnMut(usize, &TerrariaManifestPart) -> Fut,
    Fut: std::future::Future<Output = LauncherResult<S>>,
{
    let cache_dir = paths.data_dir.join("cache");
    fs::create_dir_all(&cache_dir)?;
    let archive_stem = sanitize_archive_stem(&manifest.archive_name);
    let part_path = cache_dir.join(format!("{archive_stem}-{}.zip.part", uuid::Uuid::new_v4()));
    let _part_guard = TempFileGuard::new(part_path.clone());

    let full_sha256 =
        assemble_multipart_stream(manifest, &part_path, &on_progress, open_part_stream).await?;

    verify_zip_contains_required_file_blocking(part_path.clone(), "terraria.exe").await?;

    let version = manifest.version.trim();
    let extracted_top = extract_zip_safely_blocking(
        part_path,
        paths.terraria_dir.clone(),
        InstallTarget::Terraria,
        format!("Đang giải nén Terraria {version}..."),
        on_progress,
    )
    .await?;

    verify_terraria_installed(paths, &extracted_top)?;

    let receipt = InstalledTerrariaReceipt {
        version: version.to_string(),
        sha256: full_sha256,
        source: source.trim().to_string(),
        install_dir: paths.terraria_dir.to_string_lossy().into_owned(),
    };
    save_installed_terraria_receipt(paths, &receipt)?;

    Ok(format!(
        "Đã cài đặt / cập nhật Terraria {version} vào {}.",
        paths.terraria_dir.display()
    ))
}

async fn install_terraria_from_branch_source(
    paths: &LauncherPaths,
    origin: &ManifestOrigin,
    trimmed_source: &str,
    token: Option<&str>,
    proxy_url: Option<&str>,
    on_progress: ProgressCallback,
) -> LauncherResult<String> {
    emit_progress(
        &on_progress,
        InstallTarget::Terraria,
        format!("Đang kiểm tra {TERRARIA_MANIFEST_FILENAME}..."),
        0,
        None,
    );

    let manifest = fetch_manifest_from_origin(origin, token, proxy_url).await?;
    let client = build_download_client(proxy_url)?;
    let total_parts = manifest.parts.len();

    install_terraria_from_manifest_streams(
        paths,
        trimmed_source,
        &manifest,
        on_progress,
        |idx, part| {
            let req_spec = origin.part_request(&part.file, token);
            let client = client.clone();
            let part_file = part.file.clone();
            async move {
                let mut req = client.get(&req_spec.url);
                if let Some(accept) = req_spec.accept_header {
                    req = req.header("Accept", accept);
                }
                if let Some(tok) = req_spec.bearer_token {
                    req = req.bearer_auth(tok);
                }

                let resp = req.send().await.map_err(|e| {
                    LauncherError::Network(format!(
                        "Lỗi kết nối khi tải phần '{}' ({}/{}): {e}",
                        part_file,
                        idx + 1,
                        total_parts
                    ))
                })?;

                if !resp.status().is_success() {
                    return Err(format_manifest_http_error(
                        resp.status(),
                        &format!("phần '{}' ({}/{})", part_file, idx + 1, total_parts),
                    ));
                }

                let label = format!("phần '{}' ({}/{})", part_file, idx + 1, total_parts);
                let stream = resp.bytes_stream().map(move |res| {
                    res.map_err(|e| {
                        LauncherError::Network(format!("Lỗi đọc dữ liệu khi tải {label}: {e}"))
                    })
                });
                Ok(stream)
            }
        },
    )
    .await
}

struct ResolvedTerrariaDownload {
    url: String,
    expected_size: Option<u64>,
    accept_header: Option<&'static str>,
    use_token: bool,
}

async fn resolve_github_terraria_asset(
    owner_repo: &str,
    tag_opt: Option<&str>,
    token: Option<&str>,
    proxy_url: Option<&str>,
) -> LauncherResult<ResolvedTerrariaDownload> {
    let api_url = match tag_opt {
        Some(tag) => format!("https://api.github.com/repos/{owner_repo}/releases/tags/{tag}"),
        None => format!("https://api.github.com/repos/{owner_repo}/releases/latest"),
    };

    let client = build_http_client(proxy_url)?;
    let mut req = client
        .get(&api_url)
        .header("Accept", "application/vnd.github+json");
    if let Some(tok) = token {
        req = req.bearer_auth(tok);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| LauncherError::Network(format!("Lỗi kết nối GitHub API: {e}")))?;

    if !resp.status().is_success() {
        let code = resp.status().as_u16();
        let msg = match code {
            401 => "GitHub từ chối xác thực (HTTP 401). Vui lòng kiểm tra lại GitHub Token.".to_string(),
            403 => "GitHub từ chối truy cập hoặc đã vượt giới hạn tốc độ (HTTP 403). Nếu là repo private hoặc hết rate limit, hãy kiểm tra/thêm GitHub Token.".to_string(),
            404 => format!(
                "Không tìm thấy release tại GitHub repo '{owner_repo}' (HTTP 404). Kiểm tra lại tên repo/tag hoặc thêm GitHub Token nếu là repo private."
            ),
            _ => format!("GitHub API trả về HTTP {}", resp.status()),
        };
        return Err(LauncherError::Network(msg));
    }

    let release: Value = resp
        .json()
        .await
        .map_err(|e| LauncherError::Network(format!("Phản hồi JSON từ GitHub không hợp lệ: {e}")))?;

    let assets = release["assets"].as_array().ok_or_else(|| {
        LauncherError::NotFound(format!("Release của {owner_repo} không có assets đính kèm."))
    })?;

    let chosen = assets
        .iter()
        .find(|a| {
            a["name"].as_str().is_some_and(|n| {
                let lower = n.to_ascii_lowercase();
                lower.contains("terraria") && lower.ends_with(".zip")
            })
        })
        .or_else(|| {
            assets.iter().find(|a| {
                a["name"]
                    .as_str()
                    .is_some_and(|n| n.to_ascii_lowercase().ends_with(".zip"))
            })
        })
        .ok_or_else(|| {
            LauncherError::NotFound(format!(
                "Release của {owner_repo} không có file .zip nào để tải."
            ))
        })?;

    let expected_size = chosen["size"].as_u64().filter(|&n| n > 0);
    let (url, accept_header, use_token) = if token.is_some() {
        let api_asset = chosen["url"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                chosen["browser_download_url"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
            .ok_or_else(|| {
                LauncherError::InvalidFormat("Asset trong release thiếu URL tải.".to_string())
            })?;
        (api_asset.to_string(), Some("application/octet-stream"), true)
    } else {
        let browser_url = chosen["browser_download_url"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                LauncherError::InvalidFormat(
                    "Asset trong release thiếu browser_download_url.".to_string(),
                )
            })?;
        (browser_url.to_string(), None, false)
    };

    Ok(ResolvedTerrariaDownload {
        url,
        expected_size,
        accept_header,
        use_token,
    })
}

pub async fn install_terraria_from_source(
    paths: &LauncherPaths,
    source: &str,
    github_token: Option<&str>,
    proxy_url: Option<&str>,
    on_progress: ProgressCallback,
) -> LauncherResult<String> {
    let trimmed_source = source.trim();
    if trimmed_source.is_empty() {
        return Err(LauncherError::InvalidInput(
            "Vui lòng nhập nguồn tải Terraria (repo GitHub, nhánh manifest hoặc URL .zip)."
                .to_string(),
        ));
    }

    let token = github_token.map(str::trim).filter(|s| !s.is_empty());

    if let Some(origin) = ManifestOrigin::resolve(trimmed_source) {
        return install_terraria_from_branch_source(
            paths,
            &origin,
            trimmed_source,
            token,
            proxy_url,
            on_progress,
        )
        .await;
    }

    let resolved = if let Some((owner_repo, tag_opt)) = parse_github_release_spec(trimmed_source) {
        emit_progress(
            &on_progress,
            InstallTarget::Terraria,
            format!("Đang kiểm tra release từ {owner_repo}..."),
            0,
            None,
        );
        resolve_github_terraria_asset(&owner_repo, tag_opt.as_deref(), token, proxy_url).await?
    } else {
        if !trimmed_source.starts_with("http://") && !trimmed_source.starts_with("https://") {
            return Err(LauncherError::InvalidInput(
                "Nguồn tải Terraria phải là repo GitHub (owner/repo, owner/repo#branch) hoặc URL http:// / https://."
                    .to_string(),
            ));
        }
        let send_token = token.is_some() && is_github_url(trimmed_source);
        let accept = if send_token && is_github_api_asset_url(trimmed_source) {
            Some("application/octet-stream")
        } else {
            None
        };
        ResolvedTerrariaDownload {
            url: trimmed_source.to_string(),
            expected_size: None,
            accept_header: accept,
            use_token: send_token,
        }
    };

    let cache_dir = paths.data_dir.join("cache");
    fs::create_dir_all(&cache_dir)?;
    let part_path = cache_dir.join(format!("Terraria-{}.zip.part", uuid::Uuid::new_v4()));
    let _part_guard = TempFileGuard::new(part_path.clone());

    let client = build_download_client(proxy_url)?;
    let auth_token = if resolved.use_token { token } else { None };
    download_to_part_file(
        &client,
        DownloadSpec {
            url: &resolved.url,
            dest_part: &part_path,
            target: InstallTarget::Terraria,
            stage: "Đang tải gói cài đặt Terraria...",
            fallback_total: resolved.expected_size,
            accept_header: resolved.accept_header,
            bearer_token: auth_token,
        },
        &on_progress,
    )
    .await?;

    verify_zip_contains_required_file_blocking(part_path.clone(), "terraria.exe").await?;

    let extracted_top = extract_zip_safely_blocking(
        part_path,
        paths.terraria_dir.clone(),
        InstallTarget::Terraria,
        "Đang giải nén Terraria...",
        on_progress,
    )
    .await?;

    verify_terraria_installed(paths, &extracted_top)?;
    remove_installed_terraria_receipt(paths);

    Ok(format!(
        "Đã cài đặt Terraria vào {}.",
        paths.terraria_dir.display()
    ))
}

pub fn install_terraria_from_local_zip(
    paths: &LauncherPaths,
    zip_path: &Path,
    on_progress: ProgressCallback,
) -> LauncherResult<String> {
    if !zip_path.is_file() {
        return Err(LauncherError::NotFound(format!(
            "Không tìm thấy file nén Terraria tại {}",
            zip_path.display()
        )));
    }

    verify_zip_contains_required_file(zip_path, "terraria.exe")?;

    let extracted_top = extract_zip_safely(
        zip_path,
        &paths.terraria_dir,
        InstallTarget::Terraria,
        "Đang giải nén Terraria từ file cục bộ...",
        &on_progress,
    )?;

    verify_terraria_installed(paths, &extracted_top)?;
    remove_installed_terraria_receipt(paths);

    Ok(format!(
        "Đã giải nén và cài đặt Terraria vào {}.",
        paths.terraria_dir.display()
    ))
}

pub async fn download_and_install_xna(
    paths: &LauncherPaths,
    proxy_url: Option<&str>,
    on_progress: ProgressCallback,
) -> LauncherResult<()> {
    let bundled_msi = paths.xna_msi_path();
    if bundled_msi.is_file() {
        emit_progress(
            &on_progress,
            InstallTarget::Xna40,
            "Đang mở trình cài đặt XNA Framework 4.0...",
            1,
            Some(1),
        );
        return install_xna_framework_elevated(&bundled_msi);
    }

    let cache_dir = paths.data_dir.join("cache");
    fs::create_dir_all(&cache_dir)?;
    let cached_msi = cache_dir.join("xnafx40_redist.msi");
    let part_path = cache_dir.join(format!("xnafx40_redist.msi.{}.part", uuid::Uuid::new_v4()));
    let mut part_guard = TempFileGuard::new(part_path.clone());

    let client = build_download_client(proxy_url)?;
    download_to_part_file(
        &client,
        DownloadSpec {
            url: XNA_40_OFFICIAL_MSI_URL,
            dest_part: &part_path,
            target: InstallTarget::Xna40,
            stage: "Đang tải Microsoft XNA Framework 4.0...",
            fallback_total: None,
            accept_header: None,
            bearer_token: None,
        },
        &on_progress,
    )
    .await?;

    let _ = fs::remove_file(&cached_msi);
    fs::rename(&part_path, &cached_msi)?;
    part_guard.disarm();

    emit_progress(
        &on_progress,
        InstallTarget::Xna40,
        "Đang mở trình cài đặt XNA Framework 4.0...",
        1,
        Some(1),
    );
    install_xna_framework_elevated(&cached_msi)
}

fn format_reqwest_error_chain(err: &reqwest::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut current: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(err);
    while let Some(src) = current {
        let s = src.to_string();
        if !parts.last().is_some_and(|prev| prev == &s) {
            parts.push(s);
        }
        current = src.source();
    }
    let joined = parts.join(" | ");
    if err.is_timeout() && !joined.to_ascii_lowercase().contains("time") {
        format!("{joined} | timed out")
    } else {
        joined
    }
}

fn truncate_stage(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", truncated.trim_end())
}

pub fn sanitize_winget_stream_line(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1B {
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() && (0x20..=0x3F).contains(&bytes[i]) {
                    i += 1;
                }
                if i < bytes.len() && (0x40..=0x7E).contains(&bytes[i]) {
                    i += 1;
                }
            } else if i < bytes.len() && bytes[i] == b']' {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            } else if i < bytes.len() && bytes[i] < 0x80 {
                i += 1;
            }
            continue;
        }

        let Some(ch) = raw[i..].chars().next() else {
            break;
        };
        i += ch.len_utf8();
        if ch == '\t' {
            out.push(' ');
        } else if !ch.is_control() {
            out.push(ch);
        }
    }

    let trimmed = out.trim();
    if trimmed.is_empty() || matches!(trimmed, "-" | "\\" | "|" | "/") {
        return None;
    }

    let is_bar_noise = trimmed.chars().all(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '█' | '▒' | '░' | '▓' | '-' | '─' | '━' | '▀' | '▄' | '▌' | '▐'
            )
    });
    if is_bar_noise {
        return None;
    }

    Some(trimmed.to_string())
}

async fn read_winget_stream_lines<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    collected: Arc<std::sync::Mutex<Vec<String>>>,
    last_emitted: Arc<std::sync::Mutex<Option<String>>>,
    on_line: Option<Arc<dyn Fn(String) + Send + Sync>>,
) {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];

    let emit_segment = |segment: &[u8]| {
        let raw = String::from_utf8_lossy(segment);
        let Some(clean) = sanitize_winget_stream_line(&raw) else {
            return;
        };
        {
            let mut last = last_emitted.lock().unwrap_or_else(|e| e.into_inner());
            if last.as_deref() == Some(clean.as_str()) {
                return;
            }
            *last = Some(clean.clone());
        }
        {
            let mut list = collected.lock().unwrap_or_else(|e| e.into_inner());
            list.push(clean.clone());
        }
        if let Some(ref cb) = on_line {
            cb(clean);
        }
    };

    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                let mut start = 0;
                for idx in 0..buf.len() {
                    if buf[idx] == b'\r' || buf[idx] == b'\n' {
                        if idx > start {
                            emit_segment(&buf[start..idx]);
                        }
                        start = idx + 1;
                    }
                }
                if start > 0 {
                    buf.drain(..start);
                }
            }
            Err(_) => break,
        }
    }

    if !buf.is_empty() {
        emit_segment(&buf);
    }
}

fn winget_http_proxy(proxy_url: Option<&str>) -> Option<String> {
    let norm = proxy_url.and_then(normalize_proxy_url)?;
    let lower = norm.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        Some(norm)
    } else {
        None
    }
}

fn build_winget_steamcmd_command(proxy_url: Option<&str>) -> Command {
    let mut cmd = Command::new("winget");
    cmd.args([
        "install",
        "--id",
        "Valve.SteamCMD",
        "--exact",
        "--silent",
        "--accept-package-agreements",
        "--accept-source-agreements",
    ]);
    if let Some(norm) = winget_http_proxy(proxy_url) {
        cmd.arg("--proxy").arg(&norm);
    }
    crate::mods_manager::apply_proxy_env(&mut cmd, proxy_url);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

pub(crate) async fn run_winget_install_steamcmd(
    on_line: Option<Arc<dyn Fn(String) + Send + Sync>>,
    proxy_url: Option<&str>,
) -> LauncherResult<String> {
    let mut cmd = build_winget_steamcmd_command(proxy_url);

    let mut child = cmd
        .spawn()
        .map_err(|e| LauncherError::Process(format!("Không chạy được winget: {e}")))?;

    let collected = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let last_emitted = Arc::new(std::sync::Mutex::new(None::<String>));

    let stdout_task = child.stdout.take().map(|stdout| {
        tokio::spawn(read_winget_stream_lines(
            stdout,
            Arc::clone(&collected),
            Arc::clone(&last_emitted),
            on_line.clone(),
        ))
    });
    let stderr_task = child.stderr.take().map(|stderr| {
        tokio::spawn(read_winget_stream_lines(
            stderr,
            Arc::clone(&collected),
            Arc::clone(&last_emitted),
            on_line,
        ))
    });

    let status = child
        .wait()
        .await
        .map_err(|e| LauncherError::Process(format!("Lỗi chờ winget cài SteamCMD: {e}")))?;

    if let Some(h) = stdout_task {
        let _ = h.await;
    }
    if let Some(h) = stderr_task {
        let _ = h.await;
    }

    let lines = collected.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let joined = lines.join(" | ");

    if !status.success() {
        let detail = if joined.is_empty() {
            match status.code() {
                Some(code) => format!("mã thoát {code}"),
                None => "tiến trình bị ngắt".to_string(),
            }
        } else {
            joined
        };
        return Err(LauncherError::Process(format!(
            "winget cài SteamCMD thất bại: {detail}"
        )));
    }

    Ok(joined)
}

fn winget_output_indicates_failure(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("failed")
        || lower.contains("unexpected error")
        || lower.contains("no package found")
        || lower.contains("could not be resolved")
        || lower.contains("timed out")
        || lower.contains("connection refused")
        || lower.contains("thất bại")
}

fn format_steamcmd_combined_error(
    direct_err: &str,
    winget_err: &str,
    proxy_url: Option<&str>,
) -> LauncherError {
    let combined_lower = format!("{direct_err}\n{winget_err}").to_ascii_lowercase();
    let normalized_proxy = proxy_url.and_then(normalize_proxy_url);

    let is_proxy_err = normalized_proxy.is_some()
        || combined_lower.contains("proxy")
        || combined_lower.contains("socks");
    let is_refused_or_loopback = combined_lower.contains("connection refused")
        || combined_lower.contains("actively refused")
        || combined_lower.contains("os error 10061")
        || combined_lower.contains("127.0.0.1");
    let is_timeout = combined_lower.contains("timed out")
        || combined_lower.contains("timeout")
        || combined_lower.contains("deadline has elapsed")
        || combined_lower.contains("os error 10060");
    let is_dns = combined_lower.contains("dns")
        || combined_lower.contains("lookup address")
        || combined_lower.contains("no such host")
        || combined_lower.contains("failed to resolve")
        || combined_lower.contains("could not be resolved")
        || combined_lower.contains("os error 11001")
        || combined_lower.contains("os error 11004");

    let hint = if let Some(proxy) = normalized_proxy {
        if is_refused_or_loopback {
            format!(
                "Kết nối qua Proxy ({proxy}) bị từ chối (connection refused / 127.0.0.1). Hãy kiểm tra phần mềm Proxy/VPN đang chạy, hoặc tắt Proxy ở Tab Cài đặt rồi bật Cloudflare WARP (chế độ WARP)."
            )
        } else if is_timeout {
            format!(
                "Kết nối tới steamcdn-a.akamaihd.net qua Proxy ({proxy}) bị quá thời gian chờ (timed out). Hãy kiểm tra lại Proxy/VPN hoặc chuyển sang Cloudflare WARP (chế độ WARP)."
            )
        } else {
            format!(
                "Không thể tải từ steamcdn-a.akamaihd.net qua Proxy ({proxy}). Hãy kiểm tra cấu hình Proxy ở Tab Cài đặt hoặc bật Cloudflare WARP (chế độ WARP) / VPN."
            )
        }
    } else if is_refused_or_loopback {
        "Phát hiện lỗi từ chối kết nối hoặc chuyển hướng về 127.0.0.1 (connection refused) — rất có thể nhà mạng/DNS đang chặn tên miền steamcdn-a.akamaihd.net. Hãy bật Cloudflare WARP (chế độ WARP), dùng VPN, hoặc cấu hình Proxy ở Tab Cài đặt.".to_string()
    } else if is_timeout {
        "Kết nối tới steamcdn-a.akamaihd.net bị quá thời gian chờ (timed out), có thể nhà mạng đang chặn hoặc bóp băng thông tới Steam. Hãy bật Cloudflare WARP (chế độ WARP), dùng VPN, hoặc cấu hình Proxy ở Tab Cài đặt.".to_string()
    } else if is_dns {
        "Không phân giải được tên miền steamcdn-a.akamaihd.net (lỗi DNS), có thể nhà mạng/DNS đang chặn Steam. Hãy bật Cloudflare WARP (chế độ WARP), dùng VPN, hoặc cấu hình Proxy ở Tab Cài đặt.".to_string()
    } else if is_proxy_err {
        "Gặp lỗi Proxy khi kết nối tới steamcdn-a.akamaihd.net. Hãy kiểm tra lại Proxy hệ thống/Windows, hoặc bật Cloudflare WARP (chế độ WARP), VPN, hoặc cấu hình Proxy ở Tab Cài đặt.".to_string()
    } else {
        "Nếu nhà mạng hoặc DNS đang chặn steamcdn-a.akamaihd.net, hãy bật Cloudflare WARP (chế độ WARP), dùng VPN, hoặc thiết lập Proxy ở Tab Cài đặt.".to_string()
    };

    LauncherError::Network(format!(
        "Không thể cài đặt SteamCMD.\n- Tải trực tiếp (steamcdn-a.akamaihd.net): {direct_err}\n- Cài qua winget: {winget_err}\nGợi ý: {hint}"
    ))
}

async fn extract_and_verify_steamcmd(
    zip_path: PathBuf,
    steamcmd_dir: PathBuf,
    on_progress: ProgressCallback,
) -> LauncherResult<PathBuf> {
    let extracted_top = extract_zip_safely_blocking(
        zip_path,
        steamcmd_dir.clone(),
        InstallTarget::SteamCmd,
        "Đang giải nén SteamCMD...",
        on_progress,
    )
    .await?;

    let steamcmd_exe = steamcmd_dir.join("steamcmd.exe");
    if !steamcmd_exe.is_file() {
        let preview = format_top_level_preview(&extracted_top);
        return Err(LauncherError::InvalidFormat(format!(
            "Giải nén hoàn tất nhưng không thấy steamcmd.exe tại {}. Thư mục/tệp gốc trong file nén: {preview}",
            steamcmd_dir.display()
        )));
    }

    Ok(steamcmd_exe)
}

async fn install_steamcmd_from_direct_url(
    client: &Client,
    url: &str,
    paths: &LauncherPaths,
    on_progress: &ProgressCallback,
    on_log: &Arc<dyn Fn(String) + Send + Sync>,
) -> Result<Result<String, LauncherError>, LauncherError> {
    let cache_dir = paths.data_dir.join("cache");
    fs::create_dir_all(&cache_dir).map_err(LauncherError::from)?;
    let part_path = cache_dir.join(format!("steamcmd-{}.zip.part", uuid::Uuid::new_v4()));
    let _part_guard = TempFileGuard::new(part_path.clone());

    if let Err(download_err) = download_to_part_file(
        client,
        DownloadSpec {
            url,
            dest_part: &part_path,
            target: InstallTarget::SteamCmd,
            stage: "Đang tải SteamCMD...",
            fallback_total: None,
            accept_header: None,
            bearer_token: None,
        },
        on_progress,
    )
    .await
    {
        return Ok(Err(download_err));
    }

    on_log("Đang giải nén SteamCMD vào thư mục dữ liệu...".to_string());
    let steamcmd_exe =
        extract_and_verify_steamcmd(part_path, paths.steamcmd_dir.clone(), on_progress.clone())
            .await?;

    on_log(format!("Đã cài xong SteamCMD tại {}.", steamcmd_exe.display()));
    Ok(Ok(format!(
        "Đã cài đặt SteamCMD tại {}.",
        steamcmd_exe.display()
    )))
}

pub async fn install_steamcmd_with_progress(
    paths: &LauncherPaths,
    proxy_url: Option<&str>,
    on_progress: ProgressCallback,
    on_log: Arc<dyn Fn(String) + Send + Sync>,
) -> LauncherResult<String> {
    let client = build_download_client(proxy_url)?;
    on_log("Đang tải SteamCMD chính thức từ Valve (steamcdn-a.akamaihd.net)...".to_string());

    let direct_err = match install_steamcmd_from_direct_url(
        &client,
        STEAMCMD_OFFICIAL_ZIP_URL,
        paths,
        &on_progress,
        &on_log,
    )
    .await?
    {
        Ok(msg) => return Ok(msg),
        Err(err) => err,
    };

    let direct_err_msg = direct_err.to_string();
    on_log(format!(
        "Tải trực tiếp thất bại: {direct_err_msg}. Đang thử qua winget..."
    ));
    emit_progress(
        &on_progress,
        InstallTarget::SteamCmd,
        "Đang chạy winget cài Valve.SteamCMD...",
        0,
        None,
    );

    let log_cb = Arc::clone(&on_log);
    let progress_cb = Arc::clone(&on_progress);
    let stream_cb: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |line: String| {
        log_cb(line.clone());
        emit_progress(
            &progress_cb,
            InstallTarget::SteamCmd,
            truncate_stage(&line, 64),
            0,
            None,
        );
    });

    match run_winget_install_steamcmd(Some(stream_cb), proxy_url).await {
        Ok(winget_output) => {
            resolve_post_winget_steamcmd_inner(
                &client,
                STEAMCMD_OFFICIAL_ZIP_URL,
                paths,
                proxy_url,
                &direct_err_msg,
                &winget_output,
                &on_progress,
                &on_log,
                || find_steamcmd(paths, None),
            )
            .await
        }
        Err(winget_err) => Err(format_steamcmd_combined_error(
            &direct_err_msg,
            &winget_err.to_string(),
            proxy_url,
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn resolve_post_winget_steamcmd_inner<F>(
    client: &Client,
    direct_url: &str,
    paths: &LauncherPaths,
    proxy_url: Option<&str>,
    direct_err_msg: &str,
    winget_output: &str,
    on_progress: &ProgressCallback,
    on_log: &Arc<dyn Fn(String) + Send + Sync>,
    mut lookup_steamcmd: F,
) -> LauncherResult<String>
where
    F: FnMut() -> Option<PathBuf>,
{
    let found_path = lookup_steamcmd().or_else(&mut lookup_steamcmd);
    if let Some(ref exe) = found_path {
        on_log(format!("Đã cài xong SteamCMD tại {}.", exe.display()));
        emit_progress(
            on_progress,
            InstallTarget::SteamCmd,
            "Hoàn tất cài đặt SteamCMD",
            1,
            Some(1),
        );
        return Ok("Đã cài đặt SteamCMD qua winget.".to_string());
    }

    if winget_output_indicates_failure(winget_output) {
        return Err(format_steamcmd_combined_error(
            direct_err_msg,
            winget_output.trim(),
            proxy_url,
        ));
    }

    on_log(
        "winget không tạo được lệnh steamcmd khả dụng, đang thử tải gói chính thức vào thư mục Launcher..."
            .to_string(),
    );
    match install_steamcmd_from_direct_url(client, direct_url, paths, on_progress, on_log).await? {
        Ok(msg) => Ok(msg),
        Err(err) => Err(format_steamcmd_combined_error(
            &err.to_string(),
            "winget hoàn tất nhưng không tìm thấy steamcmd.exe khả dụng trong hệ thống.",
            proxy_url,
        )),
    }
}

fn verify_terraria_installed(
    paths: &LauncherPaths,
    extracted_top: &[String],
) -> LauncherResult<()> {
    if paths.terraria_exe().is_file() {
        return Ok(());
    }
    let preview = format_top_level_preview(extracted_top);
    Err(LauncherError::InvalidFormat(format!(
        "Giải nén hoàn tất nhưng không thấy Terraria.exe tại {}. Thư mục/tệp gốc trong file nén: {preview}",
        paths.terraria_dir.display()
    )))
}

fn format_top_level_preview(top_level: &[String]) -> String {
    if top_level.is_empty() {
        return "(trống)".to_string();
    }
    const LIMIT: usize = 8;
    if top_level.len() <= LIMIT {
        top_level.join(", ")
    } else {
        let head = top_level[..LIMIT].join(", ");
        format!("{head}, ... (+{})", top_level.len() - LIMIT)
    }
}

async fn extract_zip_safely_blocking(
    zip_path: PathBuf,
    target_dir: PathBuf,
    target: InstallTarget,
    stage: impl Into<String>,
    on_progress: ProgressCallback,
) -> LauncherResult<Vec<String>> {
    let stage_str = stage.into();
    tokio::task::spawn_blocking(move || {
        extract_zip_safely(&zip_path, &target_dir, target, &stage_str, &on_progress)
    })
    .await
    .map_err(|e| LauncherError::Process(format!("Lỗi tác vụ giải nén: {e}")))?
}

struct ZipEntryPlan {
    components: Vec<OsString>,
    is_dir: bool,
}

fn inspect_zip_layout<R: std::io::Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
) -> LauncherResult<(Vec<ZipEntryPlan>, bool)> {
    let total = archive.len();
    if total == 0 {
        return Err(LauncherError::InvalidFormat(
            "File nén rỗng, không có dữ liệu để giải nén.".to_string(),
        ));
    }

    let mut plans = Vec::with_capacity(total);
    let mut common_first: Option<OsString> = None;
    let mut all_same_first = true;
    let mut has_deeper_than_one = false;

    for i in 0..total {
        let entry = archive.by_index(i)?;
        let Some(safe_rel) = entry.enclosed_name() else {
            let unsafe_name = String::from_utf8_lossy(entry.name_raw());
            return Err(LauncherError::InvalidFormat(format!(
                "File nén chứa đường dẫn không an toàn: {unsafe_name}"
            )));
        };

        let components: Vec<OsString> = safe_rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_os_string()),
                _ => None,
            })
            .collect();

        if let Some(first) = components.first() {
            if components.len() > 1 {
                has_deeper_than_one = true;
            }
            match &common_first {
                None => common_first = Some(first.clone()),
                Some(prev) if prev != first => all_same_first = false,
                _ => {}
            }
        } else {
            all_same_first = false;
        }

        plans.push(ZipEntryPlan {
            components,
            is_dir: entry.is_dir(),
        });
    }

    let should_strip = match common_first {
        Some(top) if all_same_first && has_deeper_than_one => {
            let top_lower = top.to_string_lossy().to_ascii_lowercase();
            !PRESERVED_TOP_DIRS.contains(&top_lower.as_str())
        }
        _ => false,
    };

    Ok((plans, should_strip))
}

fn verify_zip_contains_required_file(zip_path: &Path, required_suffix: &str) -> LauncherResult<()> {
    let req_segments: Vec<String> = required_suffix
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    if req_segments.is_empty() {
        return Err(LauncherError::InvalidFormat(
            "Tên tệp bắt buộc trong file nén không hợp lệ.".to_string(),
        ));
    }

    let file = File::open(zip_path)?;
    let mut archive = ZipArchive::new(file)?;
    let (plans, should_strip) = inspect_zip_layout(&mut archive)?;

    let mut top_level_set = BTreeSet::new();
    let mut found = false;

    for plan in &plans {
        let rel_slice = if should_strip {
            plan.components.get(1..).unwrap_or(&[])
        } else {
            &plan.components[..]
        };
        if let Some(first) = rel_slice.first() {
            top_level_set.insert(first.to_string_lossy().into_owned());
        } else if let Some(first) = plan.components.first() {
            top_level_set.insert(first.to_string_lossy().into_owned());
        }

        if !plan.is_dir && plan.components.len() >= req_segments.len() {
            let tail = &plan.components[plan.components.len() - req_segments.len()..];
            let matches = tail
                .iter()
                .zip(req_segments.iter())
                .all(|(comp, req)| comp.to_string_lossy().to_ascii_lowercase() == *req);
            if matches {
                found = true;
            }
        }
    }

    if found {
        return Ok(());
    }

    let top_level: Vec<String> = top_level_set.into_iter().collect();
    let preview = format_top_level_preview(&top_level);
    let display_name = if required_suffix.eq_ignore_ascii_case("terraria.exe") {
        "Terraria.exe"
    } else if required_suffix.eq_ignore_ascii_case("tmodloader.dll") {
        "tModLoader.dll"
    } else {
        required_suffix
    };
    Err(LauncherError::InvalidFormat(format!(
        "File nén không chứa {display_name}. Thư mục/tệp gốc trong file nén: {preview}"
    )))
}

async fn verify_zip_contains_required_file_blocking(
    zip_path: PathBuf,
    required_suffix: &'static str,
) -> LauncherResult<()> {
    tokio::task::spawn_blocking(move || verify_zip_contains_required_file(&zip_path, required_suffix))
        .await
        .map_err(|e| LauncherError::Process(format!("Lỗi tác vụ kiểm tra file nén: {e}")))?
}

fn extract_zip_safely(
    zip_path: &Path,
    target_dir: &Path,
    target: InstallTarget,
    stage: &str,
    on_progress: &ProgressCallback,
) -> LauncherResult<Vec<String>> {
    let file = File::open(zip_path)?;
    let mut archive = ZipArchive::new(file)?;
    let (plans, should_strip) = inspect_zip_layout(&mut archive)?;

    fs::create_dir_all(target_dir)?;

    let total_entries = plans.len() as u64;
    let report_step = (total_entries / 100).max(1);
    let mut top_level_set = BTreeSet::new();

    for (idx, plan) in plans.iter().enumerate() {
        let rel_slice = if should_strip {
            plan.components.get(1..).unwrap_or(&[])
        } else {
            &plan.components[..]
        };

        if rel_slice.is_empty() {
            if should_emit_entry(idx, total_entries, report_step) {
                emit_progress(on_progress, target, stage, idx as u64 + 1, Some(total_entries));
            }
            continue;
        }

        if let Some(first) = rel_slice.first() {
            top_level_set.insert(first.to_string_lossy().into_owned());
        }

        let mut dest = target_dir.to_path_buf();
        for comp in rel_slice {
            dest.push(comp);
        }
        if !dest.starts_with(target_dir) {
            return Err(LauncherError::InvalidFormat(format!(
                "Đường dẫn giải nén vượt ra ngoài thư mục đích: {}",
                dest.display()
            )));
        }

        let mut entry = archive.by_index(idx)?;
        if plan.is_dir {
            drop(entry);
            fs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            let tmp_path = make_extract_temp_path(&dest, idx);
            let mut cleanup = ExtractTempCleanup::new(tmp_path.clone());
            {
                let mut out = File::create(&tmp_path)?;
                std::io::copy(&mut entry, &mut out)?;
                use std::io::Write;
                out.flush()?;
            }
            drop(entry);
            rename_extracted_file(&tmp_path, &dest)?;
            cleanup.disarm();
        }

        if should_emit_entry(idx, total_entries, report_step) {
            emit_progress(on_progress, target, stage, idx as u64 + 1, Some(total_entries));
        }
    }

    Ok(top_level_set.into_iter().collect())
}

struct ExtractTempCleanup(Option<PathBuf>);

impl ExtractTempCleanup {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for ExtractTempCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn make_extract_temp_path(dest: &Path, idx: usize) -> PathBuf {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let orig = dest
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("entry");
    let short_orig: String = orig.chars().take(40).collect();
    parent.join(format!(".{short_orig}.tl_extract_{}_{idx}", std::process::id()))
}

fn should_emit_entry(idx: usize, total_entries: u64, report_step: u64) -> bool {
    let current = idx as u64 + 1;
    idx == 0 || current == total_entries || current.is_multiple_of(report_step)
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn clear_readonly_attribute(dest: &Path) -> bool {
    if let Ok(meta) = fs::metadata(dest) {
        let mut perms = meta.permissions();
        if perms.readonly() {
            perms.set_readonly(false);
            return fs::set_permissions(dest, perms).is_ok();
        }
    }
    false
}

#[cfg(not(windows))]
fn clear_readonly_attribute(_dest: &Path) -> bool {
    false
}

fn rename_extracted_file(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    match fs::rename(tmp, dest) {
        Ok(()) => Ok(()),
        Err(err) => {
            if err.kind() == std::io::ErrorKind::PermissionDenied
                && clear_readonly_attribute(dest)
                && fs::rename(tmp, dest).is_ok()
            {
                return Ok(());
            }
            if is_file_locked_error(&err) {
                return Err(std::io::Error::new(
                    err.kind(),
                    format!(
                        "Không thể ghi đè '{}' vì file đang được tiến trình khác sử dụng. Hãy đóng Terraria / tModLoader / Server rồi thử lại.",
                        dest.display()
                    ),
                ));
            }
            Err(err)
        }
    }
}

fn is_file_locked_error(err: &std::io::Error) -> bool {
    if err.kind() == std::io::ErrorKind::PermissionDenied {
        return true;
    }
    #[cfg(windows)]
    if matches!(err.raw_os_error(), Some(5 | 32 | 33)) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn new(prefix: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("{prefix}_{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn paths(&self) -> LauncherPaths {
            let base_dir = self.0.clone();
            let data_dir = base_dir.join("data");
            let saves = data_dir.join("Saves");
            LauncherPaths {
                terraria_dir: base_dir.join("Terraria"),
                tmodloader_dir: base_dir.join("tModLoader"),
                vanilla_saves_dir: saves.join("Vanilla"),
                tml_saves_dir: saves.join("tModLoader"),
                servers_dir: data_dir.join("Servers"),
                backups_dir: data_dir.join("Backups"),
                steamcmd_dir: data_dir.join("SteamCMD"),
                config_path: data_dir.join("config.json"),
                data_dir,
                base_dir,
            }
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn record_progress() -> (ProgressCallback, Arc<Mutex<Vec<InstallProgress>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_clone = log.clone();
        let cb: ProgressCallback = Arc::new(move |p| {
            log_clone.lock().unwrap().push(p);
        });
        (cb, log)
    }

    fn write_test_zip(zip_path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(zip_path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for &(name, data) in entries {
            if name.ends_with('/') {
                writer.add_directory(name, options).unwrap();
            } else {
                writer.start_file(name, options).unwrap();
                writer.write_all(data).unwrap();
            }
        }
        writer.finish().unwrap();
    }

    #[test]
    fn parse_dotnet_runtime_version_reads_framework_and_frameworks_array() {
        let single = r#"{
          "runtimeOptions": {
            "tfm": "net8.0",
            "framework": {
              "name": "Microsoft.NETCore.App",
              "version": "8.0.0"
            }
          }
        }"#;
        assert_eq!(parse_dotnet_runtime_version(single).unwrap(), "8.0.0");

        let multi = r#"{
          "runtimeOptions": {
            "frameworks": [
              { "name": "Microsoft.WindowsDesktop.App", "version": "8.0.11" },
              { "name": "Microsoft.NETCore.App", "version": "8.0.11" }
            ]
          }
        }"#;
        assert_eq!(parse_dotnet_runtime_version(multi).unwrap(), "8.0.11");

        let prerelease = r#"{
          "runtimeOptions": {
            "framework": { "version": "8.0.0-rc.2" }
          }
        }"#;
        assert_eq!(parse_dotnet_runtime_version(prerelease).unwrap(), "8.0.0-rc.2");
    }

    #[test]
    fn parse_dotnet_runtime_version_rejects_invalid_inputs() {
        assert!(parse_dotnet_runtime_version("{not json").is_err());
        assert!(parse_dotnet_runtime_version(r#"{"other": 1}"#).is_err());
        assert!(parse_dotnet_runtime_version(r#"{"runtimeOptions": {}}"#).is_err());
        assert!(parse_dotnet_runtime_version(
            r#"{"runtimeOptions": {"framework": {"version": ""}}}"#
        )
        .is_err());
        assert!(parse_dotnet_runtime_version(
            r#"{"runtimeOptions": {"framework": {"version": "abc"}}}"#
        )
        .is_err());
        assert!(parse_dotnet_runtime_version(
            r#"{"runtimeOptions": {"framework": {"version": "8.0/../evil"}}}"#
        )
        .is_err());
    }

    #[test]
    fn read_required_tml_dotnet_version_reads_config_or_falls_back() {
        let guard = TempDirGuard::new("tl_gi_req_dotnet");
        let paths = guard.paths();
        fs::create_dir_all(&paths.tmodloader_dir).unwrap();

        assert_eq!(read_required_tml_dotnet_version(&paths), "8.0.0");

        let cfg_path = paths.tmodloader_dir.join("tModLoader.runtimeconfig.json");
        fs::write(&cfg_path, "{broken json").unwrap();
        assert_eq!(read_required_tml_dotnet_version(&paths), "8.0.0");

        fs::write(
            &cfg_path,
            r#"{"runtimeOptions":{"framework":{"name":"Microsoft.NETCore.App","version":"8.0.15"}}}"#,
        )
        .unwrap();
        assert_eq!(read_required_tml_dotnet_version(&paths), "8.0.15");
    }

    #[test]
    fn winget_http_proxy_normalizes_and_filters_schemes() {
        assert_eq!(winget_http_proxy(None), None);
        assert_eq!(winget_http_proxy(Some("   ")), None);
        assert_eq!(
            winget_http_proxy(Some(" 127.0.0.1:7890 ")),
            Some("http://127.0.0.1:7890".to_string())
        );
        assert_eq!(
            winget_http_proxy(Some("https://proxy.example:8443")),
            Some("https://proxy.example:8443".to_string())
        );
        assert_eq!(winget_http_proxy(Some("socks5://127.0.0.1:1080")), None);

        let inspect_cmd = |proxy| {
            let cmd = build_winget_steamcmd_command(proxy);
            let args: Vec<_> = cmd
                .as_std()
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let envs: std::collections::HashMap<String, String> = cmd
                .as_std()
                .get_envs()
                .filter_map(|(k, v)| {
                    Some((
                        k.to_string_lossy().into_owned(),
                        v?.to_string_lossy().into_owned(),
                    ))
                })
                .collect();
            (args, envs)
        };

        let (http_args, http_envs) = inspect_cmd(Some("127.0.0.1:7890"));
        assert!(http_args
            .windows(2)
            .any(|w| w == ["--proxy", "http://127.0.0.1:7890"]));
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
            assert_eq!(
                http_envs.get(key).map(String::as_str),
                Some("http://127.0.0.1:7890")
            );
        }

        let (socks_args, socks_envs) = inspect_cmd(Some("socks5://127.0.0.1:1080"));
        assert!(!socks_args.iter().any(|a| a == "--proxy"));
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
            assert_eq!(
                socks_envs.get(key).map(String::as_str),
                Some("socks5://127.0.0.1:1080")
            );
        }
    }

    #[test]
    fn parse_github_release_spec_handles_all_supported_and_negative_inputs() {
        assert_eq!(
            parse_github_release_spec("owner/repo"),
            Some(("owner/repo".to_string(), None))
        );
        assert_eq!(
            parse_github_release_spec("  owner/repo@v1.4.5.8  "),
            Some(("owner/repo".to_string(), Some("v1.4.5.8".to_string())))
        );
        assert_eq!(
            parse_github_release_spec("https://github.com/owner/repo"),
            Some(("owner/repo".to_string(), None))
        );
        assert_eq!(
            parse_github_release_spec("https://github.com/owner/repo/"),
            Some(("owner/repo".to_string(), None))
        );
        assert_eq!(
            parse_github_release_spec("https://github.com/owner/repo/releases"),
            Some(("owner/repo".to_string(), None))
        );
        assert_eq!(
            parse_github_release_spec("https://github.com/owner/repo/releases/latest"),
            Some(("owner/repo".to_string(), None))
        );
        assert_eq!(
            parse_github_release_spec("https://github.com/owner/repo/releases/tag/v1.4.5.8"),
            Some(("owner/repo".to_string(), Some("v1.4.5.8".to_string())))
        );

        assert_eq!(
            parse_github_release_spec(
                "https://github.com/owner/repo/releases/download/v1.0/Terraria.zip"
            ),
            None
        );
        assert_eq!(
            parse_github_release_spec("https://example.com/Terraria.zip"),
            None
        );
        assert_eq!(
            parse_github_release_spec("https://gitlab.com/owner/repo"),
            None
        );
        assert_eq!(parse_github_release_spec("owner/repo@"), None);
        assert_eq!(parse_github_release_spec("single-word"), None);
        assert_eq!(parse_github_release_spec(""), None);
    }

    #[test]
    fn install_terraria_from_local_zip_unwraps_single_wrapper_dir() {
        let guard = TempDirGuard::new("tl_gi_wrapped");
        let paths = guard.paths();
        let zip_path = guard.0.join("wrapped.zip");
        write_test_zip(
            &zip_path,
            &[
                ("Terraria/", b""),
                ("Terraria/Terraria.exe", b"MZ-TERRARIA"),
                ("Terraria/Content/Images/Logo.png", b"PNGDATA"),
            ],
        );

        let (cb, log) = record_progress();
        let res = install_terraria_from_local_zip(&paths, &zip_path, cb);
        assert!(res.is_ok(), "expected ok, got: {res:?}");
        assert!(paths.terraria_exe().is_file());
        assert_eq!(
            fs::read(paths.terraria_dir.join("Content/Images/Logo.png")).unwrap(),
            b"PNGDATA"
        );
        assert!(!paths.terraria_dir.join("Terraria").exists());

        let events = log.lock().unwrap();
        assert!(!events.is_empty());
        assert_eq!(events.last().unwrap().fraction(), Some(1.0));
    }

    #[test]
    fn install_terraria_from_local_zip_preserves_unwrapped_layout() {
        let guard = TempDirGuard::new("tl_gi_unwrapped");
        let paths = guard.paths();
        let zip_path = guard.0.join("unwrapped.zip");
        write_test_zip(
            &zip_path,
            &[
                ("Terraria.exe", b"MZ-TERRARIA"),
                ("Content/Fonts/Item.xnb", b"XNB"),
                ("__redist/XNA_40/xnafx40_redist.msi", b"MSI"),
            ],
        );

        let (cb, _) = record_progress();
        let res = install_terraria_from_local_zip(&paths, &zip_path, cb);
        assert!(res.is_ok(), "expected ok, got: {res:?}");
        assert!(paths.terraria_exe().is_file());
        assert!(paths.terraria_dir.join("Content/Fonts/Item.xnb").is_file());
        assert!(paths.xna_msi_path().is_file());
    }

    #[test]
    fn install_terraria_from_local_zip_rejects_zip_slip_traversal() {
        let guard = TempDirGuard::new("tl_gi_zipslip");
        let paths = guard.paths();
        let zip_path = guard.0.join("evil.zip");
        write_test_zip(
            &zip_path,
            &[
                ("Terraria.exe", b"MZ-TERRARIA"),
                ("../escape.txt", b"PWNED"),
            ],
        );

        let (cb, _) = record_progress();
        let err = install_terraria_from_local_zip(&paths, &zip_path, cb).unwrap_err();
        assert!(matches!(err, LauncherError::InvalidFormat(_)));
        assert!(!guard.0.join("escape.txt").exists());
        assert!(!paths.terraria_exe().exists());
    }

    #[test]
    fn install_terraria_from_local_zip_rejects_missing_terraria_exe() {
        let guard = TempDirGuard::new("tl_gi_missing_exe");
        let paths = guard.paths();
        let zip_path = guard.0.join("bad_layout.zip");
        write_test_zip(
            &zip_path,
            &[
                ("MacOS/Terraria.bin", b"BIN"),
                ("README.txt", b"hello"),
            ],
        );

        let (cb, _) = record_progress();
        let err = install_terraria_from_local_zip(&paths, &zip_path, cb).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, LauncherError::InvalidFormat(_)));
        assert!(msg.contains("Terraria.exe"));
        assert!(msg.contains("MacOS") && msg.contains("README.txt"));
    }

    #[test]
    fn install_progress_fraction_and_preserved_dotnet_dirs() {
        let p_none = InstallProgress {
            target: InstallTarget::Terraria,
            stage: "init".into(),
            current: 5,
            total: None,
        };
        assert_eq!(p_none.fraction(), None);

        let p_zero = InstallProgress {
            target: InstallTarget::Xna40,
            stage: "init".into(),
            current: 5,
            total: Some(0),
        };
        assert_eq!(p_zero.fraction(), None);

        let p_half = InstallProgress {
            target: InstallTarget::TModLoader,
            stage: "dl".into(),
            current: 50,
            total: Some(100),
        };
        assert_eq!(p_half.fraction(), Some(0.5));

        let guard = TempDirGuard::new("tl_gi_dotnet_layout");
        let dotnet_dir = guard.0.join("dotnet");
        let zip_path = guard.0.join("dotnet_host_only.zip");
        write_test_zip(
            &zip_path,
            &[("host/fxr/8.0.0/hostfxr.dll", b"HOSTFXR")],
        );
        let (cb, _) = record_progress();
        extract_zip_safely(&zip_path, &dotnet_dir, InstallTarget::DotNet8, "extract", &cb).unwrap();
        assert!(dotnet_dir.join("host/fxr/8.0.0/hostfxr.dll").is_file());
    }

    fn build_in_memory_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            for &(name, data) in entries {
                if name.ends_with('/') {
                    writer.add_directory(name, options).unwrap();
                } else {
                    writer.start_file(name, options).unwrap();
                    writer.write_all(data).unwrap();
                }
            }
            writer.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn build_two_part_manifest(zip_bytes: &[u8]) -> (TerrariaBranchManifest, Vec<u8>, Vec<u8>) {
        let split_at = zip_bytes.len() / 2;
        let part1 = zip_bytes[..split_at].to_vec();
        let part2 = zip_bytes[split_at..].to_vec();
        let manifest = TerrariaBranchManifest {
            version: "1.4.5.8".to_string(),
            archive_name: "Terraria_v1.4.5.8.zip".to_string(),
            total_size: zip_bytes.len() as u64,
            sha256: sha256_hex(zip_bytes),
            chunk_size: split_at as u64,
            parts: vec![
                TerrariaManifestPart {
                    file: "Terraria.zip.001".to_string(),
                    size: part1.len() as u64,
                    sha256: sha256_hex(&part1),
                },
                TerrariaManifestPart {
                    file: "Terraria.zip.002".to_string(),
                    size: part2.len() as u64,
                    sha256: sha256_hex(&part2),
                },
            ],
        };
        (manifest, part1, part2)
    }

    #[test]
    fn parse_github_branch_spec_handles_all_supported_and_negative_inputs() {
        assert_eq!(
            parse_github_branch_spec("owner/repo#terraria-game"),
            Some(("owner/repo".to_string(), "terraria-game".to_string(), None))
        );
        assert_eq!(
            parse_github_branch_spec("  PhamTriHien/TriHienKun_Terraria_Laucher#terraria-game  "),
            Some((
                "PhamTriHien/TriHienKun_Terraria_Laucher".to_string(),
                "terraria-game".to_string(),
                None
            ))
        );
        assert_eq!(
            parse_github_branch_spec("owner/repo#branch:subdir"),
            Some((
                "owner/repo".to_string(),
                "branch".to_string(),
                Some("subdir".to_string())
            ))
        );
        assert_eq!(
            parse_github_branch_spec("owner/repo#branch:/nested/subdir/"),
            Some((
                "owner/repo".to_string(),
                "branch".to_string(),
                Some("nested/subdir".to_string())
            ))
        );
        assert_eq!(
            parse_github_branch_spec("owner/repo#branch:"),
            Some(("owner/repo".to_string(), "branch".to_string(), None))
        );
        assert_eq!(
            parse_github_branch_spec("owner/repo#branch:///"),
            Some(("owner/repo".to_string(), "branch".to_string(), None))
        );
        assert_eq!(
            parse_github_branch_spec("owner/repo#release/v1.4.5:dist"),
            Some((
                "owner/repo".to_string(),
                "release/v1.4.5".to_string(),
                Some("dist".to_string())
            ))
        );
        assert_eq!(
            parse_github_branch_spec("https://github.com/owner/repo/tree/terraria-game"),
            Some(("owner/repo".to_string(), "terraria-game".to_string(), None))
        );
        assert_eq!(
            parse_github_branch_spec("https://www.github.com/owner/repo/tree/feature/sub/"),
            Some(("owner/repo".to_string(), "feature/sub".to_string(), None))
        );

        assert_eq!(parse_github_branch_spec("owner/repo"), None);
        assert_eq!(parse_github_branch_spec("owner/repo@v1.4.5.8"), None);
        assert_eq!(parse_github_branch_spec("https://github.com/owner/repo"), None);
        assert_eq!(
            parse_github_branch_spec("https://github.com/owner/repo/releases/tag/v1.4.5.8"),
            None
        );
        assert_eq!(
            parse_github_branch_spec("https://example.com/Terraria.zip"),
            None
        );
        assert_eq!(
            parse_github_branch_spec("https://example.com/terraria-manifest.json"),
            None
        );
        assert_eq!(parse_github_branch_spec("owner/repo#"), None);
        assert_eq!(parse_github_branch_spec("/repo#branch"), None);
        assert_eq!(parse_github_branch_spec("owner/#branch"), None);
        assert_eq!(parse_github_branch_spec("owner/repo#my branch"), None);
        assert_eq!(parse_github_branch_spec("owner/repo#branch@tag"), None);
        assert_eq!(parse_github_branch_spec("owner/repo#branch?x=1"), None);
        assert_eq!(parse_github_branch_spec("owner/repo#branch:../evil"), None);
        assert_eq!(parse_github_branch_spec("owner/repo#../evil"), None);
        assert_eq!(
            parse_github_branch_spec("https://github.com/owner/repo/tree/"),
            None
        );
        assert_eq!(
            parse_github_branch_spec("https://github.com/owner/repo/tree/../evil"),
            None
        );
        assert_eq!(
            parse_github_branch_spec("https://github.com/owner/repo/tree/branch?x=1"),
            None
        );
        assert_eq!(
            parse_github_branch_spec("https://gitlab.com/owner/repo/tree/main"),
            None
        );
    }

    #[test]
    fn manifest_origin_builds_expected_github_and_direct_urls() {
        let public_origin = ManifestOrigin::resolve("owner/repo#terraria-game").unwrap();
        let pub_manifest = public_origin.manifest_request(None);
        assert_eq!(
            pub_manifest.url,
            "https://raw.githubusercontent.com/owner/repo/terraria-game/terraria-manifest.json"
        );
        assert_eq!(pub_manifest.accept_header, None);
        assert_eq!(pub_manifest.bearer_token, None);

        let subdir_origin = ManifestOrigin::resolve("owner/repo#main:bundle/win64").unwrap();
        let priv_manifest = subdir_origin.manifest_request(Some("tok_123"));
        assert_eq!(
            priv_manifest.url,
            "https://api.github.com/repos/owner/repo/contents/bundle/win64/terraria-manifest.json?ref=main"
        );
        assert_eq!(
            priv_manifest.accept_header,
            Some("application/vnd.github.raw+json")
        );
        assert_eq!(priv_manifest.bearer_token.as_deref(), Some("tok_123"));

        let priv_part = subdir_origin.part_request("Terraria.zip.001", Some("tok_123"));
        assert_eq!(
            priv_part.url,
            "https://api.github.com/repos/owner/repo/contents/bundle/win64/Terraria.zip.001?ref=main"
        );
        assert_eq!(
            priv_part.accept_header,
            Some("application/vnd.github.raw+json")
        );
        assert_eq!(priv_part.bearer_token.as_deref(), Some("tok_123"));

        assert!(ManifestOrigin::resolve("https://example.com/not-terraria-manifest.json").is_none());
        let direct =
            ManifestOrigin::resolve("https://cdn.example.com/dl/terraria-manifest.json?v=1#top")
                .unwrap();
        assert_eq!(
            direct.manifest_request(Some("tok_123")).url,
            "https://cdn.example.com/dl/terraria-manifest.json?v=1"
        );
        assert_eq!(direct.manifest_request(Some("tok_123")).bearer_token, None);
        assert_eq!(
            direct.part_request("Terraria.zip.001", None).url,
            "https://cdn.example.com/dl/Terraria.zip.001"
        );
    }

    #[test]
    fn validate_branch_manifest_rejects_unsafe_and_malformed_manifests() {
        let valid_hash = "a".repeat(64);
        let valid = TerrariaBranchManifest {
            version: "1.4.5.8".to_string(),
            archive_name: "Terraria.zip".to_string(),
            total_size: 1024,
            sha256: valid_hash.clone(),
            chunk_size: 512,
            parts: vec![TerrariaManifestPart {
                file: "Terraria.zip.001".to_string(),
                size: 1024,
                sha256: valid_hash,
            }],
        };
        assert!(validate_branch_manifest(&valid).is_ok());

        let mut empty_parts = valid.clone();
        empty_parts.parts.clear();
        assert!(matches!(
            validate_branch_manifest(&empty_parts),
            Err(LauncherError::InvalidFormat(_))
        ));

        let mut zero_total = valid.clone();
        zero_total.total_size = 0;
        assert!(matches!(
            validate_branch_manifest(&zero_total),
            Err(LauncherError::InvalidFormat(_))
        ));

        let mut empty_ver = valid.clone();
        empty_ver.version = "   ".to_string();
        assert!(matches!(
            validate_branch_manifest(&empty_ver),
            Err(LauncherError::InvalidFormat(_))
        ));

        for bad_file in ["../escape", "sub/part.001", r"sub\part.001", "a..b", ".", ""] {
            let mut m = valid.clone();
            m.parts[0].file = bad_file.to_string();
            assert!(
                matches!(
                    validate_branch_manifest(&m),
                    Err(LauncherError::InvalidFormat(_))
                ),
                "expected rejection for part file '{bad_file}'"
            );
        }

        let mut bad_part_sha = valid.clone();
        bad_part_sha.parts[0].sha256 = "not-valid-hex".to_string();
        assert!(matches!(
            validate_branch_manifest(&bad_part_sha),
            Err(LauncherError::InvalidFormat(_))
        ));

        let mut mismatched_sum = valid.clone();
        mismatched_sum.parts[0].size = 512;
        assert!(matches!(
            validate_branch_manifest(&mismatched_sum),
            Err(LauncherError::InvalidFormat(_))
        ));

        let mut zero_part_size = valid;
        zero_part_size.parts[0].size = 0;
        assert!(matches!(
            validate_branch_manifest(&zero_part_size),
            Err(LauncherError::InvalidFormat(_))
        ));
    }

    #[test]
    fn multipart_assembly_verifies_sha256_extracts_and_manages_receipt() {
        let zip_bytes = build_in_memory_zip(&[
            ("Terraria/", b""),
            ("Terraria/Terraria.exe", b"MZ-TERRARIA-1458"),
            ("Terraria/Content/Images/Logo.png", b"PNG-CHUNKED"),
        ]);
        let (manifest, part1, part2) = build_two_part_manifest(&zip_bytes);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let guard = TempDirGuard::new("tl_gi_multipart_ok");
            let paths = guard.paths();
            let (cb, log) = record_progress();
            let parts_data = [part1.clone(), part2.clone()];

            let msg = install_terraria_from_manifest_streams(
                &paths,
                "owner/repo#terraria-game",
                &manifest,
                cb,
                |idx, _part| {
                    let data = parts_data[idx].clone();
                    async move {
                        let mid = data.len() / 2;
                        let chunks = vec![Ok(data[..mid].to_vec()), Ok(data[mid..].to_vec())];
                        Ok(futures_util::stream::iter(chunks))
                    }
                },
            )
            .await
            .unwrap();

            assert!(msg.contains("1.4.5.8"));
            assert!(paths.terraria_exe().is_file());
            assert_eq!(
                fs::read(paths.terraria_dir.join("Content/Images/Logo.png")).unwrap(),
                b"PNG-CHUNKED"
            );

            let receipt = load_installed_terraria_receipt(&paths).unwrap();
            assert_eq!(
                receipt,
                InstalledTerrariaReceipt {
                    version: "1.4.5.8".to_string(),
                    sha256: manifest.sha256.clone(),
                    source: "owner/repo#terraria-game".to_string(),
                    install_dir: paths.terraria_dir.to_string_lossy().into_owned(),
                }
            );

            let events = log.lock().unwrap();
            assert!(events.iter().any(|e| e.stage.contains("(1/2)")));
            assert!(events.iter().any(|e| e.stage.contains("(2/2)")));
            assert_eq!(events.last().unwrap().fraction(), Some(1.0));

            fs::write(installed_terraria_receipt_path(&paths), "{corrupt json").unwrap();
            assert_eq!(load_installed_terraria_receipt(&paths), None);

            save_installed_terraria_receipt(&paths, &receipt).unwrap();
            assert_eq!(load_installed_terraria_receipt(&paths), Some(receipt));
            fs::remove_file(paths.terraria_exe()).unwrap();
            assert_eq!(load_installed_terraria_receipt(&paths), None);
        });

        rt.block_on(async {
            let guard = TempDirGuard::new("tl_gi_multipart_bad_hash");
            let paths = guard.paths();
            let (cb, _) = record_progress();
            let mut corrupted_part2 = part2.clone();
            corrupted_part2[0] ^= 0xFF;
            let parts_data = [part1.clone(), corrupted_part2];

            let err = install_terraria_from_manifest_streams(
                &paths,
                "owner/repo#terraria-game",
                &manifest,
                cb,
                |idx, _part| {
                    let data = parts_data[idx].clone();
                    async move {
                        Ok(futures_util::stream::iter(vec![Ok(data)]))
                    }
                },
            )
            .await
            .unwrap_err();

            assert!(matches!(err, LauncherError::InvalidFormat(_)));
            assert!(err.to_string().contains("Terraria.zip.002"));
            assert!(!paths.terraria_exe().exists());
            assert_eq!(load_installed_terraria_receipt(&paths), None);
        });

        rt.block_on(async {
            let guard = TempDirGuard::new("tl_gi_multipart_bad_full_hash");
            let paths = guard.paths();
            let (cb, _) = record_progress();
            let mut bad_full_manifest = manifest.clone();
            bad_full_manifest.sha256 = "0".repeat(64);
            let parts_data = [part1, part2];

            let err = install_terraria_from_manifest_streams(
                &paths,
                "owner/repo#terraria-game",
                &bad_full_manifest,
                cb,
                |idx, _part| {
                    let data = parts_data[idx].clone();
                    async move { Ok(futures_util::stream::iter(vec![Ok(data)])) }
                },
            )
            .await
            .unwrap_err();

            assert!(matches!(err, LauncherError::InvalidFormat(_)));
            assert!(err.to_string().contains("SHA-256"));
            assert!(!paths.terraria_exe().exists());
        });
    }

    #[test]
    fn fetch_and_install_from_local_manifest_http_server_end_to_end() {
        let zip_bytes = build_in_memory_zip(&[
            ("Terraria.exe", b"MZ-HTTP-TERRARIA"),
            ("Content/Data.xnb", b"XNB-HTTP"),
        ]);
        let (manifest, part1, part2) = build_two_part_manifest(&zip_bytes);
        let manifest_json = serde_json::to_vec(&manifest).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();

            let server = tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    let manifest_json = manifest_json.clone();
                    let part1 = part1.clone();
                    let part2 = part2.clone();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        let n = socket.read(&mut buf).await.unwrap_or(0);
                        let req_text = String::from_utf8_lossy(&buf[..n]);
                        let target_path = req_text
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/");
                        let path_only = target_path.split('?').next().unwrap_or(target_path);

                        let (status, body): (&str, Vec<u8>) = match path_only {
                            "/game/terraria-manifest.json" => ("200 OK", manifest_json),
                            "/game/Terraria.zip.001" => ("200 OK", part1),
                            "/game/Terraria.zip.002" => ("200 OK", part2),
                            "/bad/terraria-manifest.json" => ("200 OK", b"{not-json".to_vec()),
                            "/unauth/terraria-manifest.json" => {
                                ("401 Unauthorized", b"unauthorized".to_vec())
                            }
                            "/forbidden/terraria-manifest.json" => {
                                ("403 Forbidden", b"forbidden".to_vec())
                            }
                            _ => ("404 Not Found", b"not found".to_vec()),
                        };

                        let header = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(header.as_bytes()).await;
                        let _ = socket.write_all(&body).await;
                        let _ = socket.shutdown().await;
                    });
                }
            });

            let manifest_url = format!("http://127.0.0.1:{port}/game/terraria-manifest.json?v=1");
            let fetched = fetch_terraria_branch_manifest(&manifest_url, None, None)
                .await
                .unwrap();
            assert_eq!(fetched, manifest);

            let missing_url = format!("http://127.0.0.1:{port}/missing/terraria-manifest.json");
            let missing_err = fetch_terraria_branch_manifest(&missing_url, None, None)
                .await
                .unwrap_err();
            assert!(matches!(missing_err, LauncherError::NotFound(_)));
            assert!(missing_err.to_string().contains("404"));

            let unauth_url = format!("http://127.0.0.1:{port}/unauth/terraria-manifest.json");
            let unauth_err = fetch_terraria_branch_manifest(&unauth_url, None, None)
                .await
                .unwrap_err();
            assert!(matches!(unauth_err, LauncherError::Network(_)));
            assert!(unauth_err.to_string().contains("401"));

            let forbidden_url =
                format!("http://127.0.0.1:{port}/forbidden/terraria-manifest.json");
            let forbidden_err = fetch_terraria_branch_manifest(&forbidden_url, None, None)
                .await
                .unwrap_err();
            assert!(matches!(forbidden_err, LauncherError::Network(_)));
            assert!(forbidden_err.to_string().contains("403"));

            let bad_url = format!("http://127.0.0.1:{port}/bad/terraria-manifest.json");
            let bad_err = fetch_terraria_branch_manifest(&bad_url, None, None)
                .await
                .unwrap_err();
            assert!(matches!(bad_err, LauncherError::InvalidFormat(_)));

            let guard = TempDirGuard::new("tl_gi_http_manifest_install");
            let paths = guard.paths();
            let (cb, _) = record_progress();

            let msg = install_terraria_from_source(&paths, &manifest_url, None, None, cb.clone())
                .await
                .unwrap();
            assert!(msg.contains("1.4.5.8"));
            assert!(paths.terraria_exe().is_file());
            assert_eq!(
                fs::read(paths.terraria_dir.join("Content/Data.xnb")).unwrap(),
                b"XNB-HTTP"
            );

            let receipt = load_installed_terraria_receipt(&paths).unwrap();
            assert_eq!(receipt.version, "1.4.5.8");
            assert_eq!(receipt.sha256, manifest.sha256);
            assert_eq!(receipt.source, manifest_url);

            let local_zip = guard.0.join("override.zip");
            write_test_zip(&local_zip, &[("Terraria.exe", b"MZ-LOCAL-ZIP")]);
            install_terraria_from_local_zip(&paths, &local_zip, cb).unwrap();
            assert_eq!(load_installed_terraria_receipt(&paths), None);

            server.abort();
        });
    }

    #[test]
    fn steamcmd_target_label_and_official_url() {
        assert_eq!(InstallTarget::SteamCmd.label(), "SteamCMD");
        assert_eq!(
            STEAMCMD_OFFICIAL_ZIP_URL,
            "https://steamcdn-a.akamaihd.net/client/installer/steamcmd.zip"
        );
    }

    #[test]
    fn sanitize_winget_stream_line_filters_ansi_spinners_and_progress_bars() {
        assert_eq!(sanitize_winget_stream_line(""), None);
        assert_eq!(sanitize_winget_stream_line("   \t  "), None);
        assert_eq!(sanitize_winget_stream_line("\x1b[2K\x1b[1G"), None);
        assert_eq!(sanitize_winget_stream_line("\x1b]9;4;1;50\x07"), None);

        for spinner in ["-", "\\", "|", "/", "\x1b[?25l  /  \x1b[?25h", "\x08|"] {
            assert_eq!(
                sanitize_winget_stream_line(spinner),
                None,
                "expected spinner '{spinner}' to be ignored"
            );
        }

        for noise in [
            "██████████░░░░░░░░░░",
            "▒▒▒▓▓▓",
            "----------",
            "───━━━",
            "\x1b[32m████▒▒░░\x1b[0m",
        ] {
            assert_eq!(
                sanitize_winget_stream_line(noise),
                None,
                "expected noise '{noise}' to be ignored"
            );
        }

        assert_eq!(
            sanitize_winget_stream_line("\x1b[32mFound SteamCMD [Valve.SteamCMD]\x1b[0m"),
            Some("Found SteamCMD [Valve.SteamCMD]".to_string())
        );
        assert_eq!(
            sanitize_winget_stream_line(
                "  \x1b[1mDownloading https://steamcdn-a.akamaihd.net/client/installer/steamcmd.zip\x1b[0m  "
            ),
            Some("Downloading https://steamcdn-a.akamaihd.net/client/installer/steamcmd.zip".to_string())
        );
        assert_eq!(
            sanitize_winget_stream_line("  1.45 MB / 2.90 MB  "),
            Some("1.45 MB / 2.90 MB".to_string())
        );
        assert_eq!(
            sanitize_winget_stream_line("\x1b[36mĐang giải nén dữ liệu...\x1b[0m"),
            Some("Đang giải nén dữ liệu...".to_string())
        );
    }

    #[test]
    fn extract_steamcmd_zip_verifies_progress_and_rejects_missing_exe() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let guard = TempDirGuard::new("tl_gi_steamcmd_ok");
            let paths = guard.paths();
            let zip_path = guard.0.join("steamcmd.zip");
            write_test_zip(
                &zip_path,
                &[
                    ("steamcmd.exe", b"MZ-STEAMCMD"),
                    ("public/steam.cfg", b"BootStrapperInhibitAll=enable"),
                ],
            );

            let (cb, log) = record_progress();
            let exe = extract_and_verify_steamcmd(zip_path, paths.steamcmd_dir.clone(), cb)
                .await
                .unwrap();

            assert_eq!(exe, paths.steamcmd_dir.join("steamcmd.exe"));
            assert!(exe.is_file());
            assert_eq!(fs::read(&exe).unwrap(), b"MZ-STEAMCMD");
            assert_eq!(
                fs::read(paths.steamcmd_dir.join("public/steam.cfg")).unwrap(),
                b"BootStrapperInhibitAll=enable"
            );

            {
                let events = log.lock().unwrap();
                assert!(!events.is_empty());
                assert!(events.iter().all(|e| e.target == InstallTarget::SteamCmd));
                assert_eq!(events.last().unwrap().fraction(), Some(1.0));
            }

            let bad_guard = TempDirGuard::new("tl_gi_steamcmd_missing");
            let bad_paths = bad_guard.paths();
            let bad_zip = bad_guard.0.join("not_steamcmd.zip");
            write_test_zip(
                &bad_zip,
                &[("README.txt", b"missing binary"), ("bin/other.exe", b"MZ")],
            );

            let (bad_cb, _) = record_progress();
            let err = extract_and_verify_steamcmd(bad_zip, bad_paths.steamcmd_dir.clone(), bad_cb)
                .await
                .unwrap_err();
            assert!(matches!(err, LauncherError::InvalidFormat(_)));
            assert!(err.to_string().contains("steamcmd.exe"));
            assert!(!bad_paths.steamcmd_dir.join("steamcmd.exe").exists());
        });
    }

    #[test]
    fn steamcmd_http_download_and_stream_helpers_work_end_to_end() {
        let zip_bytes = build_in_memory_zip(&[("steamcmd.exe", b"MZ-STEAMCMD-HTTP")]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let body_data = zip_bytes.clone();

            let server = tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                if let Ok((mut socket, _)) = listener.accept().await {
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body_data.len()
                    );
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&body_data).await;
                    let _ = socket.shutdown().await;
                }
            });

            let guard = TempDirGuard::new("tl_gi_steamcmd_http");
            let paths = guard.paths();
            let (cb, progress_log) = record_progress();
            let logs = Arc::new(Mutex::new(Vec::<String>::new()));
            let logs_clone = Arc::clone(&logs);
            let on_log: Arc<dyn Fn(String) + Send + Sync> =
                Arc::new(move |msg| logs_clone.lock().unwrap().push(msg));

            let client = build_download_client(None).unwrap();
            let url = format!("http://127.0.0.1:{port}/steamcmd.zip");
            let msg = install_steamcmd_from_direct_url(&client, &url, &paths, &cb, &on_log)
                .await
                .unwrap()
                .unwrap();

            assert!(msg.contains("steamcmd.exe"));
            assert_eq!(
                fs::read(paths.steamcmd_dir.join("steamcmd.exe")).unwrap(),
                b"MZ-STEAMCMD-HTTP"
            );
            let cache_entries: Vec<_> = fs::read_dir(paths.data_dir.join("cache"))
                .unwrap()
                .flatten()
                .collect();
            assert!(cache_entries.is_empty(), ".part temp file should be cleaned up");
            assert!(!logs.lock().unwrap().is_empty());
            assert_eq!(progress_log.lock().unwrap().last().unwrap().fraction(), Some(1.0));

            server.abort();

            let (mut writer, reader) = tokio::io::duplex(64);
            let streamed = Arc::new(Mutex::new(Vec::<String>::new()));
            let last_emitted = Arc::new(Mutex::new(None));
            let received = Arc::new(Mutex::new(Vec::<String>::new()));
            let recv_clone = Arc::clone(&received);
            let reader_task = tokio::spawn(read_winget_stream_lines(
                reader,
                Arc::clone(&streamed),
                last_emitted,
                Some(Arc::new(move |line| recv_clone.lock().unwrap().push(line))),
            ));

            use tokio::io::AsyncWriteExt;
            writer
                .write_all(
                    "\x1b[32mFound SteamCMD\x1b[0m\r-\r\\\r████░░░\rDownloading\r\n1.0 MB / 2.0 MB\r1.0 MB / 2.0 MB\rDone"
                        .as_bytes(),
                )
                .await
                .unwrap();
            drop(writer);
            reader_task.await.unwrap();

            let expected = vec![
                "Found SteamCMD".to_string(),
                "Downloading".to_string(),
                "1.0 MB / 2.0 MB".to_string(),
                "Done".to_string(),
            ];
            assert_eq!(*received.lock().unwrap(), expected);
            assert_eq!(*streamed.lock().unwrap(), expected);

            let err_isp = format_steamcmd_combined_error(
                "tcp connect error: connection refused (127.0.0.1:443)",
                "winget failed: timed out",
                None,
            );
            let isp_text = err_isp.to_string();
            assert!(isp_text.contains("steamcdn-a.akamaihd.net"));
            assert!(isp_text.contains("127.0.0.1"));
            assert!(isp_text.contains("Cloudflare WARP"));
            assert!(isp_text.contains("Tab Cài đặt"));

            let err_proxy = format_steamcmd_combined_error(
                "connection refused",
                "winget failed",
                Some("127.0.0.1:7890"),
            );
            assert!(err_proxy.to_string().contains("http://127.0.0.1:7890"));
            assert!(err_proxy.to_string().contains("Proxy"));

            assert_eq!(truncate_stage("abc", 10), "abc");
            assert_eq!(truncate_stage("Đang tải gói SteamCMD rất dài từ máy chủ", 12).chars().count(), 12);
        });
    }

    #[test]
    fn verify_zip_contains_required_file_checks_suffix_case_insensitively() {
        let guard = TempDirGuard::new("tl_gi_verify_zip");
        let zip_ok = guard.0.join("ok_tml.zip");
        write_test_zip(
            &zip_ok,
            &[("tModLoader-2026/SubDir/TMODLOADER.DLL", b"DLL-BYTES")],
        );
        assert!(verify_zip_contains_required_file(&zip_ok, "tmodloader.dll").is_ok());

        let zip_bad = guard.0.join("bad_tml.zip");
        write_test_zip(&zip_bad, &[("README.txt", b"not tml")]);
        let err = verify_zip_contains_required_file(&zip_bad, "tmodloader.dll").unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, LauncherError::InvalidFormat(_)));
        assert!(msg.contains("tModLoader.dll"));
        assert!(msg.contains("README.txt"));

        let zip_wrong_terraria = guard.0.join("wrong_terraria.zip");
        write_test_zip(&zip_wrong_terraria, &[("Other/other.bin", b"X")]);
        let paths = guard.paths();
        let (cb, _) = record_progress();
        let install_err =
            install_terraria_from_local_zip(&paths, &zip_wrong_terraria, cb).unwrap_err();
        assert!(matches!(install_err, LauncherError::InvalidFormat(_)));
        assert!(!paths.terraria_dir.join("Other").exists());
    }

    #[test]
    fn extract_zip_safely_preserves_existing_file_on_corrupt_entry_and_cleans_temp() {
        let guard = TempDirGuard::new("tl_gi_atomic_extract");
        let target_dir = guard.0.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        let dest_file = target_dir.join("Terraria.exe");
        fs::write(&dest_file, b"ORIGINAL-INTACT").unwrap();

        let zip_path = guard.0.join("corrupt.zip");
        let payload = b"REPLACEMENT_PAYLOAD_CORRUPTED";
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            writer.start_file("Terraria.exe", opts).unwrap();
            writer.write_all(payload).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        let pos = bytes
            .windows(payload.len())
            .position(|w| w == payload)
            .expect("stored payload present");
        bytes[pos + 3] ^= 0xFF;
        fs::write(&zip_path, &bytes).unwrap();

        let (cb, _) = record_progress();
        let err = extract_zip_safely(&zip_path, &target_dir, InstallTarget::Terraria, "extract", &cb)
            .unwrap_err();
        assert!(!err.to_string().is_empty());
        assert_eq!(fs::read(&dest_file).unwrap(), b"ORIGINAL-INTACT");

        let leftovers: Vec<_> = fs::read_dir(&target_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tl_extract_"))
            .collect();
        assert!(leftovers.is_empty(), "leftover extract temp files: {leftovers:?}");
    }

    #[test]
    fn read_declared_tml_dotnet_version_and_needs_dotnet_check_sys_fallback() {
        let guard = TempDirGuard::new("tl_gi_declared_dotnet");
        let paths = guard.paths();
        fs::create_dir_all(&paths.tmodloader_dir).unwrap();

        assert_eq!(read_declared_tml_dotnet_version(&paths), None);
        assert_eq!(read_required_tml_dotnet_version(&paths), "8.0.0");

        assert!(tmodloader_needs_dotnet8_install_with(
            &paths,
            |_, _| false,
            |_| None,
            || Some("8.0.25".to_string()),
            |_| false,
        ));
        assert!(!tmodloader_needs_dotnet8_install_with(
            &paths,
            |_, _| false,
            |_| None,
            || Some("8.0.25".to_string()),
            |_| true,
        ));

        fs::write(
            paths.tmodloader_dir.join("tModLoader.runtimeconfig.json"),
            r#"{"runtimeOptions":{"framework":{"name":"Microsoft.NETCore.App","version":"8.0.23"}}}"#,
        )
        .unwrap();
        assert_eq!(
            read_declared_tml_dotnet_version(&paths).as_deref(),
            Some("8.0.23")
        );

        assert!(!tmodloader_needs_dotnet8_install_with(
            &paths,
            |_, req| req == "8.0.23",
            |_| None,
            || None,
            |_| false,
        ));
        assert!(!tmodloader_needs_dotnet8_install_with(
            &paths,
            |_, _| false,
            |_| Some("8.0.25".to_string()),
            || None,
            |_| false,
        ));
        assert!(tmodloader_needs_dotnet8_install_with(
            &paths,
            |_, _| false,
            |_| Some("8.0.14".to_string()),
            || None,
            |_| true,
        ));
        assert!(!tmodloader_needs_dotnet8_install_with(
            &paths,
            |_, _| false,
            |_| Some("8.0.14".to_string()),
            || Some("8.0.24".to_string()),
            |_| false,
        ));
        assert!(tmodloader_needs_dotnet8_install_with(
            &paths,
            |_, _| false,
            |_| None,
            || Some("8.0.14".to_string()),
            |_| true,
        ));
    }

    #[test]
    fn resolve_post_winget_steamcmd_rechecks_and_falls_back_to_direct_zip() {
        let guard = TempDirGuard::new("tl_gi_post_winget");
        let paths = guard.paths();
        let fake_exe = paths.steamcmd_dir.join("winget_steamcmd.exe");
        fs::create_dir_all(&paths.steamcmd_dir).unwrap();
        fs::write(&fake_exe, b"MZ-WINGET").unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let client = build_download_client(None).unwrap();
            let (cb, _) = record_progress();
            let logs = Arc::new(Mutex::new(Vec::<String>::new()));
            let logs_clone = Arc::clone(&logs);
            let on_log: Arc<dyn Fn(String) + Send + Sync> =
                Arc::new(move |line| logs_clone.lock().unwrap().push(line));

            let mut calls = 0usize;
            let recheck_msg = resolve_post_winget_steamcmd_inner(
                &client,
                "http://127.0.0.1:9/steamcmd.zip",
                &paths,
                None,
                "direct failed",
                "Successfully installed",
                &cb,
                &on_log,
                || {
                    calls += 1;
                    if calls == 1 {
                        None
                    } else {
                        Some(fake_exe.clone())
                    }
                },
            )
            .await
            .unwrap();
            assert_eq!(calls, 2);
            assert!(recheck_msg.contains("winget"));

            let failed_err = resolve_post_winget_steamcmd_inner(
                &client,
                "http://127.0.0.1:9/steamcmd.zip",
                &paths,
                None,
                "direct failed",
                "No package found matching input criteria.",
                &cb,
                &on_log,
                || None,
            )
            .await
            .unwrap_err();
            assert!(!failed_err.to_string().is_empty());

            let zip_path = guard.0.join("steamcmd_direct.zip");
            write_test_zip(&zip_path, &[("steamcmd.exe", b"MZ-DIRECT-ZIP")]);
            let zip_bytes = fs::read(&zip_path).unwrap();

            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut buf = [0u8; 1024];
                    let _ = std::io::Read::read(&mut stream, &mut buf);
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        zip_bytes.len()
                    );
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(&zip_bytes);
                }
            });

            let direct_url = format!("http://{addr}/steamcmd.zip");
            let fallback_msg = resolve_post_winget_steamcmd_inner(
                &client,
                &direct_url,
                &paths,
                None,
                "direct failed",
                "Successfully installed",
                &cb,
                &on_log,
                || None,
            )
            .await
            .unwrap();
            server.join().unwrap();

            assert!(fallback_msg.contains("SteamCMD"));
            assert_eq!(
                fs::read(paths.steamcmd_dir.join("steamcmd.exe")).unwrap(),
                b"MZ-DIRECT-ZIP"
            );
        });
    }

    async fn read_http_request_head(socket: &mut tokio::net::TcpStream) -> String {
        let mut data = Vec::with_capacity(1024);
        let mut chunk = [0u8; 1024];
        loop {
            match socket.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    data.extend_from_slice(&chunk[..n]);
                    if data.windows(4).any(|w| w == b"\r\n\r\n") || data.len() >= 16_384 {
                        break;
                    }
                }
            }
        }
        String::from_utf8_lossy(&data).into_owned()
    }

    #[test]
    fn fetch_tmodloader_releases_retries_unauthenticated_when_token_gets_403() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let recorded = Arc::new(Mutex::new(Vec::<(String, bool, bool, bool)>::new()));
            let recorded_srv = Arc::clone(&recorded);

            let server = tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    let recorded_req = Arc::clone(&recorded_srv);
                    tokio::spawn(async move {
                        let req_text = read_http_request_head(&mut socket).await;
                        let target = req_text
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_string();

                        let mut has_auth = false;
                        let mut has_api_ver = false;
                        let mut has_accept = false;
                        for line in req_text.lines().skip(1) {
                            let lower = line.to_ascii_lowercase();
                            if lower.starts_with("authorization:") {
                                has_auth = true;
                            } else if lower.starts_with("x-github-api-version:")
                                && lower.contains("2022-11-28")
                            {
                                has_api_ver = true;
                            } else if lower.starts_with("accept:")
                                && lower.contains("application/vnd.github+json")
                            {
                                has_accept = true;
                            }
                        }

                        recorded_req
                            .lock()
                            .unwrap()
                            .push((target.clone(), has_auth, has_api_ver, has_accept));

                        let (status, content_type, body) = if has_auth {
                            ("403 Forbidden", "application/json", r#"{"message":"Bad credentials"}"#.to_string())
                        } else {
                            (
                                "200 OK",
                                "application/json",
                                serde_json::json!([
                                    {
                                        "tag_name": "v2026.09.3.1",
                                        "name": "tModLoader v2026.09.3.1",
                                        "prerelease": false,
                                        "published_at": "2026-09-20T08:00:00Z",
                                        "assets": [
                                            {
                                                "name": "tModLoader.zip",
                                                "browser_download_url": "https://github.com/tModLoader/tModLoader/releases/download/v2026.09.3.1/tModLoader.zip",
                                                "size": 9876543u64
                                            }
                                        ]
                                    }
                                ])
                                .to_string(),
                            )
                        };

                        let resp = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                        let _ = socket.shutdown().await;
                    });
                }
            });

            let base = format!("http://127.0.0.1:{port}");
            let releases =
                fetch_tmodloader_releases_with_bases(&base, &base, None, Some("bad-token"))
                    .await
                    .unwrap();
            server.abort();

            assert_eq!(releases.len(), 1);
            assert_eq!(releases[0].tag_name, "v2026.09.3.1");
            assert_eq!(releases[0].size, 9_876_543);

            let log = recorded.lock().unwrap();
            assert_eq!(log.len(), 2);
            assert_eq!(log[0].0, "/repos/tModLoader/tModLoader/releases?per_page=10");
            assert!(log[0].1);
            assert!(log[0].2);
            assert!(log[0].3);
            assert!(!log[1].1);
            assert!(log[1].2);
            assert!(log[1].3);
        });
    }

    #[test]
    fn fetch_tmodloader_releases_falls_back_to_web_html_on_api_403() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let paths_hit = Arc::new(Mutex::new(Vec::<String>::new()));
            let paths_srv = Arc::clone(&paths_hit);

            let html_body = concat!(
                "<html><head>",
                "<link rel=\"alternate\" type=\"application/atom+xml\" href=\"/tModLoader/tModLoader/releases.atom\">",
                "</head><body>",
                "<a href=\"/tModLoader/tModLoader/releases.atom\">Atom</a>",
                "<section>",
                "<a href=\"/tModLoader/tModLoader/releases/tag/v2026.10.2.3\">tModLoader v2026.10.2.3</a>",
                "<span>Pre-release</span>",
                "<relative-time datetime=\"2026-10-15T12:00:00Z\"></relative-time>",
                "</section>",
                "<section>",
                "<a href=\"/tModLoader/tModLoader/releases/tag/v2026.09.3.1\">tModLoader v2026.09.3.1</a>",
                "<span>Latest</span>",
                "<relative-time datetime=\"2026-09-20T08:00:00Z\"></relative-time>",
                "</section>",
                "</body></html>"
            );

            let server = tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    let paths_req = Arc::clone(&paths_srv);
                    tokio::spawn(async move {
                        let req_text = read_http_request_head(&mut socket).await;
                        let target = req_text
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_string();
                        paths_req.lock().unwrap().push(target.clone());

                        let resp = match target.as_str() {
                            "/repos/tModLoader/tModLoader/releases?per_page=10" => {
                                let body = r#"{"message":"rate limit exceeded"}"#;
                                format!(
                                    "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                    body.len()
                                )
                            }
                            "/tModLoader/tModLoader/releases/latest/download/tModLoader.zip" => {
                                "HTTP/1.1 302 Found\r\nLocation: /tModLoader/tModLoader/releases/download/v2026.09.3.1/tModLoader.zip\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                            }
                            "/tModLoader/tModLoader/releases" => {
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html_body}",
                                    html_body.len()
                                )
                            }
                            _ => {
                                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                            }
                        };
                        let _ = socket.write_all(resp.as_bytes()).await;
                        let _ = socket.shutdown().await;
                    });
                }
            });

            let base = format!("http://127.0.0.1:{port}");
            let releases = fetch_tmodloader_releases_with_bases(&base, &base, None, None)
                .await
                .unwrap();
            server.abort();

            assert_eq!(releases.len(), 2);
            assert_eq!(releases[0].tag_name, "v2026.10.2.3");
            assert!(releases[0].prerelease);
            assert_eq!(releases[0].published_at, "2026-10-15T12:00:00Z");
            assert_eq!(releases[0].asset_name, "tModLoader.zip");
            assert_eq!(
                releases[0].download_url,
                format!("{base}/tModLoader/tModLoader/releases/download/v2026.10.2.3/tModLoader.zip")
            );

            assert_eq!(releases[1].tag_name, "v2026.09.3.1");
            assert!(!releases[1].prerelease);
            assert_eq!(releases[1].published_at, "2026-09-20T08:00:00Z");
            assert_eq!(
                releases[1].download_url,
                format!("{base}/tModLoader/tModLoader/releases/download/v2026.09.3.1/tModLoader.zip")
            );

            let hit = paths_hit.lock().unwrap();
            assert!(!hit.iter().any(|p| p.contains("releases.atom")));
        });
    }

    #[test]
    fn fetch_tmodloader_releases_falls_back_to_redirect_only_when_api_and_html_fail() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let fail_redirect = Arc::new(AtomicBool::new(false));
            let fail_redirect_srv = Arc::clone(&fail_redirect);

            let server = tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    let fail_flag = Arc::clone(&fail_redirect_srv);
                    tokio::spawn(async move {
                        let req_text = read_http_request_head(&mut socket).await;
                        let target = req_text
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/");

                        let resp = match target {
                            "/tModLoader/tModLoader/releases/latest/download/tModLoader.zip"
                                if !fail_flag.load(Ordering::Relaxed) =>
                            {
                                "HTTP/1.1 302 Found\r\nLocation: https://github.com/tModLoader/tModLoader/releases/download/v2026.09.3.1/tModLoader.zip\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                            }
                            _ => {
                                let body = "forbidden";
                                format!(
                                    "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                    body.len()
                                )
                            }
                        };
                        let _ = socket.write_all(resp.as_bytes()).await;
                        let _ = socket.shutdown().await;
                    });
                }
            });

            let base = format!("http://127.0.0.1:{port}");
            let releases = fetch_tmodloader_releases_with_bases(&base, &base, None, None)
                .await
                .unwrap();
            assert_eq!(releases.len(), 1);
            assert_eq!(releases[0].tag_name, "v2026.09.3.1");
            assert_eq!(releases[0].name, "tModLoader v2026.09.3.1");
            assert!(!releases[0].prerelease);
            assert_eq!(releases[0].asset_name, "tModLoader.zip");
            assert_eq!(releases[0].size, 0);
            assert_eq!(
                releases[0].download_url,
                format!("{base}/tModLoader/tModLoader/releases/download/v2026.09.3.1/tModLoader.zip")
            );

            fail_redirect.store(true, Ordering::Relaxed);
            let err = fetch_tmodloader_releases_with_bases(&base, &base, None, None)
                .await
                .unwrap_err();
            server.abort();
            assert!(matches!(err, LauncherError::Network(_)));
            assert!(err
                .to_string()
                .contains("dự phòng trang GitHub Releases cũng thất bại"));
        });
    }

    #[test]
    fn parse_tmodloader_releases_html_handles_entities_dedup_cap_and_tag_validation() {
        assert!(is_valid_release_tag("v2026.09.3.1"));
        assert!(is_valid_release_tag("v1.4.4-stable+2026_1"));
        assert!(!is_valid_release_tag(""));
        assert!(!is_valid_release_tag("."));
        assert!(!is_valid_release_tag(".."));
        assert!(!is_valid_release_tag("v2026/evil"));
        assert!(!is_valid_release_tag("v2026 tag"));

        assert_eq!(
            extract_release_tag_from_location(
                "https://github.com/tModLoader/tModLoader/releases/download/v2026.09.3.1/tModLoader.zip?x=1#top"
            )
            .as_deref(),
            Some("v2026.09.3.1")
        );
        assert_eq!(
            extract_release_tag_from_location("/tModLoader/tModLoader/releases/tag/v2026.09.3.1/")
                .as_deref(),
            Some("v2026.09.3.1")
        );
        assert_eq!(
            extract_release_tag_from_location("/tModLoader/tModLoader/releases/tag/.."),
            None
        );

        let html = concat!(
            "<a href=\"/tModLoader/tModLoader/releases.atom\">Atom</a>",
            "<a href=\"/tModLoader/tModLoader/releases/tag/..\">Traversal</a>",
            "<a href=\"/tModLoader/tModLoader/releases/tag/bad/tag\">Slash</a>",
            "<div>",
            "<a href=\"https://github.com/tModLoader/tModLoader/releases/tag/v2026.10.2.3\">",
            "<span>tModLoader &amp; &lt;Beta&gt; &quot;2026&quot; &#39;Test&#39;</span>",
            "</a>",
            "<a href=\"/tModLoader/tModLoader/releases/tag/v2026.10.2.3\">dup</a>",
            "<span>Pre-release</span>",
            "<relative-time datetime=\"2026-10-15T00:00:00Z\"></relative-time>",
            "</div>",
            "<div>",
            "<a href=\"/tModLoader/tModLoader/releases/tag/v2026.09.3.1\"></a>",
            "<span class=\"Label\">Pre-release</span>",
            "</div>",
        );

        let parsed =
            parse_tmodloader_releases_html(html, "https://github.com/", Some("v2026.09.3.1"));
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].tag_name, "v2026.10.2.3");
        assert_eq!(parsed[0].name, "tModLoader & <Beta> \"2026\" 'Test'");
        assert!(parsed[0].prerelease);
        assert_eq!(parsed[0].published_at, "2026-10-15T00:00:00Z");
        assert_eq!(
            parsed[0].download_url,
            "https://github.com/tModLoader/tModLoader/releases/download/v2026.10.2.3/tModLoader.zip"
        );

        assert_eq!(parsed[1].tag_name, "v2026.09.3.1");
        assert_eq!(parsed[1].name, "v2026.09.3.1");
        assert!(!parsed[1].prerelease);

        let mut many_cards = String::new();
        for idx in 1..=12 {
            many_cards.push_str(&format!(
                "<div><a href=\"/tModLoader/tModLoader/releases/tag/v2026.01.{idx}.0\">Rel {idx}</a></div>"
            ));
        }
        let capped = parse_tmodloader_releases_html(&many_cards, "https://github.com", None);
        assert_eq!(capped.len(), 10);
        assert_eq!(capped[0].tag_name, "v2026.01.1.0");
        assert_eq!(capped[9].tag_name, "v2026.01.10.0");

        let with_inserted_latest =
            parse_tmodloader_releases_html(&many_cards, "https://github.com", Some("v2025.12.9.9"));
        assert_eq!(with_inserted_latest.len(), 10);
        assert_eq!(with_inserted_latest[0].tag_name, "v2025.12.9.9");
        assert!(!with_inserted_latest[0].prerelease);

        assert!(should_retry_without_system_proxy(&LauncherError::Network(
            "Lỗi kết nối khi tải tModLoader: timed out".to_string()
        )));
        assert!(should_retry_without_system_proxy(&LauncherError::Network(
            "Lỗi đọc dữ liệu khi tải tModLoader: reset".to_string()
        )));
        for code in ["403", "407", "429", "502", "503", "504"] {
            assert!(should_retry_without_system_proxy(&LauncherError::Network(
                format!("Máy chủ github.com trả về HTTP {code} khi tải tModLoader")
            )));
        }
        assert!(!should_retry_without_system_proxy(&LauncherError::Network(
            "Máy chủ github.com trả về HTTP 404 Not Found khi tải tModLoader".to_string()
        )));
        assert!(!should_retry_without_system_proxy(
            &LauncherError::InvalidInput("bad".to_string())
        ));
    }

    #[test]
    fn parse_dotnet_runtime_version_handles_leading_utf8_bom() {
        let bom_single = "\u{feff}{\"runtimeOptions\":{\"framework\":{\"name\":\"Microsoft.NETCore.App\",\"version\":\"8.0.14\"}}}";
        assert_eq!(parse_dotnet_runtime_version(bom_single).unwrap(), "8.0.14");

        let bom_multi = "\u{feff}{\"runtimeOptions\":{\"frameworks\":[{\"name\":\"Microsoft.NETCore.App\",\"version\":\"8.0.23\"}]}}";
        assert_eq!(parse_dotnet_runtime_version(bom_multi).unwrap(), "8.0.23");
    }

    #[test]
    fn installed_terraria_receipt_validates_install_dir_and_legacy_fallback() {
        let guard = TempDirGuard::new("tl_gi_receipt_dir");
        let mut paths = guard.paths();
        fs::create_dir_all(&paths.terraria_dir).unwrap();
        fs::create_dir_all(&paths.data_dir).unwrap();
        fs::write(paths.terraria_exe(), b"MZ-TEST").unwrap();

        let seed = InstalledTerrariaReceipt {
            version: "1.4.5.8".to_string(),
            sha256: "abc123".to_string(),
            source: "owner/repo#terraria-game".to_string(),
            install_dir: String::new(),
        };
        save_installed_terraria_receipt(&paths, &seed).unwrap();
        let saved = load_installed_terraria_receipt(&paths).unwrap();
        assert_eq!(
            saved.install_dir,
            paths.terraria_dir.to_string_lossy().as_ref()
        );

        let slash_variant = format!(
            "{}///",
            paths
                .terraria_dir
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_uppercase()
        );
        let variant_json = serde_json::json!({
            "version": "1.4.5.8",
            "sha256": "abc123",
            "source": "owner/repo#terraria-game",
            "install_dir": slash_variant,
        });
        fs::write(
            installed_terraria_receipt_path(&paths),
            serde_json::to_string_pretty(&variant_json).unwrap(),
        )
        .unwrap();
        #[cfg(windows)]
        assert!(load_installed_terraria_receipt(&paths).is_some());

        let legacy_json = r#"{"version":"1.4.5.8","sha256":"abc123","source":"legacy"}"#;
        fs::write(installed_terraria_receipt_path(&paths), legacy_json).unwrap();
        assert!(load_installed_terraria_receipt(&paths).is_some());

        let custom_dir = guard.0.join("CustomTerraria");
        fs::create_dir_all(&custom_dir).unwrap();
        fs::write(custom_dir.join("Terraria.exe"), b"MZ-CUSTOM").unwrap();
        paths.terraria_dir = custom_dir.clone();
        assert_eq!(load_installed_terraria_receipt(&paths), None);

        save_installed_terraria_receipt(&paths, &seed).unwrap();
        let custom_saved = load_installed_terraria_receipt(&paths).unwrap();
        assert_eq!(
            custom_saved.install_dir,
            custom_dir.to_string_lossy().as_ref()
        );

        let mismatched_json = serde_json::json!({
            "version": "1.4.5.8",
            "sha256": "abc123",
            "source": "owner/repo#terraria-game",
            "install_dir": guard.0.join("OtherTerraria").to_string_lossy(),
        });
        fs::write(
            installed_terraria_receipt_path(&paths),
            serde_json::to_string_pretty(&mismatched_json).unwrap(),
        )
        .unwrap();
        assert_eq!(load_installed_terraria_receipt(&paths), None);
    }

    #[test]
    fn corrupt_bundled_dotnet_requires_install_even_when_dir_exists() {
        let guard = TempDirGuard::new("tl_gi_corrupt_dotnet");
        let paths = guard.paths();
        let dotnet_dir = paths.tmodloader_dir.join("dotnet");
        fs::create_dir_all(dotnet_dir.join("host").join("fxr")).unwrap();
        fs::create_dir_all(
            dotnet_dir
                .join("shared")
                .join("Microsoft.NETCore.App")
                .join("8.0.23"),
        )
        .unwrap();
        fs::write(dotnet_dir.join("dotnet.exe"), b"not-a-valid-pe-binary").unwrap();
        fs::write(
            paths.tmodloader_dir.join("tModLoader.runtimeconfig.json"),
            r#"{"runtimeOptions":{"framework":{"name":"Microsoft.NETCore.App","version":"8.0.23"}}}"#,
        )
        .unwrap();

        assert!(detect_exact_bundled_dotnet_runtime(&paths, "8.0.23"));
        assert_eq!(tl_core::query_local_dotnet8_runtime(&paths), None);
        assert!(tmodloader_needs_dotnet8_install_with(
            &paths,
            |p, req| {
                detect_exact_bundled_dotnet_runtime(p, req)
                    && tl_core::query_local_dotnet8_runtime(p)
                        .is_some_and(|v| tl_core::version_at_least_patch(&v, req))
            },
            tl_core::query_local_dotnet8_runtime,
            || None,
            |_| false,
        ));
    }
}
