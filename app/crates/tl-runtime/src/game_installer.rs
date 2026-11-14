use crate::game_runner::install_xna_framework_elevated;
use crate::http_client::{
    build_http_client, ensure_rustls_provider, normalize_proxy_url, LAUNCHER_USER_AGENT,
};
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
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

fn is_github_api_asset_url(raw: &str) -> bool {
    extract_http_host(raw).eq_ignore_ascii_case("api.github.com")
        && raw.contains("/releases/assets/")
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
            "Vui lòng nhập nguồn tải Terraria (repo GitHub hoặc URL .zip).".to_string(),
        ));
    }

    let token = github_token.map(str::trim).filter(|s| !s.is_empty());

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
                "Nguồn tải Terraria phải là repo GitHub (owner/repo) hoặc URL http:// / https://."
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
}
