use crate::http_client::{build_http_client, normalize_proxy_url};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error as StdError;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tl_core::config::write_atomic;
use tl_core::{
    load_enabled_mods, parse_tmod_file, save_enabled_mods, scan_tmod_directory, LauncherError,
    LauncherPaths, LauncherResult, TmodMetadata,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Notify;
use zip::ZipArchive;

#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub(crate) const TMODLOADER_STEAM_APP_ID: u64 = 1281930;
const MAX_WORKSHOP_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const STEAM_GET_PUBLISHED_FILE_DETAILS_URL: &str =
    "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/";
const STEAM_WORKSHOP_BROWSE_URL: &str = "https://steamcommunity.com/workshop/browse/";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledModEntry {
    pub meta: TmodMetadata,
    pub enabled: bool,
    pub missing_deps: Vec<String>,
    pub disabled_deps: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CuratedModSource {
    pub name: &'static str,
    pub repo: &'static str,
    pub summary: &'static str,
}

pub const CURATED_GITHUB_MODS: &[CuratedModSource] = &[
    CuratedModSource {
        name: "Recipe Browser",
        repo: "JavidPack/RecipeBrowser",
        summary: "Tra cứu công thức chế tạo & vật phẩm trong game",
    },
    CuratedModSource {
        name: "Magic Storage",
        repo: "blushiemagic/MagicStorage",
        summary: "Hệ thống kho lưu trữ & chế tạo tập trung",
    },
    CuratedModSource {
        name: "Boss Checklist",
        repo: "JavidPack/BossChecklist",
        summary: "Danh sách tiến trình Boss & sự kiện",
    },
    CuratedModSource {
        name: "AlchemistNPC Lite",
        repo: "VVV101/AlchemistNPCLite",
        summary: "NPC bán thuốc & vật phẩm tiện ích",
    },
    CuratedModSource {
        name: "Ore Excavator",
        repo: "CrazyContraption/OreExcavator",
        summary: "Đào toàn bộ mạch quặng nhanh",
    },
    CuratedModSource {
        name: "Wing Slot",
        repo: "abluescarab/tModLoader-WingSlot",
        summary: "Thêm ô trang bị riêng cho Cánh",
    },
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteTmodAsset {
    pub repo: String,
    pub tag_name: String,
    pub asset_name: String,
    pub size_bytes: u64,
    pub download_url: String,
    #[serde(default)]
    pub api_asset_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WorkshopSort {
    #[default]
    Trending,
    MostSubscribed,
    TopRated,
    MostRecent,
}

impl WorkshopSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Trending => "Thịnh hành",
            Self::MostSubscribed => "Phổ biến nhất",
            Self::TopRated => "Đánh giá cao",
            Self::MostRecent => "Mới nhất",
        }
    }

    pub fn steam_param(self) -> &'static str {
        match self {
            Self::Trending => "trend",
            Self::MostSubscribed => "totaluniquesubscribers",
            Self::TopRated => "toprated",
            Self::MostRecent => "mostrecent",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkshopItemDetails {
    pub published_file_id: String,
    pub title: String,
    pub author: String,
    pub description: String,
    pub preview_url: String,
    pub file_size: u64,
    pub time_updated: u64,
    pub subscriptions: u64,
    pub favorited: u64,
    pub views: u64,
    pub tags: Vec<String>,
    pub child_ids: Vec<String>,
    pub consumer_app_id: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkshopQueryResult {
    pub items: Vec<WorkshopItemDetails>,
    pub page: u32,
    pub has_next_page: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkshopManifestRecord {
    pub published_file_id: String,
    pub internal_name: String,
    pub file_name: String,
    pub display_name: String,
    pub mod_version: String,
    pub workshop_title: String,
    pub author: String,
    pub preview_url: String,
    pub time_updated: u64,
    pub installed_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedWorkshopMod {
    pub workshop_id: String,
    pub mod_dir: PathBuf,
    pub meta: TmodMetadata,
    pub installed: bool,
    pub enabled: bool,
    pub workshop_title: Option<String>,
    pub author: Option<String>,
    pub preview_url: Option<String>,
    pub time_updated: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModDownloadProgress {
    pub item_index: usize,
    pub item_total: usize,
    pub label: String,
    pub workshop_id: Option<String>,
    pub stage: String,
    pub current_bytes: u64,
    pub total_bytes: Option<u64>,
}

impl ModDownloadProgress {
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total_bytes.filter(|&t| t > 0)?;
        Some((self.current_bytes as f32 / total as f32).clamp(0.0, 1.0))
    }
}

pub fn scan_installed_mods(paths: &LauncherPaths) -> LauncherResult<Vec<InstalledModEntry>> {
    let mods_dir = paths.tml_mods_dir();
    fs::create_dir_all(&mods_dir)?;

    let enabled_list = load_enabled_mods(&paths.tml_enabled_json()).unwrap_or_default();
    let enabled_lower: HashSet<String> = enabled_list
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();

    let mut parsed = Vec::new();
    for entry in fs::read_dir(&mods_dir)?.flatten() {
        let p = entry.path();
        if p.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("tmod"))
            .unwrap_or(false)
        {
            if let Ok(meta) = parse_tmod_file(&p) {
                parsed.push(meta);
            }
        }
    }

    let available_lower: HashSet<String> = parsed
        .iter()
        .map(|m| m.internal_name.to_ascii_lowercase())
        .collect();

    let mut out: Vec<InstalledModEntry> = parsed
        .into_iter()
        .map(|meta| {
            let enabled = enabled_lower.contains(&meta.internal_name.to_ascii_lowercase());
            let mut missing_deps = Vec::new();
            let mut disabled_deps = Vec::new();
            for raw_ref in &meta.mod_references {
                let dep_name = raw_ref.split('@').next().unwrap_or(raw_ref).trim();
                if dep_name.is_empty() {
                    continue;
                }
                let dep_lower = dep_name.to_ascii_lowercase();
                if !available_lower.contains(&dep_lower) {
                    if !missing_deps
                        .iter()
                        .any(|d: &String| d.eq_ignore_ascii_case(dep_name))
                    {
                        missing_deps.push(dep_name.to_string());
                    }
                } else if enabled
                    && !enabled_lower.contains(&dep_lower)
                    && !disabled_deps
                        .iter()
                        .any(|d: &String| d.eq_ignore_ascii_case(dep_name))
                {
                    disabled_deps.push(dep_name.to_string());
                }
            }
            InstalledModEntry {
                meta,
                enabled,
                missing_deps,
                disabled_deps,
            }
        })
        .collect();

    out.sort_by_key(|a| a.meta.display_name.to_lowercase());
    Ok(out)
}

pub fn enable_mod_and_installed_deps(
    paths: &LauncherPaths,
    internal_name: &str,
) -> LauncherResult<Vec<String>> {
    let target = internal_name.trim();
    if target.is_empty() {
        return Ok(Vec::new());
    }

    let mods_dir = paths.tml_mods_dir();
    fs::create_dir_all(&mods_dir)?;

    let installed = scan_tmod_directory(&mods_dir).unwrap_or_default();
    let by_name: HashMap<String, TmodMetadata> = installed
        .into_iter()
        .map(|m| (m.internal_name.to_ascii_lowercase(), m))
        .collect();

    let target_lower = target.to_ascii_lowercase();
    if !by_name.contains_key(&target_lower) {
        return Err(LauncherError::NotFound(format!(
            "Không tìm thấy mod '{target}' trong thư mục Mods."
        )));
    }

    let enabled_path = paths.tml_enabled_json();
    let mut current = load_enabled_mods(&enabled_path).unwrap_or_default();
    let mut enabled_lower: HashSet<String> =
        current.iter().map(|s| s.to_ascii_lowercase()).collect();
    let mut newly_enabled = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(target_lower);

    while let Some(key) = queue.pop_front() {
        if !visited.insert(key.clone()) {
            continue;
        }
        let Some(meta) = by_name.get(&key) else {
            continue;
        };
        let canonical_name = meta.internal_name.clone();
        if enabled_lower.insert(canonical_name.to_ascii_lowercase()) {
            current.push(canonical_name.clone());
            newly_enabled.push(canonical_name);
        }
        for raw_ref in &meta.mod_references {
            let dep = raw_ref.split('@').next().unwrap_or(raw_ref).trim();
            let dep_lower = dep.to_ascii_lowercase();
            if !dep_lower.is_empty() && by_name.contains_key(&dep_lower) && !visited.contains(&dep_lower)
            {
                queue.push_back(dep_lower);
            }
        }
    }

    if !newly_enabled.is_empty() {
        save_enabled_mods(&enabled_path, &current)?;
    }

    Ok(newly_enabled)
}

pub fn set_mod_enabled(
    paths: &LauncherPaths,
    internal_name: &str,
    enable: bool,
) -> LauncherResult<Vec<String>> {
    let enabled_path = paths.tml_enabled_json();
    let mut current = load_enabled_mods(&enabled_path).unwrap_or_default();
    if enable {
        let canonical_name = scan_tmod_directory(&paths.tml_mods_dir())
            .ok()
            .and_then(|mods| {
                mods.into_iter()
                    .find(|m| m.internal_name.eq_ignore_ascii_case(internal_name))
                    .map(|m| m.internal_name)
            })
            .unwrap_or_else(|| internal_name.to_string());
        if !current
            .iter()
            .any(|n| n.eq_ignore_ascii_case(&canonical_name))
        {
            current.push(canonical_name);
        }
    } else {
        current.retain(|n| !n.eq_ignore_ascii_case(internal_name));
    }
    save_enabled_mods(&enabled_path, &current)?;
    Ok(current)
}

pub fn remove_workshop_manifest_entry(
    paths: &LauncherPaths,
    internal_name: &str,
) -> LauncherResult<()> {
    let manifest_path = workshop_manifest_path(paths);
    if !manifest_path.is_file() {
        return Ok(());
    }
    let mut records = load_workshop_manifest(paths);
    let before = records.len();
    records.retain(|r| !r.internal_name.eq_ignore_ascii_case(internal_name));
    if records.len() != before {
        let json = serde_json::to_string_pretty(&records)?;
        write_atomic(&manifest_path, json.as_bytes())?;
    }
    Ok(())
}

pub fn delete_installed_mod(
    paths: &LauncherPaths,
    mod_file_path: &Path,
    internal_name: &str,
) -> LauncherResult<()> {
    if mod_file_path.is_file() {
        fs::remove_file(mod_file_path)?;
    }
    set_mod_enabled(paths, internal_name, false)?;
    remove_workshop_manifest_entry(paths, internal_name)?;
    let _ = crate::modpack_sync::remove_mod_source(paths, internal_name);
    Ok(())
}

struct TempDirCleanupGuard(PathBuf);

impl Drop for TempDirCleanupGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct TempFileCleanupGuard(Option<PathBuf>);

impl Drop for TempFileCleanupGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn is_safe_mod_file_stem(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed != name || trimmed == "." || trimmed == ".." {
        return false;
    }
    if trimmed.ends_with(['.', ' '])
        || trimmed.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'])
        || trimmed.contains("..")
        || trimmed.chars().any(char::is_control)
    {
        return false;
    }
    let mut comps = Path::new(trimmed).components();
    matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
}

fn same_disk_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

fn remove_duplicate_internal_mods(mods_dir: &Path, keep_dest: &Path, internal_name: &str) {
    let Ok(entries) = fs::read_dir(mods_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let is_tmod = p
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("tmod"));
        if !is_tmod || same_disk_path(&p, keep_dest) {
            continue;
        }
        if parse_tmod_file(&p)
            .is_ok_and(|existing| existing.internal_name.eq_ignore_ascii_case(internal_name))
        {
            let _ = fs::remove_file(&p);
        }
    }
}

fn install_validated_tmod_file(
    src: &Path,
    meta: &TmodMetadata,
    mods_dir: &Path,
    paths: &LauncherPaths,
    auto_enable: bool,
) -> LauncherResult<TmodMetadata> {
    let target_name = {
        let trimmed = meta.internal_name.trim();
        if is_safe_mod_file_stem(trimmed) {
            format!("{trimmed}.tmod")
        } else {
            meta.file_name.clone()
        }
    };
    let dest = mods_dir.join(&target_name);
    if !same_disk_path(src, &dest) {
        fs::copy(src, &dest)?;
    }
    if !meta.internal_name.trim().is_empty() {
        remove_duplicate_internal_mods(mods_dir, &dest, &meta.internal_name);
    }
    if auto_enable {
        set_mod_enabled(paths, &meta.internal_name, true)?;
    }
    parse_tmod_file(&dest)
}

pub fn import_local_mod_files(
    sources: &[PathBuf],
    paths: &LauncherPaths,
    auto_enable: bool,
) -> LauncherResult<Vec<TmodMetadata>> {
    let mods_dir = paths.tml_mods_dir();
    fs::create_dir_all(&mods_dir)?;
    let mut imported = Vec::new();
    let target_tml_ym = detect_installed_tml_year_month(paths);

    for src in sources {
        let ext = src
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        if ext == "tmod" {
            let meta = parse_tmod_file(src)?;
            imported.push(install_validated_tmod_file(
                src,
                &meta,
                &mods_dir,
                paths,
                auto_enable,
            )?);
        } else if ext == "zip" {
            let tmp_root =
                mods_dir.join(format!(".tl_zip_import_tmp_{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&tmp_root)?;
            let _cleanup_guard = TempDirCleanupGuard(tmp_root.clone());

            let mut archive = ZipArchive::new(File::open(src)?)?;
            let mut extracted = Vec::new();
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i)?;
                if entry.is_dir() {
                    continue;
                }
                let Some(rel_path) = entry.enclosed_name() else {
                    continue;
                };
                let is_tmod = rel_path
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case("tmod"));
                if !is_tmod {
                    continue;
                }

                let out_path = tmp_root.join(&rel_path);
                if let Some(parent) = out_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut out = File::create(&out_path)?;
                std::io::copy(&mut entry, &mut out)?;
                extracted.push(out_path);
            }

            let selected = pick_best_workshop_tmods(&extracted, &tmp_root, target_tml_ym);
            for candidate in selected {
                let meta = parse_tmod_file(&candidate)?;
                imported.push(install_validated_tmod_file(
                    &candidate,
                    &meta,
                    &mods_dir,
                    paths,
                    auto_enable,
                )?);
            }
        }
    }

    Ok(imported)
}

pub async fn fetch_github_latest_tmod(
    owner_repo: &str,
    proxy_url: Option<&str>,
    github_token: Option<&str>,
) -> LauncherResult<Vec<RemoteTmodAsset>> {
    let repo = owner_repo
        .trim()
        .trim_start_matches("https://github.com/")
        .trim_matches('/');
    if !repo.contains('/') {
        return Err(LauncherError::InvalidInput(
            "Repo GitHub phải theo dạng owner/repo (vd: JavidPack/RecipeBrowser)".to_string(),
        ));
    }

    let client = build_http_client(proxy_url)?;
    let api_url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let mut req = client
        .get(&api_url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(tok) = github_token.map(str::trim).filter(|s| !s.is_empty()) {
        req = req.bearer_auth(tok);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| LauncherError::Network(format!("Lỗi kết nối GitHub API: {e}")))?;

    if !resp.status().is_success() {
        return Err(LauncherError::Network(format!(
            "GitHub API trả về HTTP {}",
            resp.status()
        )));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| LauncherError::Network(format!("JSON GitHub không hợp lệ: {e}")))?;

    let tag_name = json["tag_name"]
        .as_str()
        .unwrap_or("latest")
        .to_string();

    let assets = json["assets"]
        .as_array()
        .ok_or_else(|| LauncherError::NotFound("Release không có assets đính kèm".to_string()))?;

    let tmods: Vec<RemoteTmodAsset> = assets
        .iter()
        .filter_map(|a| {
            let name = a["name"].as_str()?;
            if !name.to_ascii_lowercase().ends_with(".tmod") {
                return None;
            }
            let url = a["browser_download_url"].as_str()?;
            let size_bytes = a["size"].as_u64().unwrap_or(0);
            let api_asset_url = a["url"].as_str().map(ToString::to_string);
            Some(RemoteTmodAsset {
                repo: repo.to_string(),
                tag_name: tag_name.clone(),
                asset_name: name.to_string(),
                size_bytes,
                download_url: url.to_string(),
                api_asset_url,
            })
        })
        .collect();

    if tmods.is_empty() {
        return Err(LauncherError::NotFound(format!(
            "Release {tag_name} của {repo} không có file .tmod"
        )));
    }

    Ok(tmods)
}

pub async fn download_and_install_tmod_url(
    url: &str,
    suggested_name: Option<&str>,
    paths: &LauncherPaths,
    proxy_url: Option<&str>,
    auto_enable: bool,
) -> LauncherResult<TmodMetadata> {
    download_and_install_tmod_url_with_source(url, suggested_name, None, paths, proxy_url, auto_enable)
        .await
}

pub async fn download_and_install_tmod_url_with_source(
    url: &str,
    suggested_name: Option<&str>,
    github_repo_hint: Option<&str>,
    paths: &LauncherPaths,
    proxy_url: Option<&str>,
    auto_enable: bool,
) -> LauncherResult<TmodMetadata> {
    download_and_install_tmod_url_with_source_and_progress(
        url,
        suggested_name,
        github_repo_hint,
        None,
        paths,
        proxy_url,
        auto_enable,
        |_| {},
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn download_and_install_tmod_url_with_source_and_progress(
    url: &str,
    suggested_name: Option<&str>,
    github_repo_hint: Option<&str>,
    known_size: Option<u64>,
    paths: &LauncherPaths,
    proxy_url: Option<&str>,
    auto_enable: bool,
    on_progress: impl Fn(ModDownloadProgress) + Send + Sync + 'static,
) -> LauncherResult<TmodMetadata> {
    download_and_install_tmod_url_with_auth_and_progress(
        url,
        None,
        None,
        suggested_name,
        github_repo_hint,
        known_size,
        paths,
        proxy_url,
        auto_enable,
        on_progress,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn download_and_install_tmod_url_with_auth_and_progress(
    url: &str,
    api_asset_url: Option<&str>,
    github_token: Option<&str>,
    suggested_name: Option<&str>,
    github_repo_hint: Option<&str>,
    known_size: Option<u64>,
    paths: &LauncherPaths,
    proxy_url: Option<&str>,
    auto_enable: bool,
    on_progress: impl Fn(ModDownloadProgress) + Send + Sync + 'static,
) -> LauncherResult<TmodMetadata> {
    let trimmed = url.trim();
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(LauncherError::InvalidInput(
            "URL tải mod phải bắt đầu bằng http:// hoặc https://".to_string(),
        ));
    }

    let label = suggested_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            trimmed
                .split(['?', '#'])
                .next()
                .and_then(|s| s.rsplit('/').next())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "Tải mod".to_string());
    let initial_total = known_size.filter(|&s| s > 0);
    let emit = |stage: &str, current_bytes: u64, total_bytes: Option<u64>| {
        on_progress(ModDownloadProgress {
            item_index: 1,
            item_total: 1,
            label: label.clone(),
            workshop_id: None,
            stage: stage.to_string(),
            current_bytes,
            total_bytes,
        });
    };

    emit("Đang kết nối máy chủ tải mod...", 0, initial_total);

    let mods_dir = paths.tml_mods_dir();
    fs::create_dir_all(&mods_dir)?;

    let client = build_http_client(proxy_url)?;
    let token = github_token.map(str::trim).filter(|s| !s.is_empty());
    let target_api = api_asset_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            trimmed
                .starts_with("https://api.github.com/")
                .then_some(trimmed)
        });

    let resp = if let (Some(tok), Some(api_url)) = (token, target_api) {
        let api_resp = client
            .get(api_url)
            .bearer_auth(tok)
            .header("Accept", "application/octet-stream")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| LauncherError::Network(format!("Lỗi tải mod: {e}")))?;
        if matches!(api_resp.status().as_u16(), 401 | 403)
            && !trimmed.eq_ignore_ascii_case(api_url)
            && !trimmed.starts_with("https://api.github.com/")
        {
            client
                .get(trimmed)
                .send()
                .await
                .map_err(|e| LauncherError::Network(format!("Lỗi tải mod: {e}")))?
        } else {
            api_resp
        }
    } else {
        let mut req = client.get(trimmed);
        if trimmed.starts_with("https://api.github.com/") {
            req = req
                .header("Accept", "application/octet-stream")
                .header("X-GitHub-Api-Version", "2022-11-28");
        }
        req.send()
            .await
            .map_err(|e| LauncherError::Network(format!("Lỗi tải mod: {e}")))?
    };

    if !resp.status().is_success() {
        return Err(LauncherError::Network(format!(
            "Server tải mod trả về HTTP {}",
            resp.status()
        )));
    }

    let total_bytes = resp.content_length().filter(|&n| n > 0).or(initial_total);
    emit("Đang tải tệp .tmod...", 0, total_bytes);

    let raw_name = suggested_name
        .map(ToString::to_string)
        .or_else(|| {
            trimmed
                .split('?')
                .next()
                .and_then(|s| s.rsplit('/').next())
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "downloaded_mod".to_string());

    let fallback_stem = if raw_name.to_ascii_lowercase().ends_with(".tmod") {
        raw_name[..raw_name.len() - 5].to_string()
    } else {
        raw_name
    };

    let part_path = mods_dir.join(format!(".tl_dl_{}.tmod.part", uuid::Uuid::new_v4()));
    let mut part_guard = TempFileCleanupGuard(Some(part_path.clone()));
    let mut downloaded: u64 = 0;
    {
        let mut file = File::create(&part_path)?;
        let mut stream = resp.bytes_stream();
        let mut last_emit = Instant::now();
        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res
                .map_err(|e| LauncherError::Network(format!("Lỗi đọc stream tải mod: {e}")))?;
            file.write_all(&chunk)?;
            downloaded += chunk.len() as u64;
            if last_emit.elapsed() >= Duration::from_millis(80) {
                last_emit = Instant::now();
                emit("Đang tải tệp .tmod...", downloaded, total_bytes);
            }
        }
        file.flush()?;
    }

    emit(
        "Đang kiểm tra & cài đặt .tmod...",
        downloaded,
        total_bytes.or_else(|| (downloaded > 0).then_some(downloaded)),
    );

    let parsed = match parse_tmod_file(&part_path) {
        Ok(m) => m,
        Err(e) => {
            return Err(LauncherError::InvalidFormat(format!(
                "File tải về không phải .tmod hợp lệ: {e}"
            )));
        }
    };

    let stem = if !parsed.internal_name.is_empty() {
        parsed.internal_name.clone()
    } else {
        fallback_stem
    };
    if !is_safe_mod_file_stem(&stem) {
        return Err(LauncherError::InvalidFormat(format!(
            "Tên mod '{stem}' không hợp lệ hoặc không an toàn."
        )));
    }
    let final_path = mods_dir.join(format!("{stem}.tmod"));
    let _ = fs::remove_file(&final_path);
    fs::rename(&part_path, &final_path)?;
    part_guard.0 = None;

    if !parsed.internal_name.is_empty() {
        remove_duplicate_internal_mods(&mods_dir, &final_path, &parsed.internal_name);
    }

    if auto_enable && !parsed.internal_name.is_empty() {
        set_mod_enabled(paths, &parsed.internal_name, true)?;
    }

    let meta = parse_tmod_file(&final_path)?;
    let source = if let Some(repo) = github_repo_hint
        .map(str::trim)
        .filter(|r| crate::modpack_sync::is_valid_github_repo(r))
    {
        tl_core::SharedModSource::Github {
            repo: repo.to_string(),
            url: trimmed.to_string(),
        }
    } else if let Some(repo) = crate::modpack_sync::parse_github_release_url(trimmed) {
        tl_core::SharedModSource::Github {
            repo,
            url: trimmed.to_string(),
        }
    } else {
        tl_core::SharedModSource::Url {
            url: trimmed.to_string(),
        }
    };

    if !meta.internal_name.trim().is_empty() {
        crate::modpack_sync::upsert_mod_source(
            paths,
            &meta.internal_name,
            &meta.display_name,
            &meta.version,
            &meta.sha1_hex,
            source,
        )?;
        let _ = remove_workshop_manifest_entry(paths, &meta.internal_name);
    }

    Ok(meta)
}

pub fn parse_workshop_id(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Some(trimmed.to_string());
    }
    if let Some(pos) = trimmed.find("id=") {
        let digits: String = trimmed[pos + 3..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !digits.is_empty() {
            return Some(digits);
        }
    }
    None
}

pub async fn fetch_workshop_details(
    workshop_id: &str,
    proxy_url: Option<&str>,
) -> LauncherResult<WorkshopItemDetails> {
    let workshop_id = validate_workshop_id(workshop_id)?;
    let client = build_http_client(proxy_url)?;
    let params = [
        ("itemcount", "1"),
        ("publishedfileids[0]", workshop_id),
        ("includechildren", "true"),
    ];

    let resp = match client
        .post(STEAM_GET_PUBLISHED_FILE_DETAILS_URL)
        .form(&params)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(err) => {
            return Err(classify_steam_network_error("Steam API", &err, proxy_url).await);
        }
    };

    if !resp.status().is_success() {
        return Err(LauncherError::Network(format!(
            "Steam API trả về HTTP {}",
            resp.status()
        )));
    }

    let json: serde_json::Value = match resp.json().await {
        Ok(val) => val,
        Err(err) => {
            if err.is_decode() {
                return Err(LauncherError::Network(
                    "Phản hồi Steam API không hợp lệ".to_string(),
                ));
            }
            return Err(classify_steam_network_error("Steam API", &err, proxy_url).await);
        }
    };

    let item = &json["response"]["publishedfiledetails"][0];
    if let Some(res_code) = item["result"].as_i64() {
        if res_code != 1 {
            return Err(LauncherError::NotFound(format!(
                "Không tìm thấy Workshop ID {workshop_id} (mã kết quả Steam: {res_code})"
            )));
        }
    }

    let title = item["title"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| LauncherError::NotFound("Không tìm thấy Workshop ID này".to_string()))?
        .to_string();

    let consumer_app_id = json_u64(&item["consumer_app_id"]).unwrap_or(0);
    if consumer_app_id != 0 && consumer_app_id != TMODLOADER_STEAM_APP_ID {
        return Err(LauncherError::InvalidInput(format!(
            "Workshop ID {workshop_id} không thuộc tModLoader (AppID {consumer_app_id})"
        )));
    }

    let mut details = WorkshopItemDetails {
        published_file_id: item["publishedfileid"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(workshop_id)
            .to_string(),
        title,
        ..Default::default()
    };
    apply_api_details(&mut details, item, true);

    if let Some(author) = fetch_workshop_author_best_effort(&client, workshop_id).await {
        details.author = author;
    }

    Ok(details)
}

pub async fn browse_workshop_mods(
    query: &str,
    sort: WorkshopSort,
    page: u32,
    proxy_url: Option<&str>,
) -> LauncherResult<WorkshopQueryResult> {
    let page = page.max(1);
    if let Some(id) = parse_workshop_id(query) {
        let details = fetch_workshop_details(&id, proxy_url).await?;
        return Ok(WorkshopQueryResult {
            items: vec![details],
            page: 1,
            has_next_page: false,
        });
    }

    let client = build_http_client(proxy_url)?;
    let page_str = page.to_string();
    let trimmed_query = query.trim();
    let sort_param = if !trimmed_query.is_empty() && sort == WorkshopSort::Trending {
        "textsearch"
    } else {
        sort.steam_param()
    };
    let mut params: Vec<(&str, &str)> = vec![
        ("appid", "1281930"),
        ("section", "readytouseitems"),
        ("browsesort", sort_param),
        ("actualsort", sort_param),
        ("p", &page_str),
        ("numperpage", "12"),
    ];
    if !trimmed_query.is_empty() {
        params.push(("searchtext", trimmed_query));
    }

    let resp = match client
        .get(STEAM_WORKSHOP_BROWSE_URL)
        .query(&params)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(err) => {
            return Err(classify_steam_network_error("Steam Workshop", &err, proxy_url).await);
        }
    };

    if !resp.status().is_success() {
        return Err(LauncherError::Network(format!(
            "Steam Workshop trả về HTTP {}",
            resp.status()
        )));
    }

    let html = match resp.text().await {
        Ok(text) => text,
        Err(err) => {
            return Err(classify_steam_network_error("Steam Workshop", &err, proxy_url).await);
        }
    };

    let mut result = parse_workshop_browse_html(&html, page);
    if !result.items.is_empty() {
        enrich_workshop_items_batch(&client, &mut result.items).await;
    }

    Ok(result)
}

pub async fn fetch_workshop_image_bytes(
    url: &str,
    proxy_url: Option<&str>,
) -> LauncherResult<Vec<u8>> {
    let trimmed = url.trim();
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(LauncherError::InvalidInput(
            "URL ảnh Workshop phải bắt đầu bằng http:// hoặc https://".to_string(),
        ));
    }

    let client = build_http_client(proxy_url)?;
    let resp = match client.get(trimmed).send().await {
        Ok(resp) => resp,
        Err(err) => {
            return Err(classify_steam_network_error("ảnh Steam Workshop", &err, proxy_url).await);
        }
    };

    if !resp.status().is_success() {
        return Err(LauncherError::Network(format!(
            "Server ảnh trả về HTTP {}",
            resp.status()
        )));
    }

    if let Some(len) = resp.content_length() {
        if len > MAX_WORKSHOP_IMAGE_BYTES as u64 {
            return Err(LauncherError::InvalidFormat(format!(
                "Ảnh Workshop vượt quá giới hạn {} MB",
                MAX_WORKSHOP_IMAGE_BYTES / (1024 * 1024)
            )));
        }
    }

    let mut bytes = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk_res) = stream.next().await {
        let chunk = match chunk_res {
            Ok(c) => c,
            Err(err) => {
                return Err(
                    classify_steam_network_error("ảnh Steam Workshop", &err, proxy_url).await,
                );
            }
        };
        if bytes.len().saturating_add(chunk.len()) > MAX_WORKSHOP_IMAGE_BYTES {
            return Err(LauncherError::InvalidFormat(format!(
                "Ảnh Workshop vượt quá giới hạn {} MB",
                MAX_WORKSHOP_IMAGE_BYTES / (1024 * 1024)
            )));
        }
        bytes.extend_from_slice(&chunk);
    }

    Ok(bytes)
}

