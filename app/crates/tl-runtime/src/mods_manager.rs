use crate::http_client::{build_http_client, normalize_proxy_url};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::error::Error as StdError;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tl_core::config::write_atomic;
use tl_core::{
    load_enabled_mods, parse_tmod_file, save_enabled_mods, LauncherError, LauncherPaths,
    LauncherResult, TmodMetadata,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use zip::ZipArchive;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TMODLOADER_STEAM_APP_ID: u64 = 1281930;
const MAX_WORKSHOP_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const STEAM_GET_PUBLISHED_FILE_DETAILS_URL: &str =
    "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/";
const STEAM_WORKSHOP_BROWSE_URL: &str = "https://steamcommunity.com/workshop/browse/";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledModEntry {
    pub meta: TmodMetadata,
    pub enabled: bool,
    pub missing_deps: Vec<String>,
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

pub fn scan_installed_mods(paths: &LauncherPaths) -> LauncherResult<Vec<InstalledModEntry>> {
    let mods_dir = paths.tml_mods_dir();
    fs::create_dir_all(&mods_dir)?;

    let enabled_list = load_enabled_mods(&paths.tml_enabled_json()).unwrap_or_default();
    let enabled_set: HashSet<String> = enabled_list.into_iter().collect();

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

    let available_names: HashSet<String> = parsed.iter().map(|m| m.internal_name.clone()).collect();

    let mut out: Vec<InstalledModEntry> = parsed
        .into_iter()
        .map(|meta| {
            let enabled = enabled_set.contains(&meta.internal_name);
            let missing_deps = meta
                .mod_references
                .iter()
                .filter_map(|raw_ref| {
                    let dep_name = raw_ref.split('@').next().unwrap_or(raw_ref).trim();
                    if dep_name.is_empty() || available_names.contains(dep_name) {
                        None
                    } else {
                        Some(dep_name.to_string())
                    }
                })
                .collect();
            InstalledModEntry {
                meta,
                enabled,
                missing_deps,
            }
        })
        .collect();

    out.sort_by_key(|a| a.meta.display_name.to_lowercase());
    Ok(out)
}

pub fn set_mod_enabled(
    paths: &LauncherPaths,
    internal_name: &str,
    enable: bool,
) -> LauncherResult<Vec<String>> {
    let enabled_path = paths.tml_enabled_json();
    let mut current = load_enabled_mods(&enabled_path).unwrap_or_default();
    if enable {
        if !current.iter().any(|n| n == internal_name) {
            current.push(internal_name.to_string());
        }
    } else {
        current.retain(|n| n != internal_name);
    }
    save_enabled_mods(&enabled_path, &current)?;
    Ok(current)
}

pub fn delete_installed_mod(
    paths: &LauncherPaths,
    mod_file_path: &Path,
    internal_name: &str,
) -> LauncherResult<()> {
    if mod_file_path.is_file() {
        fs::remove_file(mod_file_path)?;
    }
    let _ = set_mod_enabled(paths, internal_name, false);
    Ok(())
}

pub fn import_local_mod_files(
    sources: &[PathBuf],
    paths: &LauncherPaths,
    auto_enable: bool,
) -> LauncherResult<Vec<TmodMetadata>> {
    let mods_dir = paths.tml_mods_dir();
    fs::create_dir_all(&mods_dir)?;
    let mut imported = Vec::new();

    for src in sources {
        let ext = src
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        if ext == "tmod" {
            let meta = parse_tmod_file(src)?;
            let dest = mods_dir.join(&meta.file_name);
            fs::copy(src, &dest)?;
            if auto_enable {
                set_mod_enabled(paths, &meta.internal_name, true)?;
            }
            imported.push(parse_tmod_file(&dest)?);
        } else if ext == "zip" {
            let mut archive = ZipArchive::new(File::open(src)?)?;
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i)?;
                if entry.is_dir() {
                    continue;
                }
                let Some(fname) = entry
                    .enclosed_name()
                    .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
                else {
                    continue;
                };
                if fname.to_ascii_lowercase().ends_with(".tmod") {
                    let temp = mods_dir.join(format!(".import_{fname}.part"));
                    {
                        let mut out = File::create(&temp)?;
                        std::io::copy(&mut entry, &mut out)?;
                    }
                    match parse_tmod_file(&temp) {
                        Ok(meta) => {
                            let dest = mods_dir.join(&fname);
                            let _ = fs::remove_file(&dest);
                            fs::rename(&temp, &dest)?;
                            if auto_enable {
                                set_mod_enabled(paths, &meta.internal_name, true)?;
                            }
                            imported.push(parse_tmod_file(&dest)?);
                        }
                        Err(e) => {
                            let _ = fs::remove_file(&temp);
                            return Err(e);
                        }
                    }
                }
            }
        }
    }

    Ok(imported)
}

