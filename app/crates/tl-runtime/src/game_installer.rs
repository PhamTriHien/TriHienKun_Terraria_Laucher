use crate::game_runner::install_xna_framework_elevated;
use crate::http_client::{
    build_http_client, ensure_rustls_provider, normalize_proxy_url, LAUNCHER_USER_AGENT,
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
use std::sync::Arc;
use std::time::Duration;
use tl_core::paths::detect_dotnet8_runtime;
use tl_core::{LauncherError, LauncherPaths, LauncherResult};
use tokio::io::AsyncWriteExt;
use zip::ZipArchive;

pub const TMODLOADER_GITHUB_REPO: &str = "tModLoader/tModLoader";
pub const XNA_40_OFFICIAL_MSI_URL: &str =
    "https://download.microsoft.com/download/a/c/2/ac2c903b-e6e8-42c2-9fd7-bebac362a930/xnafx40_redist.msi";
pub const TERRARIA_MANIFEST_FILENAME: &str = "terraria-manifest.json";

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
}

impl InstallTarget {
    pub fn label(self) -> &'static str {
        match self {
            Self::Terraria => "Terraria",
            Self::TModLoader => "tModLoader",
            Self::DotNet8 => ".NET 8 Runtime",
            Self::Xna40 => "XNA Framework 4.0",
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

fn build_download_client(proxy_url: Option<&str>) -> LauncherResult<Client> {
    ensure_rustls_provider();

    let mut builder = Client::builder()
        .user_agent(LAUNCHER_USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60));

    if let Some(proxy_str) = proxy_url.and_then(normalize_proxy_url) {
        let proxy = reqwest::Proxy::all(&proxy_str)
            .map_err(|e| LauncherError::InvalidInput(format!("Proxy không hợp lệ: {e}")))?;
        builder = builder.proxy(proxy);
    }

    builder
        .build()
        .map_err(|e| LauncherError::Network(format!("Không thể khởi tạo HTTP client: {e}")))
}

struct DownloadSpec<'a> {
    url: &'a str,
    dest_part: &'a Path,
    target: InstallTarget,
    stage: &'a str,
    fallback_total: Option<u64>,
    accept_header: Option<&'a str>,
    bearer_token: Option<&'a str>,
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
            "Lỗi kết nối khi tải {}: {e}",
            spec.target.label()
        ))
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        return Err(LauncherError::Network(format!(
            "Máy chủ trả về HTTP {} khi tải {}",
            status,
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
                "Lỗi đọc dữ liệu khi tải {}: {e}",
                spec.target.label()
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

pub async fn fetch_tmodloader_releases(
    proxy_url: Option<&str>,
) -> LauncherResult<Vec<TmodLoaderReleaseInfo>> {
    let client = build_http_client(proxy_url)?;
    let url = format!(
        "https://api.github.com/repos/{TMODLOADER_GITHUB_REPO}/releases?per_page=10"
    );

    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| LauncherError::Network(format!("Lỗi kết nối GitHub API: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let detail = if status.as_u16() == 403 {
            " (có thể đã vượt giới hạn tốc độ GitHub API)"
        } else {
            ""
        };
        return Err(LauncherError::Network(format!(
            "GitHub API trả về HTTP {status}{detail}"
        )));
    }

    let json: Value = resp
        .json()
        .await
        .map_err(|e| LauncherError::Network(format!("Phản hồi JSON từ GitHub không hợp lệ: {e}")))?;

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
    let root: Value = serde_json::from_str(runtimeconfig_json)?;
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
        format!("https://dotnetcli.azureedge.net/dotnet/Runtime/{ver}/dotnet-runtime-{ver}-win-x64.zip"),
        format!("https://builds.dotnet.microsoft.com/dotnet/Runtime/{ver}/dotnet-runtime-{ver}-win-x64.zip"),
    ];

    let cache_dir = paths.data_dir.join("cache");
    fs::create_dir_all(&cache_dir)?;
    let part_path = cache_dir.join(format!(
        "dotnet-runtime-{ver}-win-x64-{}.zip.part",
        uuid::Uuid::new_v4()
    ));
    let _part_guard = TempFileGuard::new(part_path.clone());

    let client = build_download_client(proxy_url)?;
    let stage = format!("Đang tải .NET {ver} portable...");
    let mut last_err: Option<LauncherError> = None;

    for url in &urls {
        let _ = fs::remove_file(&part_path);
        match download_to_part_file(
            &client,
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

    let client = build_download_client(proxy_url)?;
    let fallback_size = if release.size > 0 { Some(release.size) } else { None };
    let dl_stage = format!("Đang tải tModLoader {}...", release.tag_name);

    download_to_part_file(
        &client,
        DownloadSpec {
            url,
            dest_part: &part_path,
            target: InstallTarget::TModLoader,
            stage: &dl_stage,
            fallback_total: fallback_size,
            accept_header: None,
            bearer_token: None,
        },
        &on_progress,
    )
    .await?;

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

    if also_install_dotnet && detect_dotnet8_runtime(paths).is_none() {
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

pub fn load_installed_terraria_receipt(paths: &LauncherPaths) -> Option<InstalledTerrariaReceipt> {
    if !paths.terraria_exe().is_file() {
        return None;
    }
    let raw = fs::read_to_string(installed_terraria_receipt_path(paths)).ok()?;
    serde_json::from_str::<InstalledTerrariaReceipt>(raw.trim_start_matches('\u{feff}')).ok()
}

fn save_installed_terraria_receipt(
    paths: &LauncherPaths,
    receipt: &InstalledTerrariaReceipt,
) -> LauncherResult<()> {
    fs::create_dir_all(&paths.data_dir)?;
    let json = serde_json::to_string_pretty(receipt)?;
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
            fs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = open_output_file(&dest)?;
            std::io::copy(&mut entry, &mut out)?;
            use std::io::Write;
            out.flush()?;
        }

        if should_emit_entry(idx, total_entries, report_step) {
            emit_progress(on_progress, target, stage, idx as u64 + 1, Some(total_entries));
        }
    }

    Ok(top_level_set.into_iter().collect())
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

fn open_output_file(dest: &Path) -> std::io::Result<File> {
    match File::create(dest) {
        Ok(f) => Ok(f),
        Err(err) => {
            if err.kind() == std::io::ErrorKind::PermissionDenied && clear_readonly_attribute(dest) {
                if let Ok(f) = File::create(dest) {
                    return Ok(f);
                }
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
}