pub fn parse_workshop_browse_html(html: &str, page: u32) -> WorkshopQueryResult {
    let page = page.max(1);
    if let Some(ssr_result) = parse_valve_ssr_workshop_browse(html, page) {
        return ssr_result;
    }

    let legacy = parse_legacy_workshop_browse_html(html, page);
    if !legacy.items.is_empty() {
        return legacy;
    }

    parse_ssr_dom_cards(html, page)
}

fn parse_legacy_workshop_browse_html(html: &str, page: u32) -> WorkshopQueryResult {
    let mut items = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut pos = 0usize;

    while let Some((div_start, tag_end, tag_str)) = find_next_open_tag(html, pos, "div") {
        let is_card = extract_html_attr(tag_str, "class")
            .map(|c| has_class_token(&c, "workshopItem"))
            .unwrap_or(false);
        if !is_card {
            pos = tag_end + 1;
            continue;
        }

        let Some((card_html, card_end)) = extract_balanced_div_inner(html, div_start) else {
            break;
        };
        pos = card_end;

        let Some(published_file_id) = extract_card_workshop_id(card_html) else {
            continue;
        };
        if !seen_ids.insert(published_file_id.clone()) {
            continue;
        }

        let title = extract_div_text_by_class(card_html, "workshopItemTitle").unwrap_or_default();
        let author = extract_browse_card_author(card_html).unwrap_or_default();
        let preview_url = extract_browse_card_preview_url(card_html).unwrap_or_default();

        items.push(WorkshopItemDetails {
            published_file_id,
            title,
            author,
            preview_url,
            ..Default::default()
        });
    }

    let has_next_page = detect_has_next_page(html, page);
    WorkshopQueryResult {
        items,
        page,
        has_next_page,
    }
}

fn extract_valve_ssr_json(html: &str) -> Option<serde_json::Value> {
    let mut pos = 0usize;
    while let Some((_, tag_end, tag_str)) = find_next_open_tag(html, pos, "script") {
        let content_start = tag_end + 1;
        let is_ssr_script = extract_html_attr(tag_str, "id")
            .map(|id| id.trim().eq_ignore_ascii_case("valve-ssr-data"))
            .unwrap_or(false);
        if is_ssr_script {
            let rel_close = find_ascii_ci(&html[content_start..], "</script>")?;
            let raw_json = html[content_start..content_start + rel_close].trim();
            return serde_json::from_str(raw_json).ok();
        }
        pos = content_start;
    }
    None
}

fn unwrap_json_string(val: &serde_json::Value) -> Option<serde_json::Value> {
    match val {
        serde_json::Value::String(s) => serde_json::from_str(s).ok(),
        other => Some(other.clone()),
    }
}

fn find_workshop_query_data(root: &serde_json::Value) -> Option<&serde_json::Value> {
    let queries = root.get("queries")?.as_array()?;
    let mut empty_candidate = None;
    for q in queries {
        let Some(data) = q.get("state").and_then(|s| s.get("data")) else {
            continue;
        };
        let Some(results) = data.get("results").and_then(|r| r.as_array()) else {
            continue;
        };
        if !results.is_empty()
            && results
                .iter()
                .any(|entry| entry.get("publishedfileid").is_some())
        {
            return Some(data);
        }
        if empty_candidate.is_none()
            && (data.get("current_page").is_some() || data.get("total_count").is_some())
        {
            empty_candidate = Some(data);
        }
    }
    empty_candidate
}

fn build_ssr_creator_map(data: &serde_json::Value) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(list) = data
        .get("creator_player_link_details")
        .and_then(|v| v.as_array())
    else {
        return map;
    };
    for entry in list {
        let Some(pub_data) = entry.get("public_data") else {
            continue;
        };
        let steamid = pub_data
            .get("steamid")
            .and_then(|v| v.as_str().map(str::trim).map(ToString::to_string))
            .or_else(|| json_u64(&pub_data["steamid"]).map(|n| n.to_string()));
        let persona = pub_data
            .get("persona_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);
        if let (Some(id), Some(name)) = (steamid, persona) {
            if !id.is_empty() {
                map.insert(id, name);
            }
        }
    }
    map
}

fn parse_valve_ssr_workshop_browse(html: &str, page: u32) -> Option<WorkshopQueryResult> {
    let root = extract_valve_ssr_json(html)?;
    let render_ctx = root.get("renderContext").and_then(unwrap_json_string)?;
    let query_data = render_ctx.get("queryData").and_then(unwrap_json_string)?;
    let data = find_workshop_query_data(&query_data)?;
    let results = data.get("results")?.as_array()?;

    let creator_map = build_ssr_creator_map(data);
    let dom_authors = extract_ssr_dom_authors(html);
    let mut items = Vec::with_capacity(results.len());
    let mut seen_ids = HashSet::with_capacity(results.len());

    for entry in results {
        let Some(id) = entry["publishedfileid"]
            .as_str()
            .map(str::trim)
            .map(ToString::to_string)
            .or_else(|| json_u64(&entry["publishedfileid"]).map(|n| n.to_string()))
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        if !seen_ids.insert(id.clone()) {
            continue;
        }

        let title = entry["title"]
            .as_str()
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        let creator_id = entry["creator"]
            .as_str()
            .map(str::trim)
            .map(ToString::to_string)
            .or_else(|| json_u64(&entry["creator"]).map(|n| n.to_string()))
            .unwrap_or_default();
        let author = creator_map
            .get(&creator_id)
            .cloned()
            .or_else(|| dom_authors.get(&id).cloned())
            .unwrap_or_default();

        let mut details = WorkshopItemDetails {
            published_file_id: id,
            title,
            author,
            ..Default::default()
        };
        apply_api_details(&mut details, entry, true);
        items.push(details);
    }

    let current_page = json_u64(&data["current_page"])
        .unwrap_or(page as u64)
        .max(page as u64);
    let has_more_pages = json_u64(&data["total_pages"]).is_some_and(|total| total > current_page);
    let has_cursor = data["next_cursor"]
        .as_str()
        .map(str::trim)
        .is_some_and(|c| !c.is_empty());

    Some(WorkshopQueryResult {
        items,
        page,
        has_next_page: has_more_pages || has_cursor || detect_has_next_page(html, page),
    })
}

fn strip_author_by_prefix(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .strip_prefix("By ")
        .or_else(|| collapsed.strip_prefix("by "))
        .unwrap_or(&collapsed)
        .trim()
        .to_string()
}

fn extract_ssr_dom_authors(html: &str) -> HashMap<String, String> {
    let mut authors = HashMap::new();
    let mut last_id: Option<String> = None;
    let mut pos = 0usize;

    while let Some((_, tag_end, tag_str)) = find_next_open_tag(html, pos, "a") {
        let content_start = tag_end + 1;
        let Some(rel_close) = find_ascii_ci(&html[content_start..], "</a>") else {
            break;
        };
        let close_pos = content_start + rel_close;
        let inner = &html[content_start..close_pos];
        pos = close_pos + 4;

        let Some(href) = extract_html_attr(tag_str, "href") else {
            continue;
        };
        let decoded_href = decode_html_entities(&href);
        if decoded_href.contains("sharedfiles/filedetails") {
            if let Some(id) = parse_workshop_id(&decoded_href) {
                last_id = Some(id);
            }
        } else if decoded_href.contains("myworkshopfiles") {
            if let Some(id) = last_id.take() {
                let text = strip_author_by_prefix(&decode_html_entities(&strip_html_tags(inner)));
                if !text.is_empty() {
                    authors.entry(id).or_insert(text);
                }
            }
        }
    }

    authors
}

fn parse_ssr_dom_cards(html: &str, page: u32) -> WorkshopQueryResult {
    let mut items: Vec<WorkshopItemDetails> = Vec::new();
    let mut index_by_id: HashMap<String, usize> = HashMap::new();
    let mut last_id: Option<String> = None;
    let mut pos = 0usize;

    while let Some((_, tag_end, tag_str)) = find_next_open_tag(html, pos, "a") {
        let content_start = tag_end + 1;
        let Some(rel_close) = find_ascii_ci(&html[content_start..], "</a>") else {
            break;
        };
        let close_pos = content_start + rel_close;
        let inner = &html[content_start..close_pos];
        pos = close_pos + 4;

        let Some(href) = extract_html_attr(tag_str, "href") else {
            continue;
        };
        let decoded_href = decode_html_entities(&href);

        if decoded_href.contains("sharedfiles/filedetails") {
            let Some(id) = parse_workshop_id(&decoded_href) else {
                continue;
            };
            last_id = Some(id.clone());

            let idx = *index_by_id.entry(id.clone()).or_insert_with(|| {
                items.push(WorkshopItemDetails {
                    published_file_id: id,
                    ..Default::default()
                });
                items.len() - 1
            });

            if let Some((_, _, img_tag)) = find_next_open_tag(inner, 0, "img") {
                if items[idx].preview_url.is_empty() {
                    if let Some(src) = extract_html_attr(img_tag, "src") {
                        let preview = decode_html_entities(src.trim());
                        if !preview.is_empty() {
                            items[idx].preview_url = preview;
                        }
                    }
                }
                if items[idx].title.is_empty() {
                    if let Some(alt) = extract_html_attr(img_tag, "alt") {
                        let title = decode_html_entities(alt.trim());
                        if !title.is_empty() {
                            items[idx].title = title;
                        }
                    }
                }
            }

            let text_title = decode_html_entities(&strip_html_tags(inner))
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if !text_title.is_empty() {
                items[idx].title = text_title;
            }
        } else if decoded_href.contains("myworkshopfiles") {
            if let Some(id) = last_id.as_ref() {
                if let Some(&idx) = index_by_id.get(id) {
                    if items[idx].author.is_empty() {
                        let author =
                            strip_author_by_prefix(&decode_html_entities(&strip_html_tags(inner)));
                        if !author.is_empty() {
                            items[idx].author = author;
                        }
                    }
                }
            }
        }
    }

    WorkshopQueryResult {
        items,
        page,
        has_next_page: detect_has_next_page(html, page),
    }
}

pub fn strip_steam_bbcode(raw: &str) -> String {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let bytes = normalized.as_bytes();
    let mut out = String::with_capacity(normalized.len());
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some((tag_end, inner)) = parse_bbcode_bracket(&normalized, i) {
                if let Some(tag) = classify_bbcode_tag(inner) {
                    match tag {
                        BbcodeAction::RemoveBlockUntil(close_tag) => {
                            let next_idx =
                                find_closing_bbcode_tag(&normalized, tag_end + 1, close_tag)
                                    .unwrap_or(tag_end + 1);
                            i = next_idx;
                            if out.is_empty() || out.ends_with('\n') {
                                let mut peek = i;
                                while peek < bytes.len() && matches!(bytes[peek], b' ' | b'\t') {
                                    peek += 1;
                                }
                                if peek < bytes.len() && bytes[peek] == b'\n' {
                                    i = peek + 1;
                                }
                            }
                        }
                        BbcodeAction::ListBullet => {
                            if !out.is_empty() && !out.ends_with('\n') {
                                out.push('\n');
                            }
                            out.push_str("• ");
                            i = tag_end + 1;
                            while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
                                i += 1;
                            }
                        }
                        BbcodeAction::BlockBoundary => {
                            i = tag_end + 1;
                            if !out.is_empty()
                                && !out.ends_with('\n')
                                && bytes.get(i).copied() != Some(b'\n')
                            {
                                out.push('\n');
                            }
                        }
                        BbcodeAction::CellSeparator => {
                            if !out.is_empty() && !out.ends_with('\n') && !out.ends_with(' ') {
                                out.push(' ');
                            }
                            i = tag_end + 1;
                        }
                        BbcodeAction::StripTag => {
                            i = tag_end + 1;
                        }
                    }
                    continue;
                }
            }
        }

        let ch = normalized[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    normalize_cleaned_text(&out)
}

pub fn format_unix_timestamp(ts: u64) -> String {
    if ts == 0 {
        return String::new();
    }
    let Ok(secs) = i64::try_from(ts) else {
        return String::new();
    };
    DateTime::from_timestamp(secs, 0)
        .map(|dt| dt.format("%d/%m/%Y").to_string())
        .unwrap_or_default()
}