pub async fn fetch_github_latest_tmod(
    owner_repo: &str,
    proxy_url: Option<&str>,
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
    let resp = client
        .get(&api_url)
        .header("Accept", "application/vnd.github+json")
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
            Some(RemoteTmodAsset {
                repo: repo.to_string(),
                tag_name: tag_name.clone(),
                asset_name: name.to_string(),
                size_bytes,
                download_url: url.to_string(),
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
    let trimmed = url.trim();
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(LauncherError::InvalidInput(
            "URL tải mod phải bắt đầu bằng http:// hoặc https://".to_string(),
        ));
    }

    let mods_dir = paths.tml_mods_dir();
    fs::create_dir_all(&mods_dir)?;

    let client = build_http_client(proxy_url)?;
    let resp = client
        .get(trimmed)
        .send()
        .await
        .map_err(|e| LauncherError::Network(format!("Lỗi tải mod: {e}")))?;

    if !resp.status().is_success() {
        return Err(LauncherError::Network(format!(
            "Server tải mod trả về HTTP {}",
            resp.status()
        )));
    }

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
        .unwrap_or_else(|| "downloaded_mod.tmod".to_string());

    let file_name = if raw_name.to_ascii_lowercase().ends_with(".tmod") {
        raw_name
    } else {
        format!("{raw_name}.tmod")
    };

    let part_path = mods_dir.join(format!(".{file_name}.part"));
    {
        let mut file = File::create(&part_path)?;
        let mut stream = resp.bytes_stream();
        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res
                .map_err(|e| LauncherError::Network(format!("Lỗi đọc stream tải mod: {e}")))?;
            file.write_all(&chunk)?;
        }
        file.flush()?;
    }

    let parsed = match parse_tmod_file(&part_path) {
        Ok(m) => m,
        Err(e) => {
            let _ = fs::remove_file(&part_path);
            return Err(LauncherError::InvalidFormat(format!(
                "File tải về không phải .tmod hợp lệ: {e}"
            )));
        }
    };

    let final_name = if parsed.internal_name.is_empty() {
        file_name
    } else {
        format!("{}.tmod", parsed.internal_name)
    };
    let final_path = mods_dir.join(&final_name);
    let _ = fs::remove_file(&final_path);
    fs::rename(&part_path, &final_path)?;

    if auto_enable {
        set_mod_enabled(paths, &parsed.internal_name, true)?;
    }

    parse_tmod_file(&final_path)
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
    let mut params: Vec<(&str, &str)> = vec![
        ("appid", "1281930"),
        ("section", "readytouseitems"),
        ("browsesort", sort.steam_param()),
        ("actualsort", sort.steam_param()),
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

pub fn record_workshop_install(
    paths: &LauncherPaths,
    details: Option<&WorkshopItemDetails>,
    workshop_id: &str,
    installed_mods: &[TmodMetadata],
) -> LauncherResult<()> {
    let workshop_id = workshop_id.trim();
    fs::create_dir_all(paths.tml_mods_dir())?;
    let mut records = load_workshop_manifest(paths);
    let installed_at = Utc::now().timestamp().max(0) as u64;

    for meta in installed_mods {
        let existing_idx = records.iter().position(|r| {
            r.published_file_id == workshop_id && r.internal_name == meta.internal_name
        });
        let previous = existing_idx.map(|idx| &records[idx]);

        let workshop_title = details
            .map(|d| d.title.trim())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .or_else(|| previous.map(|p| p.workshop_title.clone()).filter(|s| !s.is_empty()))
            .unwrap_or_else(|| meta.display_name.clone());

        let author = details
            .map(|d| d.author.trim())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .or_else(|| previous.map(|p| p.author.clone()).filter(|s| !s.is_empty()))
            .unwrap_or_else(|| meta.author.clone());

        let preview_url = details
            .map(|d| d.preview_url.trim())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .or_else(|| previous.map(|p| p.preview_url.clone()))
            .unwrap_or_default();

        let time_updated = details
            .map(|d| d.time_updated)
            .filter(|&t| t > 0)
            .or_else(|| previous.map(|p| p.time_updated))
            .unwrap_or(0);

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

        if let Some(idx) = existing_idx {
            records[idx] = record;
        } else {
            records.push(record);
        }
    }

    let json = serde_json::to_string_pretty(&records)?;
    write_atomic(&workshop_manifest_path(paths), json.as_bytes())?;
    Ok(())
}

pub fn scan_staged_workshop_mods(paths: &LauncherPaths) -> LauncherResult<Vec<StagedWorkshopMod>> {
    let content_dir = workshop_staging_content_dir(paths);
    if !content_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mods_dir = paths.tml_mods_dir();
    let mut installed_internal_names = HashSet::new();
    let mut installed_file_names = HashSet::new();
    if let Ok(entries) = fs::read_dir(&mods_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("tmod"))
                .unwrap_or(false)
            {
                if let Some(fname) = p.file_name().and_then(|s| s.to_str()) {
                    installed_file_names.insert(fname.to_string());
                }
                if let Ok(meta) = parse_tmod_file(&p) {
                    if !meta.internal_name.is_empty() {
                        installed_internal_names.insert(meta.internal_name);
                    }
                }
            }
        }
    }

    let enabled_set: HashSet<String> = load_enabled_mods(&paths.tml_enabled_json())
        .unwrap_or_default()
        .into_iter()
        .collect();
    let manifest = load_workshop_manifest(paths);

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
        let Some(best_tmod) = pick_best_workshop_tmod(&found_tmods) else {
            continue;
        };
        let Ok(meta) = parse_tmod_file(&best_tmod) else {
            continue;
        };

        let installed = (!meta.internal_name.is_empty()
            && installed_internal_names.contains(&meta.internal_name))
            || installed_file_names.contains(&meta.file_name);
        let enabled = installed && enabled_set.contains(&meta.internal_name);

        let manifest_rec = manifest
            .iter()
            .find(|r| r.published_file_id == workshop_id && r.internal_name == meta.internal_name)
            .or_else(|| manifest.iter().find(|r| r.published_file_id == workshop_id));

        let workshop_title = manifest_rec
            .and_then(|r| (!r.workshop_title.is_empty()).then(|| r.workshop_title.clone()));
        let author = manifest_rec.and_then(|r| (!r.author.is_empty()).then(|| r.author.clone()));
        let preview_url =
            manifest_rec.and_then(|r| (!r.preview_url.is_empty()).then(|| r.preview_url.clone()));
        let time_updated = manifest_rec.and_then(|r| (r.time_updated > 0).then_some(r.time_updated));

        out.push(StagedWorkshopMod {
            workshop_id,
            mod_dir,
            meta,
            installed,
            enabled,
            workshop_title,
            author,
            preview_url,
            time_updated,
        });
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

pub fn pick_best_workshop_tmod(candidates: &[PathBuf]) -> Option<PathBuf> {
    if candidates.is_empty() {
        return None;
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
        return None;
    }

    if let Some(preferred) = pool
        .iter()
        .copied()
        .filter(|p| workshop_path_version(p) == Some((2026, 9)))
        .max_by(|a, b| a.as_os_str().cmp(b.as_os_str()))
    {
        return Some(preferred.clone());
    }

    pool.into_iter()
        .max_by(|a, b| {
            let va = workshop_path_version(a);
            let vb = workshop_path_version(b);
            va.cmp(&vb).then_with(|| a.as_os_str().cmp(b.as_os_str()))
        })
        .cloned()
}

pub async fn install_steamcmd_via_winget() -> LauncherResult<String> {
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
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = cmd
        .output()
        .await
        .map_err(|e| LauncherError::Process(format!("Không chạy được winget: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        return Err(LauncherError::Process(format!(
            "winget cài SteamCMD thất bại: {stdout} {stderr}"
        )));
    }
    Ok(if stdout.is_empty() {
        "Đã cài đặt SteamCMD qua winget".to_string()
    } else {
        stdout
    })
}