pub fn workshop_manifest_path(paths: &LauncherPaths) -> PathBuf {
    paths.tml_mods_dir().join("workshop_manifest.json")
}

pub fn load_workshop_manifest(paths: &LauncherPaths) -> Vec<WorkshopManifestRecord> {
    let path = workshop_manifest_path(paths);
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub(crate) fn file_mtime_unix(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .filter(|&t| t > 0)
}

pub fn record_workshop_install(
    paths: &LauncherPaths,
    details: Option<&WorkshopItemDetails>,
    workshop_id: &str,
    installed_mods: &[TmodMetadata],
) -> LauncherResult<()> {
    record_workshop_install_with_timestamp(paths, details, None, workshop_id, installed_mods)
}

pub fn record_workshop_install_with_timestamp(
    paths: &LauncherPaths,
    details: Option<&WorkshopItemDetails>,
    fallback_time_updated: Option<u64>,
    workshop_id: &str,
    installed_mods: &[TmodMetadata],
) -> LauncherResult<()> {
    let workshop_id = workshop_id.trim();
    fs::create_dir_all(paths.tml_mods_dir())?;
    let mut records = load_workshop_manifest(paths);
    let installed_at = Utc::now().timestamp().max(0) as u64;

    for meta in installed_mods {
        let name_trimmed = meta.internal_name.trim();
        let previous = records
            .iter()
            .find(|r| {
                r.published_file_id == workshop_id
                    && if name_trimmed.is_empty() {
                        r.internal_name.trim().is_empty()
                    } else {
                        r.internal_name.eq_ignore_ascii_case(name_trimmed)
                    }
            })
            .cloned();

        let workshop_title = details
            .map(|d| d.title.trim())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                previous
                    .as_ref()
                    .map(|p| p.workshop_title.clone())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| meta.display_name.clone());

        let author = details
            .map(|d| d.author.trim())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                previous
                    .as_ref()
                    .map(|p| p.author.clone())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| meta.author.clone());

        let preview_url = details
            .map(|d| d.preview_url.trim())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .or_else(|| previous.as_ref().map(|p| p.preview_url.clone()))
            .unwrap_or_default();

        let time_updated = details
            .map(|d| d.time_updated)
            .filter(|&t| t > 0)
            .or_else(|| fallback_time_updated.filter(|&t| t > 0))
            .or_else(|| file_mtime_unix(&meta.file_path))
            .unwrap_or_else(|| Utc::now().timestamp().max(1) as u64);

        if name_trimmed.is_empty() {
            records.retain(|r| {
                !(r.published_file_id == workshop_id && r.internal_name.trim().is_empty())
            });
        } else {
            records.retain(|r| !r.internal_name.eq_ignore_ascii_case(name_trimmed));
        }

        let record = WorkshopManifestRecord {
            published_file_id: workshop_id.to_string(),
            internal_name: meta.internal_name.clone(),
            file_name: meta.file_name.clone(),
            display_name: meta.display_name.clone(),
            mod_version: meta.version.clone(),
            workshop_title,
            author,
            preview_url,
            time_updated,
            installed_at,
        };

        records.push(record);
    }

    let json = serde_json::to_string_pretty(&records)?;
    write_atomic(&workshop_manifest_path(paths), json.as_bytes())?;

    for meta in installed_mods {
        if !meta.internal_name.trim().is_empty() {
            crate::modpack_sync::upsert_mod_source(
                paths,
                &meta.internal_name,
                &meta.display_name,
                &meta.version,
                &meta.sha1_hex,
                tl_core::SharedModSource::Workshop {
                    id: workshop_id.to_string(),
                },
            )?;
        }
    }

    Ok(())
}

pub fn scan_staged_workshop_mods(paths: &LauncherPaths) -> LauncherResult<Vec<StagedWorkshopMod>> {
    let content_dir = workshop_staging_content_dir(paths);
    if !content_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mods_dir = paths.tml_mods_dir();
    let mut installed_by_name: HashMap<String, Vec<TmodMetadata>> = HashMap::new();
    if let Ok(entries) = fs::read_dir(&mods_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("tmod"))
                .unwrap_or(false)
            {
                if let Ok(meta) = parse_tmod_file(&p) {
                    if !meta.internal_name.is_empty() {
                        installed_by_name
                            .entry(meta.internal_name.to_ascii_lowercase())
                            .or_default()
                            .push(meta);
                    }
                }
            }
        }
    }

    let enabled_lower: HashSet<String> = load_enabled_mods(&paths.tml_enabled_json())
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let manifest = load_workshop_manifest(paths);

    let target_ym = detect_installed_tml_year_month(paths);
    let mut out = Vec::new();
    for entry in fs::read_dir(&content_dir)?.flatten() {
        let mod_dir = entry.path();
        if !mod_dir.is_dir() {
            continue;
        }
        let Some(workshop_id) = mod_dir
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
            .map(ToString::to_string)
        else {
            continue;
        };

        let mut found_tmods = Vec::new();
        collect_tmods_recursive(&mod_dir, &mut found_tmods);
        let best_tmods = pick_best_workshop_tmods(&found_tmods, &mod_dir, target_ym);
        for best_tmod in best_tmods {
            let Ok(meta) = parse_tmod_file(&best_tmod) else {
                continue;
            };

            let name_lower = meta.internal_name.to_ascii_lowercase();
            let has_manifest_match = !meta.internal_name.is_empty()
                && manifest.iter().any(|r| {
                    r.internal_name.eq_ignore_ascii_case(&meta.internal_name)
                        && r.published_file_id == workshop_id
                });
            let installed = !meta.internal_name.is_empty()
                && installed_by_name.get(&name_lower).is_some_and(|matches| {
                    matches.iter().any(|m| {
                        (!meta.sha1_hex.is_empty() && m.sha1_hex == meta.sha1_hex)
                            || has_manifest_match
                    })
                });
            let enabled =
                installed && !name_lower.is_empty() && enabled_lower.contains(&name_lower);

            let manifest_rec = manifest
                .iter()
                .find(|r| {
                    r.published_file_id == workshop_id
                        && r.internal_name.eq_ignore_ascii_case(&meta.internal_name)
                })
                .or_else(|| manifest.iter().find(|r| r.published_file_id == workshop_id));

            let workshop_title = manifest_rec
                .and_then(|r| (!r.workshop_title.is_empty()).then(|| r.workshop_title.clone()));
            let author = manifest_rec.and_then(|r| (!r.author.is_empty()).then(|| r.author.clone()));
            let preview_url =
                manifest_rec.and_then(|r| (!r.preview_url.is_empty()).then(|| r.preview_url.clone()));
            let time_updated =
                manifest_rec.and_then(|r| (r.time_updated > 0).then_some(r.time_updated));

            out.push(StagedWorkshopMod {
                workshop_id: workshop_id.clone(),
                mod_dir: mod_dir.clone(),
                meta,
                installed,
                enabled,
                workshop_title,
                author,
                preview_url,
                time_updated,
            });
        }
    }

    out.sort_by(|a, b| {
        a.meta
            .display_name
            .to_lowercase()
            .cmp(&b.meta.display_name.to_lowercase())
            .then_with(|| a.workshop_id.cmp(&b.workshop_id))
    });
    Ok(out)
}

pub fn delete_staged_workshop_mod(paths: &LauncherPaths, workshop_id: &str) -> LauncherResult<()> {
    let id = validate_workshop_id(workshop_id)?;
    let target = workshop_staging_content_dir(paths).join(id);
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    Ok(())
}

fn scan_year_month_token(text: &str, sep: u8) -> Option<(u32, u32)> {
    let bytes = text.as_bytes();
    for i in 0..bytes.len() {
        if i + 6 > bytes.len() {
            break;
        }
        if bytes[i] == b'2'
            && bytes[i + 1] == b'0'
            && bytes[i + 2].is_ascii_digit()
            && bytes[i + 3].is_ascii_digit()
            && bytes[i + 4] == sep
            && bytes[i + 5].is_ascii_digit()
        {
            if i > 0 && bytes[i - 1].is_ascii_digit() {
                continue;
            }
            let year_str = std::str::from_utf8(&bytes[i..i + 4]).ok()?;
            let mut m_end = i + 6;
            if m_end < bytes.len() && bytes[m_end].is_ascii_digit() {
                m_end += 1;
            }
            if m_end < bytes.len() && bytes[m_end].is_ascii_digit() {
                continue;
            }
            let month_str = std::str::from_utf8(&bytes[i + 5..m_end]).ok()?;
            let (Ok(year), Ok(month)) = (year_str.parse::<u32>(), month_str.parse::<u32>()) else {
                continue;
            };
            if (2020..=2099).contains(&year) && (1..=12).contains(&month) {
                return Some((year, month));
            }
        }
    }
    None
}

pub fn detect_installed_tml_year_month(paths: &LauncherPaths) -> (u32, u32) {
    let targets_path = paths.tmodloader_dir.join("tMLMod.targets");
    if let Ok(content) = fs::read_to_string(&targets_path) {
        let mut rest = content.as_str();
        while let Some(idx) = rest.find("TML_") {
            let after = &rest[idx + 4..];
            if let Some(ym) = scan_year_month_token(after, b'_') {
                return ym;
            }
            rest = after;
        }
    }

    let deps_path = paths.tmodloader_dir.join("tModLoader.deps.json");
    if let Ok(content) = fs::read_to_string(&deps_path) {
        let lower = content.to_ascii_lowercase();
        let mut search_from = 0usize;
        while let Some(rel) = lower[search_from..].find("tmodloader") {
            let start = search_from + rel + "tmodloader".len();
            let window_end = content[start..]
                .find(['}', '\n', '\r'])
                .map(|off| start + off)
                .unwrap_or(content.len())
                .min(start + 64);
            if let Some(ym) = scan_year_month_token(&content[start..window_end], b'.') {
                return ym;
            }
            search_from = start;
        }
        let simple_deps_ym = (!content.contains("\"runtimeTarget\"")
            && !content.contains("\"libraries\""))
        .then(|| scan_year_month_token(&content, b'.'))
        .flatten();
        if let Some(ym) = simple_deps_ym {
            return ym;
        }
    }

    let tml_dll_mtime = fs::metadata(paths.tmodloader_dll())
        .and_then(|m| m.modified())
        .ok();
    let logs_dir = paths.tmodloader_dir.join("tModLoader-Logs");
    for log_name in ["client.log", "server.log"] {
        let log_path = logs_dir.join(log_name);
        if let Some(dll_mtime) = tml_dll_mtime {
            let log_is_stale = fs::metadata(&log_path)
                .and_then(|m| m.modified())
                .map(|log_mtime| log_mtime < dll_mtime)
                .unwrap_or(false);
            if log_is_stale {
                continue;
            }
        }
        if let Ok(content) = fs::read_to_string(&log_path) {
            if let Some(ym) = content
                .lines()
                .take(10)
                .filter(|line| line.to_ascii_lowercase().contains("tmodloader"))
                .find_map(|line| scan_year_month_token(line, b'.'))
            {
                return ym;
            }
        }
    }

    (2026, 9)
}

fn common_ancestor(paths: &[PathBuf]) -> Option<PathBuf> {
    let first_parent = paths.first()?.parent()?;
    let mut comps: Vec<_> = first_parent.components().collect();
    for p in &paths[1..] {
        let parent = p.parent()?;
        let p_comps: Vec<_> = parent.components().collect();
        let shared = comps
            .iter()
            .zip(p_comps.iter())
            .take_while(|(a, b)| a == b)
            .count();
        comps.truncate(shared);
    }
    if comps.is_empty() {
        return None;
    }
    let mut out = PathBuf::new();
    for c in comps {
        out.push(c.as_os_str());
    }
    while let Some(name) = out.file_name().and_then(|s| s.to_str()) {
        if parse_workshop_version_tuple(name).is_some() {
            let Some(parent) = out.parent() else {
                break;
            };
            out = parent.to_path_buf();
        } else {
            break;
        }
    }
    Some(out)
}

fn tmod_workshop_rank(
    path: &Path,
    stop_dir: Option<&Path>,
    target_tml_ym: (u32, u32),
) -> (u8, (u32, u32, u32, u32)) {
    match workshop_path_version(path, stop_dir) {
        Some((major, minor, patch, rev)) if major >= 2022 => {
            if (major, minor) <= target_tml_ym {
                (3, (major, minor, patch, rev))
            } else {
                (
                    1,
                    (
                        u32::MAX - major,
                        u32::MAX - minor,
                        u32::MAX - patch,
                        u32::MAX - rev,
                    ),
                )
            }
        }
        None => (2, (0, 0, 0, 0)),
        Some(v) => (0, v),
    }
}

fn candidate_mod_group_key(path: &Path) -> String {
    if path.is_file() {
        if let Ok(meta) = parse_tmod_file(path) {
            let trimmed = meta.internal_name.trim();
            if !trimmed.is_empty() {
                return trimmed.to_ascii_lowercase();
            }
        }
    }
    path.file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| path.to_string_lossy().to_ascii_lowercase())
}

fn pick_best_workshop_tmods_inner(
    candidates: &[PathBuf],
    stop_dir: Option<&Path>,
    target_tml_ym: (u32, u32),
) -> Vec<PathBuf> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let any_exists = candidates.iter().any(|p| p.is_file());
    let pool: Vec<&PathBuf> = if any_exists {
        let valid: Vec<&PathBuf> = candidates
            .iter()
            .filter(|p| p.is_file() && parse_tmod_file(p).is_ok())
            .collect();
        if !valid.is_empty() {
            valid
        } else {
            candidates.iter().filter(|p| p.is_file()).collect()
        }
    } else {
        candidates.iter().collect()
    };

    if pool.is_empty() {
        return Vec::new();
    }

    let mut by_mod: HashMap<String, Vec<&PathBuf>> = HashMap::new();
    for cand in pool {
        by_mod
            .entry(candidate_mod_group_key(cand))
            .or_default()
            .push(cand);
    }

    let mut picked: Vec<PathBuf> = by_mod
        .into_values()
        .filter_map(|group| {
            group
                .into_iter()
                .max_by(|a, b| {
                    let ra = tmod_workshop_rank(a, stop_dir, target_tml_ym);
                    let rb = tmod_workshop_rank(b, stop_dir, target_tml_ym);
                    ra.cmp(&rb).then_with(|| a.as_os_str().cmp(b.as_os_str()))
                })
                .cloned()
        })
        .collect();

    picked.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
    picked
}

pub fn pick_best_workshop_tmods(
    candidates: &[PathBuf],
    stop_dir: &Path,
    target_tml_ym: (u32, u32),
) -> Vec<PathBuf> {
    pick_best_workshop_tmods_inner(candidates, Some(stop_dir), target_tml_ym)
}

pub fn pick_best_workshop_tmod(candidates: &[PathBuf]) -> Option<PathBuf> {
    let stop = common_ancestor(candidates);
    let target = (2026, 9);
    pick_best_workshop_tmods_inner(candidates, stop.as_deref(), target)
        .into_iter()
        .max_by(|a, b| {
            let ra = tmod_workshop_rank(a, stop.as_deref(), target);
            let rb = tmod_workshop_rank(b, stop.as_deref(), target);
            ra.cmp(&rb).then_with(|| a.as_os_str().cmp(b.as_os_str()))
        })
}

pub(crate) fn apply_proxy_env(cmd: &mut Command, proxy_url: Option<&str>) {
    if let Some(norm) = proxy_url.and_then(normalize_proxy_url) {
        cmd.env("HTTP_PROXY", &norm)
            .env("HTTPS_PROXY", &norm)
            .env("ALL_PROXY", &norm);
    }
}