pub async fn download_workshop_mod_via_steamcmd(
    steamcmd_exe: &Path,
    paths: &LauncherPaths,
    workshop_id: &str,
    auto_enable: bool,
    on_log: impl Fn(String) + Send + Sync + 'static,
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

    let mut cmd = Command::new(steamcmd_exe);
    cmd.arg("+force_install_dir")
        .arg(&staging_dir)
        .arg("+login")
        .arg("anonymous")
        .arg("+workshop_download_item")
        .arg("1281930")
        .arg(workshop_id)
        .arg("+quit");

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| LauncherError::Process(format!("Không thể chạy steamcmd.exe: {e}")))?;

    let log_cb = Arc::new(on_log);
    let stdout_task = child.stdout.take().map(|stdout| {
        let cb = Arc::clone(&log_cb);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    cb(line);
                }
            }
        })
    });
    let stderr_task = child.stderr.take().map(|stderr| {
        let cb = Arc::clone(&log_cb);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    cb(line);
                }
            }
        })
    });

    let status = child
        .wait()
        .await
        .map_err(|e| LauncherError::Process(format!("Lỗi chờ steamcmd.exe: {e}")))?;

    if let Some(h) = stdout_task {
        let _ = h.await;
    }
    if let Some(h) = stderr_task {
        let _ = h.await;
    }

    let workshop_item_dir = workshop_staging_content_dir(paths).join(workshop_id);

    let mut found_tmods = Vec::new();
    collect_tmods_recursive(&workshop_item_dir, &mut found_tmods);

    let Some(best_tmod) = pick_best_workshop_tmod(&found_tmods) else {
        return Err(LauncherError::NotFound(format!(
            "SteamCMD kết thúc ({:?}) nhưng không tìm thấy file .tmod trong {}. Nếu Steam bị chặn kết nối, hãy bật Proxy/VPN hoặc dùng GitHub/Import .tmod.",
            status.code(),
            workshop_item_dir.display()
        )));
    };

    import_local_mod_files(&[best_tmod], paths, auto_enable)
}