pub async fn install_steamcmd_via_winget(proxy_url: Option<&str>) -> LauncherResult<String> {
    let output = crate::game_installer::run_winget_install_steamcmd(None, proxy_url).await?;
    Ok(if output.is_empty() {
        "Đã cài đặt SteamCMD qua winget".to_string()
    } else {
        output
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SteamWorkshopPhase {
    Connecting,
    Reconfiguring,
    Preallocating,
    Downloading,
    Committing,
    Finished,
}

#[derive(Debug, Clone)]
pub(crate) struct SteamContentTelemetryTracker {
    phase: SteamWorkshopPhase,
    stage: String,
    current_bytes: u64,
    total_bytes: Option<u64>,
    write_baseline: Option<u64>,
    rate_mbps: Option<f64>,
    last_emitted: Option<(Option<String>, String, u64, Option<u64>)>,
}

impl SteamContentTelemetryTracker {
    pub(crate) fn new(initial_stage: impl Into<String>, known_size: Option<u64>) -> Self {
        Self {
            phase: SteamWorkshopPhase::Connecting,
            stage: initial_stage.into(),
            current_bytes: 0,
            total_bytes: known_size.filter(|&s| s > 0),
            write_baseline: None,
            rate_mbps: None,
            last_emitted: None,
        }
    }

    pub(crate) fn phase(&self) -> SteamWorkshopPhase {
        self.phase
    }

    pub(crate) fn current_bytes(&self) -> u64 {
        self.current_bytes
    }

    pub(crate) fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    pub(crate) fn mark_emitted(
        &mut self,
        key: Option<&str>,
        stage: &str,
        current_bytes: u64,
        total_bytes: Option<u64>,
    ) {
        self.last_emitted = Some((
            key.map(ToString::to_string),
            stage.to_string(),
            current_bytes,
            total_bytes,
        ));
    }

    fn reset_state(&mut self, phase: SteamWorkshopPhase, stage: &str) {
        self.phase = phase;
        self.stage = stage.to_string();
        self.current_bytes = 0;
        self.total_bytes = None;
        self.write_baseline = None;
        self.rate_mbps = None;
    }

    pub(crate) fn reset_for_new_item(&mut self, stage: &str) {
        self.reset_state(SteamWorkshopPhase::Reconfiguring, stage);
    }

    pub(crate) fn reset_idle(&mut self, stage: &str) {
        self.reset_state(SteamWorkshopPhase::Connecting, stage);
    }

    fn format_downloading_stage(rate_mbps: Option<f64>) -> String {
        match rate_mbps {
            Some(mbps) if mbps.is_finite() && mbps >= 0.8 => {
                format!("Đang tải dữ liệu Workshop ({:.1} MB/s)...", mbps / 8.0)
            }
            _ => "Đang tải dữ liệu Workshop...".to_string(),
        }
    }

    pub(crate) fn apply_stdout_line(&mut self, line: &str) -> bool {
        if !matches!(
            self.phase,
            SteamWorkshopPhase::Connecting | SteamWorkshopPhase::Reconfiguring
        ) {
            return false;
        }
        let Some(next_stage) = parse_steamcmd_stdout_stage(line) else {
            return false;
        };
        if self.stage == next_stage {
            return false;
        }
        self.stage = next_stage.to_string();
        true
    }

    pub(crate) fn apply_telemetry(&mut self, lines: &[String], current_written: Option<u64>) {
        for line in lines {
            if let Some((dl_total, stage_total)) = parse_steam_content_log_update_started(line) {
                let best_total = if stage_total > 0 { stage_total } else { dl_total };
                if best_total > 0 {
                    self.total_bytes = Some(best_total);
                }
                self.current_bytes = 0;
                self.write_baseline = None;
                self.rate_mbps = None;
                if matches!(
                    self.phase,
                    SteamWorkshopPhase::Finished | SteamWorkshopPhase::Committing
                ) {
                    self.phase = SteamWorkshopPhase::Reconfiguring;
                }
            }

            if let Some(new_phase) = parse_steam_content_log_phase(line) {
                match new_phase {
                    SteamWorkshopPhase::Connecting => {}
                    SteamWorkshopPhase::Reconfiguring => {
                        if matches!(
                            self.phase,
                            SteamWorkshopPhase::Connecting
                                | SteamWorkshopPhase::Reconfiguring
                                | SteamWorkshopPhase::Finished
                        ) {
                            if self.phase == SteamWorkshopPhase::Finished {
                                self.write_baseline = None;
                                self.rate_mbps = None;
                            }
                            self.phase = SteamWorkshopPhase::Reconfiguring;
                            self.stage = "Đang chuẩn bị tải Workshop...".to_string();
                        }
                    }
                    SteamWorkshopPhase::Preallocating => {
                        if !matches!(
                            self.phase,
                            SteamWorkshopPhase::Downloading | SteamWorkshopPhase::Committing
                        ) {
                            self.phase = SteamWorkshopPhase::Preallocating;
                            self.stage = "Đang cấp phát dung lượng Workshop...".to_string();
                        }
                    }
                    SteamWorkshopPhase::Downloading => {
                        if self.phase != SteamWorkshopPhase::Committing {
                            if self.phase != SteamWorkshopPhase::Downloading {
                                self.write_baseline = current_written;
                            }
                            self.phase = SteamWorkshopPhase::Downloading;
                            self.stage = Self::format_downloading_stage(self.rate_mbps);
                        }
                    }
                    SteamWorkshopPhase::Committing => {
                        self.phase = SteamWorkshopPhase::Committing;
                        self.stage = "Đang hoàn tất gói Workshop...".to_string();
                    }
                    SteamWorkshopPhase::Finished => {
                        self.phase = SteamWorkshopPhase::Finished;
                        if let Some(total) = self.total_bytes.filter(|&t| t > 0) {
                            self.current_bytes = total;
                        }
                        self.stage = "Đang kiểm tra tệp .tmod...".to_string();
                    }
                }
            }

            if let Some(rate) = parse_steam_content_log_rate_mbps(line) {
                self.rate_mbps = Some(rate);
                if self.phase == SteamWorkshopPhase::Downloading {
                    self.stage = Self::format_downloading_stage(self.rate_mbps);
                }
            }
        }

        if self.phase == SteamWorkshopPhase::Downloading {
            if let Some(written) = current_written {
                let baseline = *self.write_baseline.get_or_insert(written);
                let delta = written.saturating_sub(baseline);
                self.current_bytes = match self.total_bytes {
                    Some(t) if t > 1 => delta.min(t - 1),
                    Some(t) => delta.min(t),
                    None => delta,
                };
            }
            self.stage = Self::format_downloading_stage(self.rate_mbps);
        }
    }

    pub(crate) fn take_changed_for_key(
        &mut self,
        key: Option<&str>,
    ) -> Option<(String, u64, Option<u64>)> {
        let changed = match &self.last_emitted {
            Some((prev_key, prev_stage, prev_cur, prev_tot)) => {
                prev_key.as_deref() != key
                    || prev_stage != &self.stage
                    || *prev_cur != self.current_bytes
                    || *prev_tot != self.total_bytes
            }
            None => true,
        };
        if !changed {
            return None;
        }
        self.last_emitted = Some((
            key.map(ToString::to_string),
            self.stage.clone(),
            self.current_bytes,
            self.total_bytes,
        ));
        Some((self.stage.clone(), self.current_bytes, self.total_bytes))
    }
}

fn extract_slash_pair_total(text: &str, key: &str) -> Option<u64> {
    let idx = find_ascii_ci(text, key)?;
    let after = text[idx + key.len()..].trim_start();
    let done_len = after.bytes().take_while(u8::is_ascii_digit).count();
    if done_len == 0 {
        return None;
    }
    let total_str = after[done_len..].strip_prefix('/')?;
    let total_len = total_str.bytes().take_while(u8::is_ascii_digit).count();
    if total_len == 0 {
        return None;
    }
    total_str[..total_len].parse::<u64>().ok()
}

pub(crate) fn parse_steam_content_log_update_started(line: &str) -> Option<(u64, u64)> {
    let marker = "appid 1281930 update started";
    let idx = find_ascii_ci(line, marker)?;
    let tail = &line[idx + marker.len()..];
    let dl_total = extract_slash_pair_total(tail, "download ")?;
    let stage_total = extract_slash_pair_total(tail, "stage ")?;
    Some((dl_total, stage_total))
}

pub(crate) fn parse_steam_content_log_phase(line: &str) -> Option<SteamWorkshopPhase> {
    let marker = "appid 1281930 workshop update changed :";
    let idx = find_ascii_ci(line, marker)?;
    let portion = line[idx + marker.len()..].trim();
    if portion.eq_ignore_ascii_case("none") {
        return Some(SteamWorkshopPhase::Finished);
    }
    let lower = portion.to_ascii_lowercase();
    if lower.contains("committing") {
        Some(SteamWorkshopPhase::Committing)
    } else if lower.contains("downloading") || lower.contains("staging") {
        Some(SteamWorkshopPhase::Downloading)
    } else if lower.contains("preallocating") {
        Some(SteamWorkshopPhase::Preallocating)
    } else if lower.contains("reconfiguring") || lower.contains("running update") {
        Some(SteamWorkshopPhase::Reconfiguring)
    } else {
        None
    }
}

pub(crate) fn parse_steam_content_log_rate_mbps(line: &str) -> Option<f64> {
    let marker = "increasing target number of download connections";
    let idx = find_ascii_ci(line, marker)?;
    let tail = &line[idx + marker.len()..];
    let now_idx = find_ascii_ci(tail, "now ")?;
    let after_now = tail[now_idx + 4..].trim_start();
    let token = after_now
        .split(|c: char| c == ')' || c.is_whitespace())
        .next()?;
    let val: f64 = token.parse().ok()?;
    (val.is_finite() && val > 0.0).then_some(val)
}

pub(crate) fn parse_steamcmd_stdout_stage(line: &str) -> Option<&'static str> {
    if find_ascii_ci(line, "connecting anonymously").is_some() {
        Some("Đang đăng nhập SteamCMD...")
    } else if find_ascii_ci(line, "waiting for client config").is_some()
        || find_ascii_ci(line, "waiting for user info").is_some()
    {
        Some("Đang xác thực phiên Steam...")
    } else if find_ascii_ci(line, "downloading item ").is_some() {
        Some("Đang kiểm tra gói Workshop...")
    } else if find_ascii_ci(line, "downloaded item ").is_some()
        || find_ascii_ci(line, "success. downloaded item").is_some()
    {
        Some("Đang cài đặt tệp .tmod...")
    } else {
        None
    }
}

pub(crate) fn extract_steamcmd_downloading_item_id(line: &str) -> Option<String> {
    let needle = "downloading item ";
    let idx = find_ascii_ci(line, needle)?;
    let after = &line[idx + needle.len()..];
    let digits: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    (!digits.is_empty()).then_some(digits)
}

pub(crate) fn steamcmd_content_log_path(steamcmd_exe: &Path) -> Option<PathBuf> {
    steamcmd_exe
        .parent()
        .map(|dir| dir.join("logs").join("content_log.txt"))
}

pub(crate) fn current_file_len(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

pub(crate) fn read_process_written_bytes(sys: &mut sysinfo::System, pid: u32) -> Option<u64> {
    let spid = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[spid]),
        true,
        sysinfo::ProcessRefreshKind::nothing().with_disk_usage(),
    );
    sys.process(spid)
        .map(|p| p.disk_usage().total_written_bytes)
}

pub(crate) fn read_new_complete_lines(path: &Path, offset: &mut u64) -> Vec<String> {
    const MAX_READ_BYTES: usize = 256 * 1024;
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len < *offset {
        *offset = 0;
    }
    if file.seek(SeekFrom::Start(*offset)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    while buf.len() < MAX_READ_BYTES {
        let limit = (MAX_READ_BYTES - buf.len()).min(chunk.len());
        match file.read(&mut chunk[..limit]) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    if buf.is_empty() {
        return Vec::new();
    }
    let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') else {
        return Vec::new();
    };
    *offset += (last_nl + 1) as u64;
    String::from_utf8_lossy(&buf[..=last_nl])
        .lines()
        .map(ToString::to_string)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn spawn_workshop_pipe_reader<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    stream: R,
    wid: String,
    label: String,
    item_index: usize,
    item_total: usize,
    marker: Arc<AtomicBool>,
    log_cb: Arc<impl Fn(String) + Send + Sync + 'static>,
    progress_cb: Arc<impl Fn(ModDownloadProgress) + Send + Sync + 'static>,
    tracker: Arc<Mutex<SteamContentTelemetryTracker>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            if line_indicates_workshop_downloaded(&line, &wid) {
                marker.store(true, Ordering::Release);
            }
            let changed = tracker
                .lock()
                .ok()
                .and_then(|mut trk| {
                    trk.apply_stdout_line(&line)
                        .then(|| trk.take_changed_for_key(Some(&wid)))
                        .flatten()
                });
            if let Some((stage, current_bytes, total_bytes)) = changed {
                progress_cb(ModDownloadProgress {
                    item_index,
                    item_total,
                    label: label.clone(),
                    workshop_id: Some(wid.clone()),
                    stage,
                    current_bytes,
                    total_bytes,
                });
            }
            log_cb(line);
        }
    })
}

pub async fn download_workshop_mod_via_steamcmd(
    steamcmd_exe: &Path,
    paths: &LauncherPaths,
    workshop_id: &str,
    proxy_url: Option<&str>,
    auto_enable: bool,
    on_log: impl Fn(String) + Send + Sync + 'static,
) -> LauncherResult<Vec<TmodMetadata>> {
    download_workshop_mod_via_steamcmd_with_progress(
        steamcmd_exe,
        paths,
        workshop_id,
        proxy_url,
        auto_enable,
        None,
        "",
        1,
        1,
        on_log,
        |_| {},
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn download_workshop_mod_via_steamcmd_with_progress(
    steamcmd_exe: &Path,
    paths: &LauncherPaths,
    workshop_id: &str,
    proxy_url: Option<&str>,
    auto_enable: bool,
    known_size: Option<u64>,
    display_label: &str,
    item_index: usize,
    item_total: usize,
    on_log: impl Fn(String) + Send + Sync + 'static,
    on_progress: impl Fn(ModDownloadProgress) + Send + Sync + 'static,
) -> LauncherResult<Vec<TmodMetadata>> {
    if !steamcmd_exe.is_file() {
        return Err(LauncherError::NotFound(format!(
            "Không tìm thấy steamcmd.exe tại {}",
            steamcmd_exe.display()
        )));
    }
    let workshop_id = validate_workshop_id(workshop_id)?;

    let staging_dir = paths.steamcmd_dir.join("staging");
    fs::create_dir_all(&staging_dir)?;

    let workshop_item_dir = workshop_staging_content_dir(paths).join(workshop_id);
    let before_snapshot = snapshot_tmod_files(&workshop_item_dir);

    let label = if display_label.trim().is_empty() {
        format!("Workshop ID {workshop_id}")
    } else {
        display_label.trim().to_string()
    };
    let content_log_path = steamcmd_content_log_path(steamcmd_exe);
    let initial_log_offset = content_log_path
        .as_deref()
        .map(current_file_len)
        .unwrap_or(0);
    let initial_total = known_size.filter(|&s| s > 0);
    let initial_stage = "Đang khởi động SteamCMD...".to_string();

    let progress_cb = Arc::new(on_progress);
    progress_cb(ModDownloadProgress {
        item_index,
        item_total,
        label: label.clone(),
        workshop_id: Some(workshop_id.to_string()),
        stage: initial_stage.clone(),
        current_bytes: 0,
        total_bytes: initial_total,
    });

    let mut cmd = Command::new(steamcmd_exe);
    cmd.arg("+force_install_dir")
        .arg(&staging_dir)
        .arg("+login")
        .arg("anonymous")
        .arg("+workshop_download_item")
        .arg("1281930")
        .arg(workshop_id)
        .arg("+quit");
    apply_proxy_env(&mut cmd, proxy_url);

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| LauncherError::Process(format!("Không thể chạy steamcmd.exe: {e}")))?;

    let child_pid = child.id();
    let log_cb = Arc::new(on_log);
    let saw_downloaded_marker = Arc::new(AtomicBool::new(false));
    let tracker = Arc::new(Mutex::new({
        let mut trk = SteamContentTelemetryTracker::new(&initial_stage, initial_total);
        trk.mark_emitted(Some(workshop_id), &initial_stage, 0, initial_total);
        trk
    }));

    let stdout_task = child.stdout.take().map(|stdout| {
        spawn_workshop_pipe_reader(
            stdout,
            workshop_id.to_string(),
            label.clone(),
            item_index,
            item_total,
            Arc::clone(&saw_downloaded_marker),
            Arc::clone(&log_cb),
            Arc::clone(&progress_cb),
            Arc::clone(&tracker),
        )
    });
    let stderr_task = child.stderr.take().map(|stderr| {
        spawn_workshop_pipe_reader(
            stderr,
            workshop_id.to_string(),
            label.clone(),
            item_index,
            item_total,
            Arc::clone(&saw_downloaded_marker),
            Arc::clone(&log_cb),
            Arc::clone(&progress_cb),
            Arc::clone(&tracker),
        )
    });

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_notify = Arc::new(Notify::new());
    let monitor_task = {
        let stop_flag = Arc::clone(&stop_flag);
        let stop_notify = Arc::clone(&stop_notify);
        let tracker = Arc::clone(&tracker);
        let progress_cb = Arc::clone(&progress_cb);
        let label = label.clone();
        let wid = workshop_id.to_string();
        tokio::spawn(async move {
            let mut sys = sysinfo::System::new().ok();
            let mut log_offset = initial_log_offset;
            let mut ticker = tokio::time::interval(Duration::from_millis(200));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        if stop_flag.load(Ordering::Acquire) {
                            break;
                        }
                        let lines = content_log_path
                            .as_deref()
                            .map(|p| read_new_complete_lines(p, &mut log_offset))
                            .unwrap_or_default();
                        let written = sys
                            .as_mut()
                            .zip(child_pid)
                            .and_then(|(s, pid)| read_process_written_bytes(s, pid));
                        let changed = tracker.lock().ok().and_then(|mut trk| {
                            trk.apply_telemetry(&lines, written);
                            trk.take_changed_for_key(Some(&wid))
                        });
                        if let Some((stage, current_bytes, total_bytes)) = changed {
                            progress_cb(ModDownloadProgress {
                                item_index,
                                item_total,
                                label: label.clone(),
                                workshop_id: Some(wid.clone()),
                                stage,
                                current_bytes,
                                total_bytes,
                            });
                        }
                    }
                    _ = stop_notify.notified() => {
                        break;
                    }
                }
            }
        })
    };

    let status = child
        .wait()
        .await
        .map_err(|e| LauncherError::Process(format!("Lỗi chờ steamcmd.exe: {e}")))?;

    stop_flag.store(true, Ordering::Release);
    stop_notify.notify_waiters();
    let _ = monitor_task.await;
    if let Some(h) = stdout_task {
        let _ = h.await;
    }
    if let Some(h) = stderr_task {
        let _ = h.await;
    }

    let any_changed_or_new = has_any_tmod_changed_or_new(&workshop_item_dir, &before_snapshot);
    let saw_marker = saw_downloaded_marker.load(Ordering::Acquire);
    if !should_accept_workshop_download(status.success(), saw_marker, any_changed_or_new) {
        return Err(LauncherError::Process(format!(
            "SteamCMD thất bại (mã thoát {:?}) khi tải Workshop item {workshop_id}.",
            status.code()
        )));
    }

    let mut found_tmods = Vec::new();
    collect_tmods_recursive(&workshop_item_dir, &mut found_tmods);

    let target_ym = detect_installed_tml_year_month(paths);
    let best_tmods = pick_best_workshop_tmods(&found_tmods, &workshop_item_dir, target_ym);
    if best_tmods.is_empty() {
        return Err(LauncherError::NotFound(format!(
            "SteamCMD kết thúc ({:?}) nhưng không tìm thấy file .tmod trong {}. Nếu Steam bị chặn kết nối, hãy bật Proxy/VPN hoặc dùng GitHub/Import .tmod.",
            status.code(),
            workshop_item_dir.display()
        )));
    }

    let (tracked_bytes, tracked_total) = tracker
        .lock()
        .map(|trk| (trk.current_bytes(), trk.total_bytes()))
        .unwrap_or((0, initial_total));
    let total_bytes = tracked_total.or_else(|| {
        let sum: u64 = best_tmods
            .iter()
            .filter_map(|p| fs::metadata(p).ok().map(|m| m.len()))
            .sum();
        (sum > 0).then_some(sum)
    });
    let final_bytes = total_bytes.unwrap_or(tracked_bytes);
    progress_cb(ModDownloadProgress {
        item_index,
        item_total,
        label,
        workshop_id: Some(workshop_id.to_string()),
        stage: "Đang cài đặt tệp .tmod...".to_string(),
        current_bytes: final_bytes,
        total_bytes,
    });

    import_local_mod_files(&best_tmods, paths, auto_enable)
}

pub(crate) fn validate_workshop_id(workshop_id: &str) -> LauncherResult<&str> {
    let trimmed = workshop_id.trim();
    if trimmed.is_empty() || !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Err(LauncherError::InvalidInput(
            "Workshop ID chỉ gồm chữ số".to_string(),
        ));
    }
    Ok(trimmed)
}

pub(crate) fn workshop_staging_content_dir(paths: &LauncherPaths) -> PathBuf {
    paths
        .steamcmd_dir
        .join("staging")
        .join("steamapps")
        .join("workshop")
        .join("content")
        .join("1281930")
}

pub(crate) fn collect_tmods_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_tmods_recursive(&p, out);
        } else if p
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("tmod"))
            .unwrap_or(false)
        {
            out.push(p);
        }
    }
}

pub(crate) type TmodFileSnapshot = HashMap<PathBuf, (Option<SystemTime>, u64)>;

pub(crate) fn snapshot_tmod_files(dir: &Path) -> TmodFileSnapshot {
    let mut paths = Vec::new();
    collect_tmods_recursive(dir, &mut paths);
    let mut out = HashMap::with_capacity(paths.len());
    for p in paths {
        if let Ok(meta) = fs::metadata(&p) {
            out.insert(p, (meta.modified().ok(), meta.len()));
        }
    }
    out
}

pub(crate) fn has_any_tmod_changed_or_new(dir: &Path, before: &TmodFileSnapshot) -> bool {
    let after = snapshot_tmod_files(dir);
    after.iter().any(|(path, state)| match before.get(path) {
        None => true,
        Some(prev) => prev != state,
    })
}

pub(crate) fn line_indicates_workshop_downloaded(line: &str, workshop_id: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains(&format!("downloaded item {workshop_id}"))
        || lower.contains("success. downloaded item")
}

pub(crate) fn should_accept_workshop_download(
    success: bool,
    saw_marker: bool,
    any_changed_or_new: bool,
) -> bool {
    success || saw_marker || any_changed_or_new
}

fn parse_workshop_version_tuple(segment: &str) -> Option<(u32, u32, u32, u32)> {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parts: Vec<&str> = trimmed.split('.').collect();
    if !(2..=4).contains(&parts.len()) {
        return None;
    }
    if parts
        .iter()
        .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let major = parts[0].parse::<u32>().ok()?;
    let minor = parts[1].parse::<u32>().ok()?;
    let patch = parts.get(2).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    let rev = parts.get(3).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    Some((major, minor, patch, rev))
}

fn workshop_path_version(path: &Path, stop_dir: Option<&Path>) -> Option<(u32, u32, u32, u32)> {
    let mut dir = path.parent();
    while let Some(current) = dir {
        if stop_dir == Some(current) {
            break;
        }
        if let Some(name) = current.file_name().and_then(|s| s.to_str()) {
            if let Some(ver) = parse_workshop_version_tuple(name) {
                return Some(ver);
            }
        }
        dir = current.parent();
    }
    None
}

fn json_u64(val: &serde_json::Value) -> Option<u64> {
    val.as_u64()
        .or_else(|| val.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
}

fn apply_api_details(
    target: &mut WorkshopItemDetails,
    item: &serde_json::Value,
    overwrite_existing: bool,
) {
    if let Some(title) = item["title"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if overwrite_existing || target.title.is_empty() {
            target.title = title.to_string();
        }
    }
    if let Some(size) = json_u64(&item["file_size"]) {
        if overwrite_existing || size > 0 {
            target.file_size = size;
        }
    }
    if let Some(updated) = json_u64(&item["time_updated"]) {
        if overwrite_existing || updated > 0 {
            target.time_updated = updated;
        }
    }
    if let Some(subs) = json_u64(&item["subscriptions"]) {
        if overwrite_existing || subs > 0 {
            target.subscriptions = subs;
        }
    }
    if let Some(fav) = json_u64(&item["favorited"]) {
        if overwrite_existing || fav > 0 {
            target.favorited = fav;
        }
    }
    if let Some(views) = json_u64(&item["views"]) {
        if overwrite_existing || views > 0 {
            target.views = views;
        }
    }
    if let Some(app_id) = json_u64(&item["consumer_app_id"]) {
        if overwrite_existing || app_id > 0 {
            target.consumer_app_id = app_id;
        }
    }

    if let Some(preview) = item["preview_url"].as_str().map(str::trim) {
        if !preview.is_empty() {
            target.preview_url = preview.to_string();
        }
    }

    if let Some(raw_desc) = item["file_description"]
        .as_str()
        .or_else(|| item["description"].as_str())
        .or_else(|| item["short_description"].as_str())
    {
        let cleaned = strip_steam_bbcode(raw_desc);
        if overwrite_existing || !cleaned.is_empty() {
            target.description = cleaned;
        }
    }

    if let Some(arr) = item["tags"].as_array() {
        let parsed_tags: Vec<String> = arr
            .iter()
            .filter_map(|t| {
                t["display_name"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .or_else(|| t["tag"].as_str().map(str::trim).filter(|s| !s.is_empty()))
                    .map(ToString::to_string)
            })
            .collect();
        if overwrite_existing || !parsed_tags.is_empty() {
            target.tags = parsed_tags;
        }
    }

    if let Some(arr) = item["children"].as_array() {
        let parsed_children: Vec<String> = arr
            .iter()
            .filter_map(|c| {
                c["publishedfileid"]
                    .as_str()
                    .map(str::trim)
                    .map(ToString::to_string)
                    .or_else(|| c["publishedfileid"].as_u64().map(|n| n.to_string()))
                    .filter(|s| !s.is_empty() && s.chars().all(|ch| ch.is_ascii_digit()))
            })
            .collect();
        if overwrite_existing || !parsed_children.is_empty() {
            target.child_ids = parsed_children;
        }
    }
}

pub(crate) async fn enrich_workshop_items_batch(
    client: &reqwest::Client,
    items: &mut [WorkshopItemDetails],
) {
    if items.is_empty() {
        return;
    }

    for chunk in items.chunks_mut(100) {
        let mut params: Vec<(String, String)> = Vec::with_capacity(chunk.len() + 2);
        params.push(("itemcount".to_string(), chunk.len().to_string()));
        params.push(("includechildren".to_string(), "true".to_string()));
        for (idx, item) in chunk.iter().enumerate() {
            params.push((
                format!("publishedfileids[{idx}]"),
                item.published_file_id.clone(),
            ));
        }

        let fut = async {
            let resp = client
                .post(STEAM_GET_PUBLISHED_FILE_DETAILS_URL)
                .form(&params)
                .timeout(Duration::from_secs(12))
                .send()
                .await
                .ok()?;
            if !resp.status().is_success() {
                return None;
            }
            resp.json::<serde_json::Value>().await.ok()
        };

        let Ok(Some(json)) = tokio::time::timeout(Duration::from_secs(12), fut).await else {
            continue;
        };

        let Some(details_arr) = json["response"]["publishedfiledetails"].as_array() else {
            continue;
        };

        let mut by_id: HashMap<String, &serde_json::Value> =
            HashMap::with_capacity(details_arr.len());
        for entry in details_arr {
            if let Some(res_code) = entry["result"].as_i64() {
                if res_code != 1 {
                    continue;
                }
            }
            let id_opt = entry["publishedfileid"]
                .as_str()
                .map(str::trim)
                .map(ToString::to_string)
                .or_else(|| entry["publishedfileid"].as_u64().map(|n| n.to_string()));
            if let Some(id) = id_opt.filter(|s| !s.is_empty()) {
                by_id.insert(id, entry);
            }
        }

        for item in chunk.iter_mut() {
            if let Some(api_item) = by_id.get(&item.published_file_id) {
                apply_api_details(item, api_item, false);
            }
        }
    }
}

async fn fetch_workshop_author_best_effort(
    client: &reqwest::Client,
    workshop_id: &str,
) -> Option<String> {
    let url = format!("https://steamcommunity.com/sharedfiles/filedetails/?id={workshop_id}");
    let fut = async {
        let resp = client
            .get(&url)
            .timeout(Duration::from_secs(6))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let html = resp.text().await.ok()?;
        extract_filedetails_author(&html)
    };
    tokio::time::timeout(Duration::from_secs(6), fut)
        .await
        .ok()
        .flatten()
}

fn extract_filedetails_author(html: &str) -> Option<String> {
    let mut pos = 0usize;
    while let Some((div_start, tag_end, tag_str)) = find_next_open_tag(html, pos, "div") {
        let is_creators = extract_html_attr(tag_str, "class")
            .map(|c| has_class_token(&c, "creatorsBlock"))
            .unwrap_or(false);
        if !is_creators {
            pos = tag_end + 1;
            continue;
        }
        let (block_html, _) = extract_balanced_div_inner(html, div_start)?;
        let mut authors = Vec::new();
        let mut inner_pos = 0usize;
        while let Some((fb_start, fb_end, fb_tag)) =
            find_next_open_tag(block_html, inner_pos, "div")
        {
            let is_content = extract_html_attr(fb_tag, "class")
                .map(|c| has_class_token(&c, "friendBlockContent"))
                .unwrap_or(false);
            if !is_content {
                inner_pos = fb_end + 1;
                continue;
            }
            if let Some((fb_inner, fb_close)) = extract_balanced_div_inner(block_html, fb_start) {
                let raw_name = fb_inner.split('<').next().unwrap_or(fb_inner);
                let name = decode_html_entities(raw_name).trim().to_string();
                if !name.is_empty() {
                    authors.push(name);
                }
                inner_pos = fb_close;
            } else {
                inner_pos = fb_end + 1;
            }
        }
        if !authors.is_empty() {
            return Some(authors.join(", "));
        }
        break;
    }

    extract_browse_card_author(html)
}

fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    debug_assert!(needle.is_ascii());
    if needle.is_empty() {
        return Some(0);
    }
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    h.windows(n.len()).position(|w| w.eq_ignore_ascii_case(n))
}

fn find_tag_end(html: &str, start: usize) -> Option<usize> {
    let bytes = html.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = start;
    while i < bytes.len() {
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

fn find_next_open_tag<'a>(
    html: &'a str,
    from: usize,
    tag_name: &str,
) -> Option<(usize, usize, &'a str)> {
    debug_assert!(tag_name.is_ascii());
    let mut pos = from;
    let bytes = html.as_bytes();
    let needle = format!("<{tag_name}");

    while let Some(rel) = find_ascii_ci(&html[pos..], &needle) {
        let start = pos + rel;
        let after_name = start + needle.len();
        let next_byte = bytes.get(after_name).copied();
        if matches!(
            next_byte,
            Some(b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/')
        ) {
            let end = find_tag_end(html, start)?;
            return Some((start, end, &html[start..=end]));
        }
        pos = after_name;
    }
    None
}

fn extract_balanced_div_inner(html: &str, open_tag_start: usize) -> Option<(&str, usize)> {
    let open_end = find_tag_end(html, open_tag_start)?;
    let content_start = open_end + 1;
    let bytes = html.as_bytes();
    let mut depth = 1usize;
    let mut pos = content_start;

    while let Some(rel) = html[pos..].find('<') {
        let lt = pos + rel;
        if bytes.len() >= lt + 5
            && bytes[lt..lt + 5].eq_ignore_ascii_case(b"</div")
            && matches!(
                bytes.get(lt + 5).copied(),
                Some(b' ' | b'\t' | b'\r' | b'\n' | b'>')
            )
        {
            depth -= 1;
            let close_end = find_tag_end(html, lt)?;
            if depth == 0 {
                return Some((&html[content_start..lt], close_end + 1));
            }
            pos = close_end + 1;
        } else if bytes.len() >= lt + 4
            && bytes[lt..lt + 4].eq_ignore_ascii_case(b"<div")
            && matches!(
                bytes.get(lt + 4).copied(),
                Some(b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/')
            )
        {
            depth += 1;
            let inner_end = find_tag_end(html, lt)?;
            pos = inner_end + 1;
        } else {
            pos = lt + 1;
        }
    }

    Some((&html[content_start..], html.len()))
}

fn extract_html_attr(tag_str: &str, attr_name: &str) -> Option<String> {
    let bytes = tag_str.as_bytes();
    let mut i = 1usize;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
        i += 1;
    }

    while i < bytes.len() {
        while i < bytes.len()
            && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/' || bytes[i] == b'>')
        {
            if bytes[i] == b'>' {
                return None;
            }
            i += 1;
        }
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'>'
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let name = &tag_str[name_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let val = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let q = bytes[i];
                i += 1;
                let val_start = i;
                while i < bytes.len() && bytes[i] != q {
                    i += 1;
                }
                let slice = &tag_str[val_start..i];
                if i < bytes.len() {
                    i += 1;
                }
                slice
            } else {
                let val_start = i;
                while i < bytes.len()
                    && !bytes[i].is_ascii_whitespace()
                    && bytes[i] != b'>'
                    && bytes[i] != b'/'
                {
                    i += 1;
                }
                &tag_str[val_start..i]
            };
            if name.eq_ignore_ascii_case(attr_name) {
                return Some(val.to_string());
            }
        }
    }

    None
}

fn has_class_token(class_attr: &str, token: &str) -> bool {
    class_attr
        .split_whitespace()
        .any(|t| t.eq_ignore_ascii_case(token))
}

fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    let mut quote: Option<char> = None;

    for ch in input.chars() {
        if in_tag {
            match quote {
                Some(q) if ch == q => quote = None,
                Some(_) => {}
                None if ch == '"' || ch == '\'' => quote = Some(ch),
                None if ch == '>' => in_tag = false,
                None => {}
            }
        } else if ch == '<' {
            in_tag = true;
        } else {
            out.push(ch);
        }
    }
    out
}

fn decode_single_entity(entity: &str) -> Option<char> {
    if let Some(hex) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
        let code = u32::from_str_radix(hex, 16).ok()?;
        return (code != 0).then(|| char::from_u32(code)).flatten();
    }
    if let Some(dec) = entity.strip_prefix('#') {
        let code = dec.parse::<u32>().ok()?;
        return (code != 0).then(|| char::from_u32(code)).flatten();
    }
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        "ndash" => Some('–'),
        "mdash" => Some('—'),
        "hellip" => Some('…'),
        "lsquo" => Some('‘'),
        "rsquo" => Some('’'),
        "ldquo" => Some('“'),
        "rdquo" => Some('”'),
        _ => None,
    }
}

fn decode_html_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp_idx) = rest.find('&') {
        out.push_str(&rest[..amp_idx]);
        let after_amp = &rest[amp_idx + 1..];
        if let Some(semi_idx) = after_amp.find(';') {
            if semi_idx > 0 && semi_idx <= 12 {
                let entity = &after_amp[..semi_idx];
                if let Some(decoded) = decode_single_entity(entity) {
                    out.push(decoded);
                    rest = &after_amp[semi_idx + 1..];
                    continue;
                }
            }
        }
        out.push('&');
        rest = after_amp;
    }
    out.push_str(rest);
    out
}

fn extract_card_workshop_id(card_html: &str) -> Option<String> {
    let mut pos = 0usize;
    while let Some((_, tag_end, tag_str)) = find_next_open_tag(card_html, pos, "a") {
        if let Some(id) = extract_html_attr(tag_str, "data-publishedfileid") {
            let trimmed = id.trim();
            if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
                return Some(trimmed.to_string());
            }
        }
        if let Some(href) = extract_html_attr(tag_str, "href") {
            let decoded = decode_html_entities(&href);
            if let Some(id) = parse_workshop_id(&decoded) {
                return Some(id);
            }
        }
        pos = tag_end + 1;
    }

    if let Some(idx) = find_ascii_ci(card_html, "id=") {
        let digits: String = card_html[idx + 3..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !digits.is_empty() {
            return Some(digits);
        }
    }

    None
}

fn extract_div_text_by_class(html: &str, class_token: &str) -> Option<String> {
    let mut pos = 0usize;
    while let Some((div_start, tag_end, tag_str)) = find_next_open_tag(html, pos, "div") {
        let matches = extract_html_attr(tag_str, "class")
            .map(|c| has_class_token(&c, class_token))
            .unwrap_or(false);
        if matches {
            let (inner, _) = extract_balanced_div_inner(html, div_start)?;
            let stripped = strip_html_tags(inner);
            let decoded = decode_html_entities(&stripped);
            let collapsed = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
            return Some(collapsed);
        }
        pos = tag_end + 1;
    }
    None
}

fn extract_browse_card_author(card_html: &str) -> Option<String> {
    let mut pos = 0usize;
    while let Some((div_start, tag_end, tag_str)) = find_next_open_tag(card_html, pos, "div") {
        let matches = extract_html_attr(tag_str, "class")
            .map(|c| has_class_token(&c, "workshopItemAuthorName"))
            .unwrap_or(false);
        if !matches {
            pos = tag_end + 1;
            continue;
        }
        let (inner, _) = extract_balanced_div_inner(card_html, div_start)?;

        let mut link_authors = Vec::new();
        let mut a_pos = 0usize;
        while let Some((_, a_end, _)) = find_next_open_tag(inner, a_pos, "a") {
            let content_start = a_end + 1;
            if let Some(rel_close) = find_ascii_ci(&inner[content_start..], "</a>") {
                let raw_link = &inner[content_start..content_start + rel_close];
                let cleaned = decode_html_entities(&strip_html_tags(raw_link))
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if !cleaned.is_empty() {
                    link_authors.push(cleaned);
                }
                a_pos = content_start + rel_close + 4;
            } else {
                break;
            }
        }
        if !link_authors.is_empty() {
            return Some(link_authors.join(", "));
        }

        let stripped = decode_html_entities(&strip_html_tags(inner));
        let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
        let trimmed = collapsed
            .strip_prefix("by ")
            .or_else(|| collapsed.strip_prefix("By "))
            .unwrap_or(&collapsed)
            .trim()
            .to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
        return None;
    }
    None
}