fn validate_workshop_id(workshop_id: &str) -> LauncherResult<&str> {
    let trimmed = workshop_id.trim();
    if trimmed.is_empty() || !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Err(LauncherError::InvalidInput(
            "Workshop ID chỉ gồm chữ số".to_string(),
        ));
    }
    Ok(trimmed)
}

fn workshop_staging_content_dir(paths: &LauncherPaths) -> PathBuf {
    paths
        .steamcmd_dir
        .join("staging")
        .join("steamapps")
        .join("workshop")
        .join("content")
        .join("1281930")
}

fn collect_tmods_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
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

fn parse_workshop_version_tuple(segment: &str) -> Option<(u32, u32)> {
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
    Some((major, minor))
}

fn workshop_path_version(path: &Path) -> Option<(u32, u32)> {
    let mut dir = path.parent();
    while let Some(current) = dir {
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

async fn enrich_workshop_items_batch(client: &reqwest::Client, items: &mut [WorkshopItemDetails]) {
    if items.is_empty() {
        return;
    }

    let mut params: Vec<(String, String)> = Vec::with_capacity(items.len() + 2);
    params.push(("itemcount".to_string(), items.len().to_string()));
    params.push(("includechildren".to_string(), "true".to_string()));
    for (idx, item) in items.iter().enumerate() {
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
        return;
    };

    let Some(details_arr) = json["response"]["publishedfiledetails"].as_array() else {
        return;
    };

    let mut by_id: HashMap<String, &serde_json::Value> = HashMap::with_capacity(details_arr.len());
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

    for item in items.iter_mut() {
        if let Some(api_item) = by_id.get(&item.published_file_id) {
            apply_api_details(item, api_item, false);
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
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut info = Vec::new();
        write_dotnet_str(&mut info, "displayName");
        write_dotnet_str(&mut info, display_name);
        write_dotnet_str(&mut info, "author");
        write_dotnet_str(&mut info, "TestAuthor");
        write_dotnet_str(&mut info, "");

        let mut tmod = Vec::new();
        tmod.extend_from_slice(b"TMOD");
        write_dotnet_str(&mut tmod, "2026.9.3.1");
        tmod.extend_from_slice(&[0x11u8; 20]);
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
        assert_eq!(parse_workshop_version_tuple("2026.10"), Some((2026, 10)));
        assert_eq!(parse_workshop_version_tuple("2026.09"), Some((2026, 9)));
        assert_eq!(parse_workshop_version_tuple("2026.2"), Some((2026, 2)));
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
            PathBuf::from(r"C:\staging\1281930\2824688072\2026.10\CalamityMod.tmod")
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
            .join("staging/steamapps/workshop/content/1281930/2824688072/2026.10/CalamityMod.tmod");
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
}