fn extract_browse_card_preview_url(card_html: &str) -> Option<String> {
    let mut pos = 0usize;
    while let Some((_, tag_end, tag_str)) = find_next_open_tag(card_html, pos, "img") {
        let is_preview = extract_html_attr(tag_str, "class")
            .map(|c| has_class_token(&c, "workshopItemPreviewImage"))
            .unwrap_or(false);
        if is_preview {
            if let Some(src) = extract_html_attr(tag_str, "src") {
                let decoded = decode_html_entities(src.trim());
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
        }
        pos = tag_end + 1;
    }

    let mut div_pos = 0usize;
    while let Some((div_start, tag_end, tag_str)) = find_next_open_tag(card_html, div_pos, "div") {
        let is_holder = extract_html_attr(tag_str, "class")
            .map(|c| has_class_token(&c, "workshopItemPreviewHolder"))
            .unwrap_or(false);
        if is_holder {
            let (holder_inner, _) = extract_balanced_div_inner(card_html, div_start)?;
            if let Some((_, _, img_tag)) = find_next_open_tag(holder_inner, 0, "img") {
                if let Some(src) = extract_html_attr(img_tag, "src") {
                    let decoded = decode_html_entities(src.trim());
                    if !decoded.is_empty() {
                        return Some(decoded);
                    }
                }
            }
            break;
        }
        div_pos = tag_end + 1;
    }

    None
}

fn detect_has_next_page(html: &str, page: u32) -> bool {
    let next_marker = format!("p={}", page + 1);
    let mut pos = 0usize;

    while let Some((_, tag_end, tag_str)) = find_next_open_tag(html, pos, "a") {
        let classes = extract_html_attr(tag_str, "class").unwrap_or_default();
        let is_disabled = has_class_token(&classes, "disabled");

        if !is_disabled {
            if let Some(href) = extract_html_attr(tag_str, "href") {
                let decoded_href = decode_html_entities(&href).replace("%3D", "=").replace("%3d", "=");
                if href_has_query_param(&decoded_href, &next_marker) {
                    return true;
                }
            }

            if has_class_token(&classes, "pagebtn") {
                let content_start = tag_end + 1;
                if let Some(rel_close) = find_ascii_ci(&html[content_start..], "</a>") {
                    let text = decode_html_entities(&strip_html_tags(
                        &html[content_start..content_start + rel_close],
                    ))
                    .trim()
                    .to_string();
                    if matches!(text.as_str(), ">" | "»") || text.eq_ignore_ascii_case("next") {
                        return true;
                    }
                }
            }
        }
        pos = tag_end + 1;
    }

    false
}

fn href_has_query_param(href: &str, marker: &str) -> bool {
    let mut start = 0usize;
    while let Some(rel) = href[start..].find(marker) {
        let idx = start + rel;
        let before_ok = idx == 0 || matches!(href.as_bytes()[idx - 1], b'?' | b'&');
        let after_idx = idx + marker.len();
        let after_ok = after_idx == href.len()
            || matches!(
                href.as_bytes()[after_idx],
                b'&' | b'#' | b' ' | b'\t'
            );
        if before_ok && after_ok {
            return true;
        }
        start = after_idx;
    }
    false
}

enum BbcodeAction {
    RemoveBlockUntil(&'static str),
    ListBullet,
    BlockBoundary,
    CellSeparator,
    StripTag,
}

fn parse_bbcode_bracket(text: &str, open_idx: usize) -> Option<(usize, &str)> {
    let bytes = text.as_bytes();
    let mut j = open_idx + 1;
    while j < bytes.len() {
        let b = bytes[j];
        if b == b']' {
            let inner = &text[open_idx + 1..j];
            return (!inner.is_empty()).then_some((j, inner));
        }
        if b == b'\n' || b == b'[' {
            return None;
        }
        j += 1;
    }
    None
}

fn classify_bbcode_tag(inner: &str) -> Option<BbcodeAction> {
    let trimmed = inner.trim();
    if trimmed == "*" {
        return Some(BbcodeAction::ListBullet);
    }
    if trimmed == "/*" {
        return Some(BbcodeAction::StripTag);
    }

    let is_closing = trimmed.starts_with('/');
    let body = trimmed.strip_prefix('/').unwrap_or(trimmed);

    if body.contains(char::is_whitespace) && !body.contains('=') {
        return None;
    }

    let name_raw = body
        .split(['=', ' ', '\t'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    match name_raw.as_str() {
        "img" if !is_closing => Some(BbcodeAction::RemoveBlockUntil("img")),
        "previewyoutube" if !is_closing => Some(BbcodeAction::RemoveBlockUntil("previewyoutube")),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "list" | "olist" | "quote" | "code"
        | "table" | "tr" | "hr" => Some(BbcodeAction::BlockBoundary),
        "td" | "th" if is_closing => Some(BbcodeAction::CellSeparator),
        "b" | "i" | "u" | "s" | "strike" | "url" | "img" | "previewyoutube" | "spoiler"
        | "noparse" | "td" | "th" | "color" | "size" | "font" | "align" | "center" | "left"
        | "right" | "steamlink" | "dynamiclink" => Some(BbcodeAction::StripTag),
        _ => None,
    }
}

fn find_closing_bbcode_tag(text: &str, from: usize, tag_name: &str) -> Option<usize> {
    let close_pattern = format!("[/{tag_name}]");
    find_ascii_ci(&text[from..], &close_pattern).map(|rel| from + rel + close_pattern.len())
}

fn normalize_cleaned_text(raw: &str) -> String {
    let mut lines_out = Vec::new();
    let mut blank_streak = 0usize;

    for raw_line in raw.lines() {
        let mut line = String::with_capacity(raw_line.len());
        let mut prev_space = false;
        for ch in raw_line.trim().chars() {
            if ch == ' ' || ch == '\t' {
                if !prev_space {
                    line.push(' ');
                    prev_space = true;
                }
            } else {
                line.push(ch);
                prev_space = false;
            }
        }

        if line.is_empty() {
            if !lines_out.is_empty() && blank_streak == 0 {
                lines_out.push(String::new());
                blank_streak = 1;
            }
        } else {
            lines_out.push(line);
            blank_streak = 0;
        }
    }

    while lines_out.last().map(String::is_empty).unwrap_or(false) {
        lines_out.pop();
    }

    lines_out.join("\n")
}

fn error_cause_summary(err: &reqwest::Error) -> String {
    let mut causes = Vec::new();
    let mut current: Option<&(dyn StdError + 'static)> = err.source();
    while let Some(src) = current {
        causes.push(src.to_string());
        current = src.source();
    }

    let combined_lower = causes
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" | ");

    if err.is_timeout()
        || combined_lower.contains("timed out")
        || combined_lower.contains("timeout")
        || combined_lower.contains("deadline has elapsed")
        || combined_lower.contains("os error 10060")
    {
        return "quá thời gian chờ kết nối (timed out)".to_string();
    }

    if combined_lower.contains("connection reset")
        || combined_lower.contains("forcibly closed")
        || combined_lower.contains("os error 10054")
        || combined_lower.contains("broken pipe")
        || combined_lower.contains("unexpected eof")
    {
        return "kết nối bị ngắt đột ngột (connection reset)".to_string();
    }

    if combined_lower.contains("connection refused")
        || combined_lower.contains("actively refused")
        || combined_lower.contains("os error 10061")
    {
        return "bị từ chối kết nối (connection refused)".to_string();
    }

    if combined_lower.contains("lookup address")
        || combined_lower.contains("no such host")
        || combined_lower.contains("failed to resolve")
        || combined_lower.contains("name or service not known")
        || combined_lower.contains("dns")
        || combined_lower.contains("os error 11001")
        || combined_lower.contains("os error 11004")
    {
        return "không phân giải được tên miền (DNS)".to_string();
    }

    if combined_lower.contains("tls")
        || combined_lower.contains("ssl")
        || combined_lower.contains("handshake")
        || combined_lower.contains("certificate")
        || combined_lower.contains("rustls")
        || combined_lower.contains("received fatal alert")
        || combined_lower.contains("eof without close_notify")
    {
        return "lỗi bắt tay bảo mật TLS".to_string();
    }

    if combined_lower.contains("unreachable")
        || combined_lower.contains("os error 10051")
        || combined_lower.contains("os error 10065")
    {
        return "không thể định tuyến tới máy chủ".to_string();
    }

    if combined_lower.contains("proxy")
        || combined_lower.contains("socks")
        || combined_lower.contains("tunnel")
    {
        return "lỗi kết nối qua Proxy".to_string();
    }

    if err.is_decode() {
        return "dữ liệu phản hồi không hợp lệ".to_string();
    }

    if err.is_connect() {
        return "không thể thiết lập kết nối mạng".to_string();
    }

    if err.is_body() {
        return "lỗi truyền tải dữ liệu phản hồi".to_string();
    }

    if let Some(deepest) = causes
        .iter()
        .rev()
        .map(|s| strip_url_from_error_text(s))
        .find(|s| !s.is_empty())
    {
        return deepest;
    }

    "lỗi yêu cầu mạng".to_string()
}

fn strip_url_from_error_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.contains("http://") || trimmed.contains("https://") {
        return String::new();
    }
    trimmed.to_string()
}

fn parse_warp_dns_only(status_out: &str, settings_out: &str) -> bool {
    let connected = status_out.contains("Connected") && !status_out.contains("Disconnected");
    let dns_only =
        settings_out.contains("Mode: DnsOverTls") || settings_out.contains("Mode: DnsOverHttps");
    connected && dns_only
}

#[cfg(windows)]
async fn run_warp_readonly_cmd(warp_cli: &Path, subcommand: &str) -> Option<String> {
    let mut cmd = Command::new(warp_cli);
    cmd.args(["--accept-tos", subcommand])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .creation_flags(CREATE_NO_WINDOW);

    let output = tokio::time::timeout(Duration::from_secs(2), cmd.output())
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(windows)]
async fn is_cloudflare_warp_dns_only() -> bool {
    let warp_cli = Path::new(r"C:\Program Files\Cloudflare\Cloudflare WARP\warp-cli.exe");
    if !warp_cli.is_file() {
        return false;
    }
    let Some(status_out) = run_warp_readonly_cmd(warp_cli, "status").await else {
        return false;
    };
    if !status_out.contains("Connected") || status_out.contains("Disconnected") {
        return false;
    }
    let Some(settings_out) = run_warp_readonly_cmd(warp_cli, "settings").await else {
        return false;
    };
    parse_warp_dns_only(&status_out, &settings_out)
}

#[cfg(not(windows))]
async fn is_cloudflare_warp_dns_only() -> bool {
    false
}

async fn classify_steam_network_error(
    service_label: &str,
    err: &reqwest::Error,
    proxy_url: Option<&str>,
) -> LauncherError {
    let summary = error_cause_summary(err);

    if let Some(proxy) = proxy_url.and_then(normalize_proxy_url) {
        return LauncherError::Network(format!(
            "Không thể kết nối {service_label} qua Proxy ({proxy}): {summary}. Kiểm tra lại phần mềm Proxy/VPN đang chạy."
        ));
    }

    if (err.is_connect() || err.is_timeout() || err.is_request())
        && is_cloudflare_warp_dns_only().await
    {
        return LauncherError::Network(format!(
            "Không thể kết nối {service_label} ({summary}). Cloudflare WARP trên máy đang chạy ở chế độ chỉ DNS (1.1.1.1), chưa bật đường hầm WARP — hãy chuyển sang chế độ '1.1.1.1 with WARP', bật VPN hoặc cấu hình Proxy ở tab Cài đặt."
        ));
    }

    LauncherError::Network(format!(
        "Không thể kết nối {service_label} ({summary}). Có thể nhà mạng đang chặn Steam — hãy bật Cloudflare WARP (chế độ WARP), VPN hoặc nhập Proxy ở tab Cài đặt."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_paths(label: &str) -> LauncherPaths {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base_dir = std::env::temp_dir().join(format!(
            "tl_runtime_test_{label}_{}_{}",
            std::process::id(),
            nanos
        ));
        let data_dir = base_dir.join("data");
        LauncherPaths {
            terraria_dir: base_dir.join("Terraria"),
            tmodloader_dir: base_dir.join("tModLoader"),
            vanilla_saves_dir: data_dir.join("Saves").join("Vanilla"),
            tml_saves_dir: data_dir.join("Saves").join("tModLoader"),
            servers_dir: data_dir.join("Servers"),
            backups_dir: data_dir.join("Backups"),
            steamcmd_dir: data_dir.join("SteamCMD"),
            config_path: data_dir.join("config.json"),
            data_dir,
            base_dir,
        }
    }

    fn write_dotnet_str(buf: &mut Vec<u8>, s: &str) {
        let mut len = s.len();
        loop {
            let mut byte = (len & 0x7F) as u8;
            len >>= 7;
            if len != 0 {
                byte |= 0x80;
            }
            buf.push(byte);
            if len == 0 {
                break;
            }
        }
        buf.extend_from_slice(s.as_bytes());
    }

    fn write_minimal_tmod(path: &Path, internal_name: &str, display_name: &str, version: &str) {
        write_minimal_tmod_with_refs(path, internal_name, display_name, version, &[]);
    }

    fn write_minimal_tmod_with_refs(
        path: &Path,
        internal_name: &str,
        display_name: &str,
        version: &str,
        mod_references: &[&str],
    ) {
        write_minimal_tmod_full(
            path,
            internal_name,
            display_name,
            version,
            mod_references,
            [0x11u8; 20],
        );
    }

    fn write_minimal_tmod_full(
        path: &Path,
        internal_name: &str,
        display_name: &str,
        version: &str,
        mod_references: &[&str],
        sha1_bytes: [u8; 20],
    ) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut info = Vec::new();
        write_dotnet_str(&mut info, "displayName");
        write_dotnet_str(&mut info, display_name);
        write_dotnet_str(&mut info, "author");
        write_dotnet_str(&mut info, "TestAuthor");
        if !mod_references.is_empty() {
            write_dotnet_str(&mut info, "modReferences");
            for r in mod_references {
                write_dotnet_str(&mut info, r);
            }
            write_dotnet_str(&mut info, "");
        }
        write_dotnet_str(&mut info, "");

        let mut tmod = Vec::new();
        tmod.extend_from_slice(b"TMOD");
        write_dotnet_str(&mut tmod, "2026.9.3.1");
        tmod.extend_from_slice(&sha1_bytes);
        tmod.extend_from_slice(&[0u8; 256]);
        tmod.extend_from_slice(&128i32.to_le_bytes());
        write_dotnet_str(&mut tmod, internal_name);
        write_dotnet_str(&mut tmod, version);
        tmod.extend_from_slice(&1i32.to_le_bytes());
        write_dotnet_str(&mut tmod, "Info");
        tmod.extend_from_slice(&(info.len() as i32).to_le_bytes());
        tmod.extend_from_slice(&(info.len() as i32).to_le_bytes());
        tmod.extend_from_slice(&info);

        fs::write(path, &tmod).unwrap();
    }

    #[test]
    fn parse_workshop_browse_html_extracts_items_entities_and_pagination() {
        let html = r#"
        <div class="workshopBrowseItems">
            <div class="workshopItem">
                <a href="https://steamcommunity.com/sharedfiles/filedetails/?id=2824688072&amp;searchtext=" class="ugc" data-publishedfileid="2824688072">
                    <div class="workshopItemPreviewHolder aspectratio_16x9">
                        <img class="workshopItemPreviewImage " src="https://steamuserimages-a.akamaihd.net/ugc/111/?imw=200&amp;imh=200" />
                    </div>
                </a>
                <a href="https://steamcommunity.com/sharedfiles/filedetails/?id=2824688072">
                    <div class="workshopItemTitle ellipsis">Calamity &amp; Addons &#39;Redux&#39;</div>
                </a>
                <div class="workshopItemAuthorName ellipsis">by <a href="https://steamcommunity.com/id/fabsol">Fabsol &amp; Co&#x27;s</a></div>
                <img class="fileRating" src="https://example.com/5star.png" />
            </div>
            <div class="workshopItem">
                <a href="https://steamcommunity.com/sharedfiles/filedetails/?id=2563309347">
                    <div class="workshopItemPreviewHolder">
                        <img src="https://steamuserimages-a.akamaihd.net/ugc/222/thumb.jpg?a=1&amp;b=2" class="workshopItemPreviewImage" />
                    </div>
                </a>
                <a href="https://steamcommunity.com/sharedfiles/filedetails/?id=2563309347">
                    <div class="workshopItemTitle">Magic &lt;Storage&gt; &quot;Pro&quot;&nbsp;&#8212; v2</div>
                </a>
                <div class="workshopItemAuthorName">by blushiemagic &#x2014; Team</div>
            </div>
        </div>
        <div class="workshopBrowsePagingControls">
            <span class="pagebtn disabled">&lt;</span>
            <a class="pagelink" href="https://steamcommunity.com/workshop/browse/?appid=1281930&amp;p=2">2</a>
            <a class="pagebtn" href="https://steamcommunity.com/workshop/browse/?appid=1281930&amp;p=2">&gt;</a>
        </div>
        "#;

        let res = parse_workshop_browse_html(html, 1);
        assert_eq!(res.page, 1);
        assert!(res.has_next_page);
        assert_eq!(res.items.len(), 2);

        assert_eq!(res.items[0].published_file_id, "2824688072");
        assert_eq!(res.items[0].title, "Calamity & Addons 'Redux'");
        assert_eq!(res.items[0].author, "Fabsol & Co's");
        assert_eq!(
            res.items[0].preview_url,
            "https://steamuserimages-a.akamaihd.net/ugc/111/?imw=200&imh=200"
        );

        assert_eq!(res.items[1].published_file_id, "2563309347");
        assert_eq!(res.items[1].title, "Magic <Storage> \"Pro\" — v2");
        assert_eq!(res.items[1].author, "blushiemagic — Team");
        assert_eq!(
            res.items[1].preview_url,
            "https://steamuserimages-a.akamaihd.net/ugc/222/thumb.jpg?a=1&b=2"
        );

        let last_page_html = r#"
        <div class="workshopItem">
            <a href="https://steamcommunity.com/sharedfiles/filedetails/?id=999">
                <div class="workshopItemTitle">Final Mod</div>
            </a>
        </div>
        <div class="workshopBrowsePagingControls">
            <a class="pagebtn" href="https://steamcommunity.com/workshop/browse/?appid=1281930&amp;p=1">&lt;</a>
            <a class="pagelink" href="https://steamcommunity.com/workshop/browse/?appid=1281930&amp;p=20">20</a>
            <span class="pagebtn disabled">&gt;</span>
        </div>
        "#;
        let last = parse_workshop_browse_html(last_page_html, 2);
        assert_eq!(last.page, 2);
        assert!(!last.has_next_page);
        assert_eq!(last.items.len(), 1);
    }

    #[test]
    fn strip_steam_bbcode_cleans_tags_and_preserves_literal_brackets() {
        let raw = concat!(
            "[1.4.4] [b]Awesome Mod[/b] [v2.0]\r\n\r\n\r\n",
            "[h1]Features [WIP][/h1]",
            "[list]",
            "[*] First [i]italic[/i] feature",
            "[*] Second [u]underlined[/u] & [strike]old[/strike] feature",
            "[/list]",
            "[img]https://cdn.example.com/banner.png[/img]\n",
            "[code]let x = arr[0];[/code]\n",
            "[quote=Author]Play [MP Compatible]![/quote]\n",
            "[spoiler]Hidden[/spoiler] ",
            "See [url=https://example.com/docs]Documentation[/url] or [url]https://example.com[/url]",
        );

        let cleaned = strip_steam_bbcode(raw);
        let expected = concat!(
            "[1.4.4] Awesome Mod [v2.0]\n\n",
            "Features [WIP]\n",
            "• First italic feature\n",
            "• Second underlined & old feature\n",
            "let x = arr[0];\n",
            "Play [MP Compatible]!\n",
            "Hidden See Documentation or https://example.com",
        );
        assert_eq!(cleaned, expected);
    }

    #[test]
    fn pick_best_workshop_tmod_uses_numeric_version_tuples() {
        assert_eq!(parse_workshop_version_tuple("2026.10"), Some((2026, 10, 0, 0)));
        assert_eq!(parse_workshop_version_tuple("2026.09"), Some((2026, 9, 0, 0)));
        assert_eq!(parse_workshop_version_tuple("2026.2"), Some((2026, 2, 0, 0)));
        assert_eq!(parse_workshop_version_tuple("1.4.4.9"), Some((1, 4, 4, 9)));
        assert_eq!(parse_workshop_version_tuple("CalamityMod"), None);
        assert!(parse_workshop_version_tuple("2026.10") > parse_workshop_version_tuple("2026.2"));

        let candidates_without_2026_9 = vec![
            PathBuf::from(r"C:\staging\1281930\2824688072\2026.2\CalamityMod.tmod"),
            PathBuf::from(r"C:\staging\1281930\2824688072\2026.10\CalamityMod.tmod"),
            PathBuf::from(r"C:\staging\1281930\2824688072\2025.12\CalamityMod.tmod"),
        ];
        let picked = pick_best_workshop_tmod(&candidates_without_2026_9).unwrap();
        assert_eq!(
            picked,
            PathBuf::from(r"C:\staging\1281930\2824688072\2026.2\CalamityMod.tmod")
        );

        let candidates_with_2026_9 = vec![
            PathBuf::from(r"C:\staging\1281930\2824688072\2026.2\CalamityMod.tmod"),
            PathBuf::from(r"C:\staging\1281930\2824688072\2026.9\CalamityMod.tmod"),
            PathBuf::from(r"C:\staging\1281930\2824688072\2026.10\CalamityMod.tmod"),
        ];
        let picked_2026_9 = pick_best_workshop_tmod(&candidates_with_2026_9).unwrap();
        assert_eq!(
            picked_2026_9,
            PathBuf::from(r"C:\staging\1281930\2824688072\2026.9\CalamityMod.tmod")
        );

        let root_vs_legacy_and_future = vec![
            PathBuf::from(r"C:\staging\1281930\2824688072\1.4.4.9\CalamityMod.tmod"),
            PathBuf::from(r"C:\staging\1281930\2824688072\CalamityMod.tmod"),
            PathBuf::from(r"C:\staging\1281930\2824688072\2026.11\CalamityMod.tmod"),
            PathBuf::from(r"C:\staging\1281930\2824688072\2026.10\CalamityMod.tmod"),
        ];
        assert_eq!(
            pick_best_workshop_tmod(&root_vs_legacy_and_future).unwrap(),
            PathBuf::from(r"C:\staging\1281930\2824688072\CalamityMod.tmod")
        );

        let only_future_and_legacy = vec![
            PathBuf::from(r"C:\staging\1281930\2824688072\1.4.4.9\CalamityMod.tmod"),
            PathBuf::from(r"C:\staging\1281930\2824688072\2026.11\CalamityMod.tmod"),
            PathBuf::from(r"C:\staging\1281930\2824688072\2026.10\CalamityMod.tmod"),
        ];
        assert_eq!(
            pick_best_workshop_tmod(&only_future_and_legacy).unwrap(),
            PathBuf::from(r"C:\staging\1281930\2824688072\2026.10\CalamityMod.tmod")
        );

        assert_eq!(pick_best_workshop_tmod(&[]), None);
    }

    #[test]
    fn workshop_manifest_roundtrip_and_staging_scan() {
        let paths = make_temp_paths("manifest");
        paths.ensure_dirs().unwrap();

        let staged_old = paths
            .steamcmd_dir
            .join("staging/steamapps/workshop/content/1281930/2824688072/2026.2/CalamityMod.tmod");
        let staged_new = paths
            .steamcmd_dir
            .join("staging/steamapps/workshop/content/1281930/2824688072/2026.9/CalamityMod.tmod");
        write_minimal_tmod(&staged_old, "CalamityMod", "Calamity Mod", "2.0.4");
        write_minimal_tmod(&staged_new, "CalamityMod", "Calamity Mod", "2.0.5");

        let meta_v1 = parse_tmod_file(&staged_new).unwrap();
        let details = WorkshopItemDetails {
            published_file_id: "2824688072".to_string(),
            title: "Calamity Mod [Workshop]".to_string(),
            author: "Fabsol".to_string(),
            preview_url: "https://example.com/calamity.png".to_string(),
            time_updated: 1_760_000_000,
            ..Default::default()
        };

        record_workshop_install(&paths, Some(&details), "2824688072", &[meta_v1]).unwrap();
        let loaded = load_workshop_manifest(&paths);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].published_file_id, "2824688072");
        assert_eq!(loaded[0].internal_name, "CalamityMod");
        assert_eq!(loaded[0].mod_version, "2.0.5");
        assert_eq!(loaded[0].workshop_title, "Calamity Mod [Workshop]");
        assert_eq!(loaded[0].author, "Fabsol");
        assert_eq!(loaded[0].preview_url, "https://example.com/calamity.png");
        assert_eq!(loaded[0].time_updated, 1_760_000_000);

        let mut meta_v2 = parse_tmod_file(&staged_new).unwrap();
        meta_v2.version = "2.0.6".to_string();
        record_workshop_install(&paths, None, "2824688072", &[meta_v2]).unwrap();
        let reloaded = load_workshop_manifest(&paths);
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].mod_version, "2.0.6");
        assert_eq!(reloaded[0].workshop_title, "Calamity Mod [Workshop]");

        import_local_mod_files(std::slice::from_ref(&staged_new), &paths, true).unwrap();
        let staged = scan_staged_workshop_mods(&paths).unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].workshop_id, "2824688072");
        assert_eq!(staged[0].meta.version, "2.0.5");
        assert!(staged[0].installed);
        assert!(staged[0].enabled);
        assert_eq!(
            staged[0].workshop_title.as_deref(),
            Some("Calamity Mod [Workshop]")
        );
        assert_eq!(staged[0].author.as_deref(), Some("Fabsol"));

        delete_staged_workshop_mod(&paths, "2824688072").unwrap();
        assert!(scan_staged_workshop_mods(&paths).unwrap().is_empty());

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn sort_and_timestamp_helpers_match_spec() {
        assert_eq!(WorkshopSort::default(), WorkshopSort::Trending);
        assert_eq!(WorkshopSort::Trending.label(), "Thịnh hành");
        assert_eq!(WorkshopSort::Trending.steam_param(), "trend");
        assert_eq!(
            WorkshopSort::MostSubscribed.steam_param(),
            "totaluniquesubscribers"
        );
        assert_eq!(WorkshopSort::TopRated.label(), "Đánh giá cao");
        assert_eq!(WorkshopSort::MostRecent.steam_param(), "mostrecent");

        assert_eq!(format_unix_timestamp(0), "");
        assert_eq!(format_unix_timestamp(1_700_000_000), "14/11/2023");
    }

    #[test]
    fn api_details_and_filedetails_author_extraction_work() {
        let payload = serde_json::json!({
            "publishedfileid": "2824688072",
            "result": 1,
            "consumer_app_id": 1281930,
            "file_size": "52428800",
            "time_updated": 1760000000u64,
            "subscriptions": 123456,
            "favorited": 7890,
            "views": 999999,
            "preview_url": "https://images.steamusercontent.com/ugc/preview.jpg",
            "file_description": "[h1]Title[/h1][b]Body[/b]",
            "tags": [
                { "tag": "QoL", "display_name": "Quality of Life" },
                { "tag": "Content" }
            ],
            "children": [
                { "publishedfileid": "111222333" },
                { "publishedfileid": 444555666u64 }
            ]
        });

        let mut item = WorkshopItemDetails {
            published_file_id: "2824688072".to_string(),
            title: "Keep Browse Title".to_string(),
            author: "Keep Browse Author".to_string(),
            ..Default::default()
        };
        apply_api_details(&mut item, &payload, false);
        assert_eq!(item.title, "Keep Browse Title");
        assert_eq!(item.author, "Keep Browse Author");

        let mut empty_title_item = WorkshopItemDetails {
            published_file_id: "2824688072".to_string(),
            ..Default::default()
        };
        apply_api_details(
            &mut empty_title_item,
            &serde_json::json!({ "title": "  Calamity Mod  " }),
            false,
        );
        assert_eq!(empty_title_item.title, "Calamity Mod");
        assert_eq!(item.file_size, 52_428_800);
        assert_eq!(item.time_updated, 1_760_000_000);
        assert_eq!(item.subscriptions, 123_456);
        assert_eq!(item.favorited, 7_890);
        assert_eq!(item.views, 999_999);
        assert_eq!(item.consumer_app_id, 1_281_930);
        assert_eq!(
            item.preview_url,
            "https://images.steamusercontent.com/ugc/preview.jpg"
        );
        assert_eq!(item.description, "Title\nBody");
        assert_eq!(item.tags, vec!["Quality of Life", "Content"]);
        assert_eq!(item.child_ids, vec!["111222333", "444555666"]);

        let filedetails_html = r#"
        <div class="rightDetailsBlock">
            <div class="creatorsBlock">
                <div class="friendBlock persona offline">
                    <a class="friendBlockLinkOverlay" href="https://steamcommunity.com/id/author"></a>
                    <div class="playerAvatar offline"><img src="avatar.jpg" /></div>
                    <div class="friendBlockContent">
                        Fabsol &amp; Team<br>
                        <span class="friendSmallText">Offline</span>
                    </div>
                </div>
            </div>
        </div>
        "#;
        assert_eq!(
            extract_filedetails_author(filedetails_html).as_deref(),
            Some("Fabsol & Team")
        );

        let paths = make_temp_paths("validation");
        assert!(delete_staged_workshop_mod(&paths, "../bad").is_err());
        assert!(delete_staged_workshop_mod(&paths, "").is_err());
    }

    #[test]
    fn warp_dns_only_detection_parses_cli_output() {
        assert!(parse_warp_dns_only(
            "Status update: Connected",
            "Mode: DnsOverTls"
        ));
        assert!(parse_warp_dns_only(
            "Status update: Connected\nSuccess",
            "Always On: true\nMode: DnsOverHttps"
        ));
        assert!(!parse_warp_dns_only(
            "Status update: Connected",
            "Mode: Warp"
        ));
        assert!(!parse_warp_dns_only(
            "Status update: Disconnected",
            "Mode: DnsOverHttps"
        ));
    }

    #[test]
    fn network_error_classifier_summarizes_causes_without_exposing_urls() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let closed_port = listener.local_addr().unwrap().port();
            drop(listener);

            let client = build_http_client(None).unwrap();
            let secret_url = format!("http://127.0.0.1:{closed_port}/secret/workshop_endpoint");
            let refused_err = client.get(&secret_url).send().await.unwrap_err();
            let refused_summary = error_cause_summary(&refused_err);
            assert!(
                refused_summary.contains("refused") || refused_summary.contains("kết nối"),
                "unexpected summary: {refused_summary}"
            );
            assert!(!refused_summary.contains("127.0.0.1"));
            assert!(!refused_summary.contains("secret"));
            assert!(!refused_summary.contains("http://"));

            let proxy_classified =
                classify_steam_network_error("Steam API", &refused_err, Some("127.0.0.1:7890"))
                    .await;
            let proxy_msg = proxy_classified.to_string();
            assert!(proxy_msg.contains("Steam API"));
            assert!(proxy_msg.contains("http://127.0.0.1:7890"));
            assert!(!proxy_msg.contains("secret"));

            let direct_classified =
                classify_steam_network_error("Steam Workshop", &refused_err, None).await;
            let direct_msg = direct_classified.to_string();
            assert!(direct_msg.contains("Steam Workshop"));
            assert!(!direct_msg.contains("secret"));

            let hold_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let slow_port = hold_listener.local_addr().unwrap().port();
            let accept_task = tokio::spawn(async move {
                let Ok((_stream, _)) = hold_listener.accept().await else {
                    return;
                };
                tokio::time::sleep(Duration::from_millis(250)).await;
            });

            let timeout_client = reqwest::Client::builder()
                .timeout(Duration::from_millis(40))
                .build()
                .unwrap();
            let timeout_err = timeout_client
                .get(format!("http://127.0.0.1:{slow_port}/slow"))
                .send()
                .await
                .unwrap_err();
            let timeout_summary = error_cause_summary(&timeout_err);
            assert!(
                timeout_summary.contains("timed out") || timeout_summary.contains("thời gian"),
                "unexpected timeout summary: {timeout_summary}"
            );
            assert!(!timeout_summary.contains("http://"));
            let _ = accept_task.await;
        });
    }

    #[test]
    fn parse_workshop_browse_html_supports_valve_ssr_json_and_pagination() {
        let ssr_query_data = serde_json::json!({
            "queries": [
                {
                    "state": { "data": { "foo": "bar" } },
                    "queryKey": ["app_admin_access", 1281930]
                },
                {
                    "state": {
                        "data": {
                            "eresult": 1,
                            "current_page": 1,
                            "total_pages": 57,
                            "total_count": 1694,
                            "results": [
                                {
                                    "publishedfileid": "2824688072",
                                    "creator": "76561198010743466",
                                    "consumer_appid": 1281930,
                                    "preview_url": "https://images.steamusercontent.com/ugc/calamity/",
                                    "title": "Calamity Mod",
                                    "short_description": "[b]Content mod[/b] for tModLoader",
                                    "file_size": "12345678",
                                    "time_updated": 1794000000u64,
                                    "subscriptions": 5000000u64,
                                    "favorited": 120000u64,
                                    "views": 9000000u64,
                                    "tags": [
                                        { "tag": "New Content", "display_name": "New Content" },
                                        { "tag": "1.4.4", "display_name": "1.4.4" }
                                    ]
                                },
                                {
                                    "publishedfileid": "3835851637",
                                    "creator": "76561199999999999",
                                    "consumer_appid": 1281930,
                                    "preview_url": "https://images.steamusercontent.com/ugc/puddles/",
                                    "title": "Fake Puddles!",
                                    "short_description": "Rain puddles"
                                }
                            ],
                            "creator_player_link_details": [
                                {
                                    "public_data": {
                                        "steamid": "76561198010743466",
                                        "persona_name": "Fabsol"
                                    }
                                }
                            ]
                        }
                    }
                }
            ]
        });

        let payload = serde_json::json!({
            "route": "workshop/browse",
            "renderContext": {
                "queryData": ssr_query_data.to_string()
            }
        });

        let html = format!(
            r#"<!DOCTYPE html><html><head>
            <script id="valve-ssr-data" type="application/json">{payload}</script>
            </head><body>
            <div>
                <a href="https://steamcommunity.com/sharedfiles/filedetails/?id=3835851637"><img src="x" alt="Fake Puddles!"/></a>
                <a href="https://steamcommunity.com/sharedfiles/filedetails/?id=3835851637">Fake Puddles!</a>
                <a href="https://steamcommunity.com/id/levitatingGT/myworkshopfiles/?appid=1281930">By LevitatingGT</a>
            </div>
            </body></html>"#
        );

        let res = parse_workshop_browse_html(&html, 1);
        assert_eq!(res.page, 1);
        assert!(res.has_next_page);
        assert_eq!(res.items.len(), 2);
        assert_eq!(res.items[0].published_file_id, "2824688072");
        assert_eq!(res.items[0].title, "Calamity Mod");
        assert_eq!(res.items[0].author, "Fabsol");
        assert_eq!(res.items[0].description, "Content mod for tModLoader");
        assert_eq!(res.items[0].file_size, 12345678);
        assert_eq!(res.items[0].tags, vec!["New Content", "1.4.4"]);

        assert_eq!(res.items[1].published_file_id, "3835851637");
        assert_eq!(res.items[1].author, "LevitatingGT");

        let empty_ssr = serde_json::json!({
            "renderContext": {
                "queryData": {
                    "queries": [{
                        "state": {
                            "data": {
                                "current_page": 2,
                                "total_pages": 2,
                                "total_count": 0,
                                "results": []
                            }
                        }
                    }]
                }
            }
        });
        let empty_html = format!(r#"<script id="valve-ssr-data">{empty_ssr}</script>"#);
        let empty_res = parse_workshop_browse_html(&empty_html, 2);
        assert!(empty_res.items.is_empty());
        assert!(!empty_res.has_next_page);
    }

    #[test]
    fn parse_workshop_browse_html_falls_back_to_semantic_ssr_dom_cards() {
        let html = r#"
        <div class="UNowfeldbNg- Panel">
            <div><a href="https://steamcommunity.com/sharedfiles/filedetails/?id=3835851637" class="hash-1"><img src="https://images.steamusercontent.com/ugc/1/puddles.jpg?imw=322&amp;imh=322" alt="Fake Puddles!"/></a></div>
            <div class="Sw3NXcvOA4Y-"><a href="https://steamcommunity.com/sharedfiles/filedetails/?id=3835851637" class="hash-2">Fake &amp; Fun Puddles!</a></div>
            <div class="o14JIlvi52E-"><a href="https://steamcommunity.com/id/levitatingGT/myworkshopfiles/?appid=1281930" class="hash-3">By LevitatingGT</a></div>
        </div>
        <div class="UNowfeldbNg- Panel">
            <div><a href="https://steamcommunity.com/sharedfiles/filedetails/?id=3835720328"><img src="https://images.steamusercontent.com/ugc/2/respawn.jpg" alt="Tactical Respawn"/></a></div>
        </div>
        "#;

        let res = parse_workshop_browse_html(html, 1);
        assert_eq!(res.items.len(), 2);
        assert_eq!(res.items[0].published_file_id, "3835851637");
        assert_eq!(res.items[0].title, "Fake & Fun Puddles!");
        assert_eq!(res.items[0].author, "LevitatingGT");
        assert_eq!(
            res.items[0].preview_url,
            "https://images.steamusercontent.com/ugc/1/puddles.jpg?imw=322&imh=322"
        );
        assert_eq!(res.items[1].published_file_id, "3835720328");
        assert_eq!(res.items[1].title, "Tactical Respawn");
        assert!(!res.has_next_page);
    }

    #[test]
    fn scan_installed_mods_reports_disabled_deps_and_bfs_enables_transitively() {
        let paths = make_temp_paths("deps_bfs");
        paths.ensure_dirs().unwrap();
        let mods_dir = paths.tml_mods_dir();

        write_minimal_tmod_with_refs(
            &mods_dir.join("ModA.tmod"),
            "ModA",
            "Alpha Mod",
            "1.0.0",
            &["ModB@1.0", "UninstalledLib"],
        );
        write_minimal_tmod_with_refs(
            &mods_dir.join("ModB.tmod"),
            "ModB",
            "Beta Lib",
            "1.0.0",
            &["ModC"],
        );
        write_minimal_tmod(&mods_dir.join("ModC.tmod"), "ModC", "Core Util", "1.0.0");

        set_mod_enabled(&paths, "ModA", true).unwrap();

        let entries = scan_installed_mods(&paths).unwrap();
        let alpha = entries.iter().find(|e| e.meta.internal_name == "ModA").unwrap();
        let beta = entries.iter().find(|e| e.meta.internal_name == "ModB").unwrap();
        assert!(alpha.enabled);
        assert_eq!(alpha.missing_deps, vec!["UninstalledLib".to_string()]);
        assert_eq!(alpha.disabled_deps, vec!["ModB".to_string()]);
        assert!(!beta.enabled);
        assert!(beta.disabled_deps.is_empty());

        let newly_enabled = enable_mod_and_installed_deps(&paths, "ModA").unwrap();
        assert_eq!(
            newly_enabled,
            vec!["ModB".to_string(), "ModC".to_string()]
        );

        let after = scan_installed_mods(&paths).unwrap();
        let alpha_after = after.iter().find(|e| e.meta.internal_name == "ModA").unwrap();
        let beta_after = after.iter().find(|e| e.meta.internal_name == "ModB").unwrap();
        let gamma_after = after.iter().find(|e| e.meta.internal_name == "ModC").unwrap();
        assert!(alpha_after.disabled_deps.is_empty());
        assert!(beta_after.enabled);
        assert!(gamma_after.enabled);

        assert!(enable_mod_and_installed_deps(&paths, "ModA")
            .unwrap()
            .is_empty());

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn detect_installed_tml_year_month_parses_targets_deps_and_fallback() {
        let paths = make_temp_paths("tml_ym");
        paths.ensure_dirs().unwrap();
        fs::create_dir_all(&paths.tmodloader_dir).unwrap();

        assert_eq!(detect_installed_tml_year_month(&paths), (2026, 9));

        fs::write(
            paths.tmodloader_dir.join("tModLoader.deps.json"),
            r#"{"runtimeTarget":{"name":".NETCoreApp,Version=v8.0"},"libraries":{"JetBrains.Annotations/2025.2.4":{},"tModLoader/1.4.4.9":{}}}"#,
        )
        .unwrap();
        assert_eq!(detect_installed_tml_year_month(&paths), (2026, 9));

        fs::write(
            paths.tmodloader_dir.join("tMLMod.targets"),
            "<Project><PropertyGroup><DefineConstants>$(DefineConstants);TML_2025_11</DefineConstants></PropertyGroup></Project>",
        )
        .unwrap();
        assert_eq!(detect_installed_tml_year_month(&paths), (2025, 11));

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn pick_best_workshop_tmods_bounds_stop_dir_supports_multi_mods_and_compat_order() {
        let paths = make_temp_paths("picker_multi");
        let item_dir = paths
            .base_dir
            .join("2025.1")
            .join("workshop_item");
        let root_a = item_dir.join("MainMod.tmod");
        let ver_compat_a = item_dir.join("2026.8").join("MainMod.tmod");
        let ver_future_a = item_dir.join("2026.10").join("MainMod.tmod");
        let root_b = item_dir.join("SupportLib.tmod");

        write_minimal_tmod(&root_a, "MainMod", "Main Mod", "1.0.0");
        write_minimal_tmod(&ver_compat_a, "MainMod", "Main Mod", "1.1.0");
        write_minimal_tmod(&ver_future_a, "MainMod", "Main Mod", "1.2.0");
        write_minimal_tmod(&root_b, "SupportLib", "Support Lib", "1.0.0");

        let picked = pick_best_workshop_tmods(
            &[
                root_a.clone(),
                ver_compat_a.clone(),
                ver_future_a.clone(),
                root_b.clone(),
            ],
            &item_dir,
            (2026, 9),
        );
        assert_eq!(picked.len(), 2);
        assert!(picked.contains(&ver_compat_a));
        assert!(picked.contains(&root_b));

        let only_unversioned_and_future =
            pick_best_workshop_tmods(&[root_a.clone(), ver_future_a], &item_dir, (2026, 9));
        assert_eq!(only_unversioned_and_future, vec![root_a]);

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn import_local_mod_files_normalizes_name_removes_duplicates_and_picks_zip() {
        let paths = make_temp_paths("import_norm");
        paths.ensure_dirs().unwrap();
        let stage_dir = paths.base_dir.join("incoming");
        fs::create_dir_all(&stage_dir).unwrap();

        let copy_one = stage_dir.join("CoolMod (1).tmod");
        let copy_two = stage_dir.join("CoolMod - Copy.tmod");
        write_minimal_tmod(&copy_one, "CoolMod", "Cool Mod", "1.0.0");
        write_minimal_tmod(&copy_two, "CoolMod", "Cool Mod", "1.1.0");
        fs::copy(&copy_one, paths.tml_mods_dir().join("CoolMod (1).tmod")).unwrap();

        let imported = import_local_mod_files(&[copy_two], &paths, true).unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].internal_name, "CoolMod");
        assert_eq!(imported[0].version, "1.1.0");
        assert!(paths.tml_mods_dir().join("CoolMod.tmod").is_file());
        assert!(!paths.tml_mods_dir().join("CoolMod (1).tmod").exists());

        let zip_src_old = stage_dir.join("zip_old.tmod");
        let zip_src_match = stage_dir.join("zip_match.tmod");
        let zip_src_future = stage_dir.join("zip_future.tmod");
        let zip_src_helper = stage_dir.join("helper.tmod");
        write_minimal_tmod(&zip_src_old, "ZipMod", "Zip Mod", "1.0.0");
        write_minimal_tmod(&zip_src_match, "ZipMod", "Zip Mod", "1.1.0");
        write_minimal_tmod(&zip_src_future, "ZipMod", "Zip Mod", "1.2.0");
        write_minimal_tmod(&zip_src_helper, "ZipHelper", "Zip Helper", "2.0.0");

        let zip_path = stage_dir.join("bundle.zip");
        {
            let file = File::create(&zip_path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (archive_path, disk_path) in [
                ("2025.6/ZipMod.tmod", &zip_src_old),
                ("2026.9/ZipMod.tmod", &zip_src_match),
                ("2026.11/ZipMod.tmod", &zip_src_future),
                ("ZipHelper.tmod", &zip_src_helper),
            ] {
                writer.start_file(archive_path, opts).unwrap();
                writer.write_all(&fs::read(disk_path).unwrap()).unwrap();
            }
            writer.finish().unwrap();
        }

        let zip_imported = import_local_mod_files(&[zip_path], &paths, true).unwrap();
        assert_eq!(zip_imported.len(), 2);
        let zip_mod = parse_tmod_file(&paths.tml_mods_dir().join("ZipMod.tmod")).unwrap();
        assert_eq!(zip_mod.version, "1.1.0");
        assert!(paths.tml_mods_dir().join("ZipHelper.tmod").is_file());

        let leftover_tmp = fs::read_dir(paths.tml_mods_dir())
            .unwrap()
            .flatten()
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".tl_zip_import_tmp_")
            });
        assert!(!leftover_tmp);

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn detect_installed_tml_year_month_skips_stale_log_older_than_dll() {
        let paths = make_temp_paths("tml_stale_log");
        paths.ensure_dirs().unwrap();
        fs::create_dir_all(paths.tmodloader_dir.join("tModLoader-Logs")).unwrap();

        let log_path = paths.tmodloader_dir.join("tModLoader-Logs").join("client.log");
        fs::write(&log_path, "[INFO] tModLoader v2024.1.2.0").unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(7200);
        File::options()
            .write(true)
            .open(&log_path)
            .unwrap()
            .set_modified(old_time)
            .unwrap();

        fs::write(paths.tmodloader_dll(), b"NEW-DLL").unwrap();
        assert_eq!(detect_installed_tml_year_month(&paths), (2026, 9));

        fs::write(
            paths.tmodloader_dir.join("tModLoader-Logs").join("server.log"),
            "[INFO] tModLoader v2026.5.1.0",
        )
        .unwrap();
        assert_eq!(detect_installed_tml_year_month(&paths), (2026, 5));

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn record_workshop_install_assigns_nonzero_timestamp_without_stale_inheritance() {
        let paths = make_temp_paths("ws_nonzero_ts");
        paths.ensure_dirs().unwrap();
        let mod_path = paths.tml_mods_dir().join("TimeMod.tmod");
        write_minimal_tmod(&mod_path, "TimeMod", "Time Mod", "1.0.0");
        let meta = parse_tmod_file(&mod_path).unwrap();

        let old_details = WorkshopItemDetails {
            published_file_id: "555555555".to_string(),
            title: "Time Mod".to_string(),
            time_updated: 1_500_000_000,
            ..Default::default()
        };
        record_workshop_install(&paths, Some(&old_details), "555555555", std::slice::from_ref(&meta))
            .unwrap();

        record_workshop_install(&paths, None, "555555555", &[meta]).unwrap();
        let loaded = load_workshop_manifest(&paths);
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].time_updated > 1_500_000_000);

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn delete_installed_mod_cleans_enabled_and_workshop_manifest_case_insensitively() {
        let paths = make_temp_paths("del_manifest");
        paths.ensure_dirs().unwrap();
        let mod_path = paths.tml_mods_dir().join("AlphaMod.tmod");
        write_minimal_tmod(&mod_path, "AlphaMod", "Alpha Mod", "1.0.0");
        let meta = parse_tmod_file(&mod_path).unwrap();
        set_mod_enabled(&paths, "alphamod", true).unwrap();
        record_workshop_install(&paths, None, "777777777", &[meta]).unwrap();
        assert_eq!(crate::modpack_sync::load_mod_sources(&paths).len(), 1);

        delete_installed_mod(&paths, &mod_path, "ALPHAMOD").unwrap();
        assert!(!mod_path.exists());
        assert!(load_enabled_mods(&paths.tml_enabled_json()).unwrap().is_empty());
        assert!(load_workshop_manifest(&paths).is_empty());
        assert!(crate::modpack_sync::load_mod_sources(&paths).is_empty());

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn case_insensitive_scan_enable_and_set_mod_enabled() {
        let paths = make_temp_paths("ci_enable");
        paths.ensure_dirs().unwrap();

        write_minimal_tmod(
            &paths.tml_mods_dir().join("BossLib.tmod"),
            "BossLib",
            "Boss Lib",
            "1.0.0",
        );
        write_minimal_tmod_with_refs(
            &paths.tml_mods_dir().join("CalamityMod.tmod"),
            "CalamityMod",
            "Calamity Mod",
            "2.0.0",
            &["bosslib"],
        );

        save_enabled_mods(&paths.tml_enabled_json(), &["calamitymod".to_string()]).unwrap();
        let scanned = scan_installed_mods(&paths).unwrap();
        let cal = scanned
            .iter()
            .find(|e| e.meta.internal_name == "CalamityMod")
            .unwrap();
        assert!(cal.enabled);
        assert!(cal.missing_deps.is_empty());
        assert_eq!(cal.disabled_deps, vec!["bosslib".to_string()]);

        let newly = enable_mod_and_installed_deps(&paths, "CALAMITYMOD").unwrap();
        assert_eq!(newly, vec!["BossLib".to_string()]);
        let enabled = load_enabled_mods(&paths.tml_enabled_json()).unwrap();
        assert_eq!(enabled, vec!["calamitymod".to_string(), "BossLib".to_string()]);

        set_mod_enabled(&paths, "BOSSLIB", false).unwrap();
        let after_disable = load_enabled_mods(&paths.tml_enabled_json()).unwrap();
        assert_eq!(after_disable, vec!["calamitymod".to_string()]);

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn download_and_install_tmod_url_rejects_unsafe_stem_and_removes_duplicates() {
        let paths = make_temp_paths("dl_url_safe");
        paths.ensure_dirs().unwrap();

        let temp_src = paths.base_dir.join("src");
        fs::create_dir_all(&temp_src).unwrap();
        let bad_tmod = temp_src.join("bad.tmod");
        write_minimal_tmod(&bad_tmod, "../Escape", "Escape", "1.0.0");
        let bad_bytes = fs::read(&bad_tmod).unwrap();

        let good_tmod = temp_src.join("good.tmod");
        write_minimal_tmod(&good_tmod, "CleanMod", "Clean Mod", "2.0.0");
        let good_bytes = fs::read(&good_tmod).unwrap();

        let dup_old = paths.tml_mods_dir().join("cleanmod_old.tmod");
        write_minimal_tmod(&dup_old, "cleanmod", "Clean Mod Old", "1.0.0");

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for body in [bad_bytes, good_bytes] {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut buf = [0u8; 1024];
                    let _ = std::io::Read::read(&mut stream, &mut buf);
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(hdr.as_bytes());
                    let _ = stream.write_all(&body);
                }
            }
        });

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let err = download_and_install_tmod_url(
                &format!("http://{addr}/../../unsafe.tmod"),
                Some("../ignored"),
                &paths,
                None,
                true,
            )
            .await
            .unwrap_err();
            assert!(matches!(err, LauncherError::InvalidFormat(_)));

            let ok_meta = download_and_install_tmod_url(
                &format!("http://{addr}/CleanMod.tmod"),
                None,
                &paths,
                None,
                true,
            )
            .await
            .unwrap();
            assert_eq!(ok_meta.internal_name, "CleanMod");
        });
        server.join().unwrap();

        assert!(paths.tml_mods_dir().join("CleanMod.tmod").is_file());
        assert!(!dup_old.exists());
        let leftover_parts = fs::read_dir(paths.tml_mods_dir())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".tl_dl_"));
        assert!(!leftover_parts);

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn workshop_snapshot_and_marker_gate_rejects_stale_staging_on_failure() {
        let paths = make_temp_paths("ws_snapshot_gate");
        paths.ensure_dirs().unwrap();
        let item_dir = workshop_staging_content_dir(&paths).join("123456789");
        let stale_file = item_dir.join("2026.9").join("StaleMod.tmod");
        write_minimal_tmod(&stale_file, "StaleMod", "Stale Mod", "1.0.0");

        let before = snapshot_tmod_files(&item_dir);
        assert!(!has_any_tmod_changed_or_new(&item_dir, &before));
        assert!(!line_indicates_workshop_downloaded(
            "ERROR! Timeout downloading item 123456789.",
            "123456789"
        ));
        assert!(line_indicates_workshop_downloaded(
            "Success. Downloaded item 123456789 to C:\\staging",
            "123456789"
        ));
        assert!(!should_accept_workshop_download(false, false, false));
        assert!(should_accept_workshop_download(false, true, false));
        assert!(should_accept_workshop_download(true, false, false));

        write_minimal_tmod(&stale_file, "StaleMod", "Stale Mod", "1.0.1-longer");
        assert!(has_any_tmod_changed_or_new(&item_dir, &before));

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn mod_download_progress_fraction_handles_bounds_and_zero() {
        let mut p = ModDownloadProgress {
            item_index: 1,
            item_total: 1,
            label: "Calamity".to_string(),
            workshop_id: None,
            stage: "Đang tải...".to_string(),
            current_bytes: 50,
            total_bytes: None,
        };
        assert_eq!(p.fraction(), None);

        p.total_bytes = Some(0);
        assert_eq!(p.fraction(), None);

        p.total_bytes = Some(100);
        assert_eq!(p.fraction(), Some(0.5));

        p.current_bytes = 200;
        assert_eq!(p.fraction(), Some(1.0));
    }

    #[test]
    fn steam_content_log_and_stdout_parsers_match_real_lines() {
        let started = "[2026-11-15 23:47:12] AppID 1281930 update started : download 0/328600272, store 0/0, reuse 0/0, delta 0/0, stage 0/404391973";
        assert_eq!(
            parse_steam_content_log_update_started(started),
            Some((328_600_272, 404_391_973))
        );
        assert_eq!(parse_steam_content_log_update_started("unrelated line"), None);

        assert_eq!(
            parse_steam_content_log_phase(
                "[2026-11-15 23:47:10] AppID 1281930 Workshop update changed : Running Update,Reconfiguring,"
            ),
            Some(SteamWorkshopPhase::Reconfiguring)
        );
        assert_eq!(
            parse_steam_content_log_phase(
                "[2026-11-15 23:47:12] AppID 1281930 Workshop update changed : Running Update,Preallocating,"
            ),
            Some(SteamWorkshopPhase::Preallocating)
        );
        assert_eq!(
            parse_steam_content_log_phase(
                "[2026-11-15 23:47:12] AppID 1281930 Workshop update changed : Running Update,Downloading,Staging,"
            ),
            Some(SteamWorkshopPhase::Downloading)
        );
        assert_eq!(
            parse_steam_content_log_phase(
                "[2026-11-15 23:47:47] AppID 1281930 Workshop update changed : Running Update,Committing,"
            ),
            Some(SteamWorkshopPhase::Committing)
        );
        assert_eq!(
            parse_steam_content_log_phase(
                "[2026-11-15 23:47:47] AppID 1281930 Workshop update changed : None"
            ),
            Some(SteamWorkshopPhase::Finished)
        );
        assert_eq!(
            parse_steam_content_log_phase("AppID 1281930 Workshop update changed : UnknownPhase"),
            None
        );

        let rate_line = "[2026-11-15 23:47:42] Increasing target number of download connections to 10 (rate was 67.763, now 81.647)";
        let parsed_rate = parse_steam_content_log_rate_mbps(rate_line).unwrap();
        assert!((parsed_rate - 81.647).abs() < 1e-6);
        assert_eq!(
            parse_steam_content_log_rate_mbps(
                "Increasing target number of download connections to 1 (rate was 0.000, now 0.000)"
            ),
            None
        );

        assert_eq!(
            extract_steamcmd_downloading_item_id("Downloading item 2824688072..."),
            Some("2824688072".to_string())
        );
        assert_eq!(extract_steamcmd_downloading_item_id("Downloading item "), None);

        assert_eq!(
            parse_steamcmd_stdout_stage("Connecting anonymously to Steam Public...OK"),
            Some("Đang đăng nhập SteamCMD...")
        );
        assert_eq!(
            parse_steamcmd_stdout_stage("Waiting for client config...OK"),
            Some("Đang xác thực phiên Steam...")
        );
        assert_eq!(
            parse_steamcmd_stdout_stage("Waiting for user info...OK"),
            Some("Đang xác thực phiên Steam...")
        );
        assert_eq!(
            parse_steamcmd_stdout_stage("Downloading item 2824688072..."),
            Some("Đang kiểm tra gói Workshop...")
        );
        assert_eq!(
            parse_steamcmd_stdout_stage("Success. Downloaded item 2824688072 to C:\\staging"),
            Some("Đang cài đặt tệp .tmod...")
        );
        assert_eq!(parse_steamcmd_stdout_stage("Random SteamCMD line"), None);
    }

    #[test]
    fn read_new_complete_lines_handles_partial_completed_and_truncated_files() {
        let paths = make_temp_paths("read_log_lines");
        paths.ensure_dirs().unwrap();
        let log_path = paths.base_dir.join("content_log.txt");

        fs::write(&log_path, b"first line\nsecond line\npartial").unwrap();
        let mut offset = 0u64;
        let first = read_new_complete_lines(&log_path, &mut offset);
        assert_eq!(first, vec!["first line".to_string(), "second line".to_string()]);
        assert_eq!(offset, b"first line\nsecond line\n".len() as u64);

        let still_partial = read_new_complete_lines(&log_path, &mut offset);
        assert!(still_partial.is_empty());

        {
            let mut f = fs::OpenOptions::new().append(true).open(&log_path).unwrap();
            f.write_all(b" completed\n").unwrap();
        }
        let completed = read_new_complete_lines(&log_path, &mut offset);
        assert_eq!(completed, vec!["partial completed".to_string()]);
        assert_eq!(offset, fs::metadata(&log_path).unwrap().len());

        fs::write(&log_path, b"reset\n").unwrap();
        let after_truncate = read_new_complete_lines(&log_path, &mut offset);
        assert_eq!(after_truncate, vec!["reset".to_string()]);
        assert_eq!(offset, b"reset\n".len() as u64);

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn steam_telemetry_tracker_caps_bytes_formats_speed_and_resets_cleanly() {
        let mut trk = SteamContentTelemetryTracker::new("Đang khởi động SteamCMD...", None);
        assert!(trk.apply_stdout_line("Connecting anonymously to Steam Public...OK"));
        assert!(trk.take_changed_for_key(Some("123")).is_some());

        trk.apply_telemetry(
            &[
                "AppID 1281930 Workshop update changed : Running Update,Preallocating,".to_string(),
                "AppID 1281930 Workshop update changed : Running Update,".to_string(),
                "AppID 1281930 update started : download 0/800, store 0/0, reuse 0/0, delta 0/0, stage 0/1000".to_string(),
                "AppID 1281930 Workshop update changed : Running Update,Downloading,Staging,".to_string(),
            ],
            Some(5_000),
        );
        let (_, cur, tot) = trk.take_changed_for_key(Some("123")).unwrap();
        assert_eq!(cur, 0);
        assert_eq!(tot, Some(1000));

        trk.apply_telemetry(
            &[
                "Increasing target number of download connections to 10 (rate was 0.000, now 81.647)"
                    .to_string(),
            ],
            Some(6_500),
        );
        let (stage, cur, tot) = trk.take_changed_for_key(Some("123")).unwrap();
        assert!(stage.contains("10.2 MB/s"), "unexpected stage: {stage}");
        assert_eq!(cur, 999);
        assert_eq!(tot, Some(1000));

        assert!(!trk.apply_stdout_line("Connecting anonymously to Steam Public...OK"));

        trk.apply_telemetry(
            &["AppID 1281930 Workshop update changed : None".to_string()],
            Some(6_500),
        );
        let (_, cur, tot) = trk.take_changed_for_key(Some("123")).unwrap();
        assert_eq!(cur, 1000);
        assert_eq!(tot, Some(1000));

        trk.reset_for_new_item("Đang kiểm tra gói Workshop...");
        assert_eq!(trk.current_bytes(), 0);
        assert_eq!(trk.total_bytes(), None);
    }

    #[test]
    fn download_and_install_tmod_url_with_source_and_progress_reports_bytes() {
        let paths = make_temp_paths("dl_url_progress");
        paths.ensure_dirs().unwrap();

        let src_tmod = paths.base_dir.join("src").join("ProgressMod.tmod");
        write_minimal_tmod(&src_tmod, "ProgressMod", "Progress Mod", "1.0.0");
        let body = fs::read(&src_tmod).unwrap();
        let expected_len = body.len() as u64;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = std::io::Read::read(&mut stream, &mut buf);
                let hdr = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(hdr.as_bytes());
                let _ = stream.write_all(&body);
            }
        });

        let events = Arc::new(Mutex::new(Vec::<ModDownloadProgress>::new()));
        let events_clone = Arc::clone(&events);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let meta = download_and_install_tmod_url_with_source_and_progress(
                &format!("http://{addr}/ProgressMod.tmod"),
                Some("Progress Mod"),
                None,
                None,
                &paths,
                None,
                false,
                move |prog| events_clone.lock().unwrap().push(prog),
            )
            .await
            .unwrap();
            assert_eq!(meta.internal_name, "ProgressMod");
        });
        server.join().unwrap();

        let captured = events.lock().unwrap();
        assert!(captured.len() >= 3);
        assert_eq!(captured[0].label, "Progress Mod");
        assert_eq!(captured[0].current_bytes, 0);
        let last = captured.last().unwrap();
        assert_eq!(last.current_bytes, expected_len);
        assert_eq!(last.total_bytes, Some(expected_len));
        assert_eq!(last.fraction(), Some(1.0));

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn scan_staged_workshop_mods_rejects_unrelated_local_custom_tmod() {
        let paths = make_temp_paths("staged_sha1_guard");
        paths.ensure_dirs().unwrap();

        let staged_custom = paths
            .steamcmd_dir
            .join("staging/steamapps/workshop/content/1281930/111111111/2026.9/CustomMod.tmod");
        write_minimal_tmod_full(&staged_custom, "CustomMod", "Custom Mod", "1.0.0", &[], [0xAA; 20]);

        let local_custom = paths.tml_mods_dir().join("CustomMod.tmod");
        write_minimal_tmod_full(&local_custom, "custommod", "Custom Mod Local", "1.0.0", &[], [0xBB; 20]);
        set_mod_enabled(&paths, "custommod", true).unwrap();

        let staged_mismatch = scan_staged_workshop_mods(&paths).unwrap();
        assert_eq!(staged_mismatch.len(), 1);
        assert!(!staged_mismatch[0].installed);
        assert!(!staged_mismatch[0].enabled);

        let staged_same_sha = paths
            .steamcmd_dir
            .join("staging/steamapps/workshop/content/1281930/222222222/2026.9/MatchShaMod.tmod");
        write_minimal_tmod_full(&staged_same_sha, "MatchShaMod", "Match SHA", "1.0.0", &[], [0xCC; 20]);
        let installed_same_sha = paths.tml_mods_dir().join("MatchShaMod.tmod");
        write_minimal_tmod_full(&installed_same_sha, "matchshamod", "Match SHA", "1.0.0", &[], [0xCC; 20]);
        set_mod_enabled(&paths, "MATCHSHAMOD", true).unwrap();

        let staged_manifest_diff_sha = paths
            .steamcmd_dir
            .join("staging/steamapps/workshop/content/1281930/333333333/2026.9/UpdatedWsMod.tmod");
        write_minimal_tmod_full(
            &staged_manifest_diff_sha,
            "UpdatedWsMod",
            "Updated WS",
            "1.0.0",
            &[],
            [0xDD; 20],
        );
        let installed_updated_ws = paths.tml_mods_dir().join("UpdatedWsMod.tmod");
        write_minimal_tmod_full(
            &installed_updated_ws,
            "updatedwsmod",
            "Updated WS",
            "1.1.0",
            &[],
            [0xEE; 20],
        );
        let ws_meta = parse_tmod_file(&installed_updated_ws).unwrap();
        record_workshop_install(&paths, None, "333333333", &[ws_meta]).unwrap();

        let staged = scan_staged_workshop_mods(&paths).unwrap();
        let by_id: HashMap<String, StagedWorkshopMod> = staged
            .into_iter()
            .map(|s| (s.workshop_id.clone(), s))
            .collect();
        assert!(!by_id["111111111"].installed);
        assert!(by_id["222222222"].installed);
        assert!(by_id["222222222"].enabled);
        assert!(by_id["333333333"].installed);
        assert!(!by_id["333333333"].enabled);

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn url_mod_install_clears_stale_workshop_manifest_entry() {
        let paths = make_temp_paths("url_clears_ws_manifest");
        paths.ensure_dirs().unwrap();

        let old_ws = paths.tml_mods_dir().join("OverwrittenMod.tmod");
        write_minimal_tmod(&old_ws, "OverwrittenMod", "Overwritten Mod", "1.0.0");
        let old_meta = parse_tmod_file(&old_ws).unwrap();
        record_workshop_install(&paths, None, "444444444", &[old_meta]).unwrap();
        assert_eq!(load_workshop_manifest(&paths).len(), 1);

        let src_new = paths.base_dir.join("src").join("OverwrittenMod.tmod");
        write_minimal_tmod_full(&src_new, "overwrittenmod", "Overwritten Mod", "2.0.0", &[], [0x55; 20]);
        let body = fs::read(&src_new).unwrap();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = std::io::Read::read(&mut stream, &mut buf);
                let hdr = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(hdr.as_bytes());
                let _ = stream.write_all(&body);
            }
        });

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            download_and_install_tmod_url_with_source_and_progress(
                &format!("http://{addr}/OverwrittenMod.tmod"),
                Some("OverwrittenMod"),
                Some("Owner/OverwrittenMod"),
                None,
                &paths,
                None,
                false,
                |_| {},
            )
            .await
            .unwrap();
        });
        server.join().unwrap();

        assert!(load_workshop_manifest(&paths).is_empty());
        let sources = crate::modpack_sync::load_mod_sources(&paths);
        assert_eq!(sources.len(), 1);
        assert!(matches!(
            sources[0].source,
            tl_core::SharedModSource::Github { .. }
        ));

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn download_and_install_tmod_url_with_auth_and_progress_verifies_headers_and_401_fallback() {
        let paths = make_temp_paths("dl_gh_auth");
        paths.ensure_dirs().unwrap();

        let src_tmod = paths.base_dir.join("src").join("AuthMod.tmod");
        write_minimal_tmod(&src_tmod, "AuthMod", "Auth Mod", "1.2.3");
        let body = fs::read(&src_tmod).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let seen_requests = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
            let seen_clone = Arc::clone(&seen_requests);
            let body_srv = body.clone();

            let server = tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    let seen = Arc::clone(&seen_clone);
                    let body = body_srv.clone();
                    tokio::spawn(async move {
                        let mut raw = Vec::new();
                        let mut buf = [0u8; 1024];
                        loop {
                            let Ok(n) = socket.read(&mut buf).await else {
                                return;
                            };
                            if n == 0 {
                                break;
                            }
                            raw.extend_from_slice(&buf[..n]);
                            if raw.windows(4).any(|w| w == b"\r\n\r\n") || raw.len() >= 16384 {
                                break;
                            }
                        }
                        let req_text = String::from_utf8_lossy(&raw).into_owned();
                        let target = req_text
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_string();
                        seen.lock().unwrap().push((target.clone(), req_text.clone()));

                        let lower = req_text.to_ascii_lowercase();
                        let (status, resp_body): (&str, Vec<u8>) =
                            if target == "/repos/owner/repo/releases/assets/42" {
                                let has_bearer = lower.contains("authorization: bearer good-token");
                                let has_accept = lower.contains("accept: application/octet-stream");
                                let has_ver = lower.contains("x-github-api-version: 2022-11-28");
                                if has_bearer && has_accept && has_ver {
                                    ("200 OK", body)
                                } else {
                                    ("401 Unauthorized", b"unauthorized".to_vec())
                                }
                            } else if target == "/owner/repo/releases/download/v1.0/AuthMod.tmod" {
                                if lower.contains("authorization:") {
                                    ("400 Bad Request", b"unexpected auth".to_vec())
                                } else {
                                    ("200 OK", body)
                                }
                            } else {
                                ("404 Not Found", b"not found".to_vec())
                            };

                        let hdr = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            resp_body.len()
                        );
                        let _ = socket.write_all(hdr.as_bytes()).await;
                        let _ = socket.write_all(&resp_body).await;
                        let _ = socket.shutdown().await;
                    });
                }
            });

            let browser_url = format!(
                "http://{addr}/owner/repo/releases/download/v1.0/AuthMod.tmod"
            );
            let api_url = format!("http://{addr}/repos/owner/repo/releases/assets/42");

            let meta_ok = download_and_install_tmod_url_with_auth_and_progress(
                &browser_url,
                Some(&api_url),
                Some("good-token"),
                Some("AuthMod.tmod"),
                Some("owner/repo"),
                None,
                &paths,
                None,
                false,
                |_| {},
            )
            .await
            .unwrap();
            assert_eq!(meta_ok.internal_name, "AuthMod");
            {
                let reqs = seen_requests.lock().unwrap();
                assert_eq!(reqs.len(), 1);
                assert_eq!(reqs[0].0, "/repos/owner/repo/releases/assets/42");
                let lower = reqs[0].1.to_ascii_lowercase();
                assert!(lower.contains("authorization: bearer good-token"));
                assert!(lower.contains("accept: application/octet-stream"));
                assert!(lower.contains("x-github-api-version: 2022-11-28"));
            }

            let meta_fallback = download_and_install_tmod_url_with_auth_and_progress(
                &browser_url,
                Some(&api_url),
                Some("expired-token"),
                Some("AuthMod.tmod"),
                Some("owner/repo"),
                None,
                &paths,
                None,
                false,
                |_| {},
            )
            .await
            .unwrap();
            assert_eq!(meta_fallback.internal_name, "AuthMod");
            {
                let reqs = seen_requests.lock().unwrap();
                assert_eq!(reqs.len(), 3);
                assert_eq!(reqs[1].0, "/repos/owner/repo/releases/assets/42");
                assert_eq!(reqs[2].0, "/owner/repo/releases/download/v1.0/AuthMod.tmod");
                assert!(!reqs[2].1.to_ascii_lowercase().contains("authorization:"));
            }

            let sources = crate::modpack_sync::load_mod_sources(&paths);
            assert_eq!(sources.len(), 1);
            assert_eq!(
                sources[0].source,
                tl_core::SharedModSource::Github {
                    repo: "owner/repo".to_string(),
                    url: browser_url,
                }
            );

            server.abort();
        });

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn record_workshop_install_evicts_cross_id_case_insensitively_and_uses_fallback_ts() {
        let paths = make_temp_paths("ws_cross_id_evict");
        paths.ensure_dirs().unwrap();

        let mod_path_v1 = paths.tml_mods_dir().join("ReuploadMod.tmod");
        write_minimal_tmod(&mod_path_v1, "ReuploadMod", "Reupload Mod", "1.0.0");
        let meta_v1 = parse_tmod_file(&mod_path_v1).unwrap();

        let old_details = WorkshopItemDetails {
            published_file_id: "111111111".to_string(),
            title: "Reupload Mod Old".to_string(),
            time_updated: 1_700_000_000,
            ..Default::default()
        };
        record_workshop_install(&paths, Some(&old_details), "111111111", &[meta_v1]).unwrap();

        let mut meta_v2 = parse_tmod_file(&mod_path_v1).unwrap();
        meta_v2.internal_name = "reuploadmod".to_string();
        meta_v2.version = "1.1.0".to_string();
        record_workshop_install_with_timestamp(
            &paths,
            None,
            Some(1_750_000_000),
            "222222222",
            &[meta_v2],
        )
        .unwrap();

        let loaded = load_workshop_manifest(&paths);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].published_file_id, "222222222");
        assert_eq!(loaded[0].internal_name, "reuploadmod");
        assert_eq!(loaded[0].mod_version, "1.1.0");
        assert_eq!(loaded[0].time_updated, 1_750_000_000);

        let _ = fs::remove_dir_all(&paths.base_dir);
    }
}
