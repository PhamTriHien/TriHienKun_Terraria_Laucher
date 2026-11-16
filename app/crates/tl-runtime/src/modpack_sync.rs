use crate::http_client::build_http_client;
#[cfg(windows)]
use crate::mods_manager::CREATE_NO_WINDOW;
use crate::mods_manager::{
    apply_proxy_env, browse_workshop_mods, collect_tmods_recursive, current_file_len,
    detect_installed_tml_year_month, download_and_install_tmod_url_with_auth_and_progress,
    download_and_install_tmod_url_with_source_and_progress, enrich_workshop_items_batch,
    extract_steamcmd_downloading_item_id, fetch_github_latest_tmod, file_mtime_unix,
    has_any_tmod_changed_or_new, import_local_mod_files, load_workshop_manifest, parse_workshop_id,
    pick_best_workshop_tmods, read_new_complete_lines, read_process_written_bytes,
    record_workshop_install, record_workshop_install_with_timestamp, scan_installed_mods,
    scan_staged_workshop_mods, snapshot_tmod_files, steamcmd_content_log_path,
    validate_workshop_id, workshop_staging_content_dir, InstalledModEntry, ModDownloadProgress,
    RemoteTmodAsset, StagedWorkshopMod, SteamContentTelemetryTracker, SteamWorkshopPhase,
    TmodFileSnapshot, WorkshopItemDetails, WorkshopManifestRecord, WorkshopSort,
    TMODLOADER_STEAM_APP_ID,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tl_core::config::write_atomic;
use tl_core::{
    activate_modpack, backup_unsaved_enabled_mods_as_modpack, create_modpack,
    decode_modpack_share_key, encode_modpack_share_key, extract_source_hint_from_homepage,
    is_safe_single_dir_component, parse_tmod_file, resolve_unique_modpack_name,
    sanitize_modpack_name, write_modpack_install_txt, LauncherError, LauncherPaths,
    LauncherResult, ModPackShareKeyPayload, SharedModEntry, SharedModSource, TmodMetadata,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Notify;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModSourceRecord {
    pub internal_name: String,
    #[serde(default)]
    pub display_name: String,
    pub source: SharedModSource,
    #[serde(default)]
    pub mod_version: String,
    #[serde(default)]
    pub sha1_hex: String,
    pub updated_at: u64,
}

#[derive(Debug, Clone)]
pub struct ModPackExportEntry {
    pub internal_name: String,
    pub display_name: String,
    pub version: String,
    pub source: SharedModSource,
    pub homepage: String,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct ModPackSharePreview {
    pub pack_name: String,
    pub payload: ModPackShareKeyPayload,
    pub share_key: String,
    pub entries: Vec<ModPackExportEntry>,
    pub workshop_count: usize,
    pub url_count: usize,
    pub manual_count: usize,
}

#[derive(Debug, Clone)]
pub struct ModPackImportPreview {
    pub payload: ModPackShareKeyPayload,
    pub suggested_pack_name: String,
    pub already_present_count: usize,
    pub workshop_download_count: usize,
    pub url_download_count: usize,
    pub manual_only_count: usize,
    pub installed_matches: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct ModPackSyncProgress {
    pub completed: usize,
    pub total: usize,
    pub current_mod: String,
    pub stage: String,
    pub current_bytes: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ModPackSyncReport {
    pub pack_name: String,
    pub backup_pack_name: Option<String>,
    pub total_mods: usize,
    pub already_present: usize,
    pub downloaded_workshop: usize,
    pub downloaded_other: usize,
    pub missing_manual: Vec<String>,
    pub failed_mods: Vec<(String, String)>,
    pub enabled_count: usize,
}

pub fn mod_sources_path(paths: &LauncherPaths) -> PathBuf {
    paths.tml_mods_dir().join("mod_sources.json")
}

pub fn load_mod_sources(paths: &LauncherPaths) -> Vec<ModSourceRecord> {
    let path = mod_sources_path(paths);
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(raw.trim_start_matches('\u{feff}')).unwrap_or_default()
}

pub fn upsert_mod_source(
    paths: &LauncherPaths,
    internal_name: &str,
    display_name: &str,
    mod_version: &str,
    sha1_hex: &str,
    source: SharedModSource,
) -> LauncherResult<Vec<ModSourceRecord>> {
    let clean_name = validate_internal_name(internal_name)?;
    validate_mod_source(&source, clean_name)?;

    fs::create_dir_all(paths.tml_mods_dir())?;
    let mut records = load_mod_sources(paths);
    let updated_at = Utc::now().timestamp().max(0) as u64;

    let matching_prev: Vec<&ModSourceRecord> = records
        .iter()
        .rev()
        .filter(|r| r.internal_name.eq_ignore_ascii_case(clean_name))
        .collect();

    let resolved_display = pick_non_empty(display_name)
        .or_else(|| {
            matching_prev
                .iter()
                .find_map(|p| pick_non_empty(&p.display_name))
        })
        .unwrap_or_default();
    let resolved_version = pick_non_empty(mod_version)
        .or_else(|| {
            matching_prev
                .iter()
                .find_map(|p| pick_non_empty(&p.mod_version))
        })
        .unwrap_or_default();
    let resolved_sha1 = pick_non_empty(sha1_hex)
        .or_else(|| {
            matching_prev
                .iter()
                .find_map(|p| pick_non_empty(&p.sha1_hex))
        })
        .unwrap_or_default();

    let record = ModSourceRecord {
        internal_name: clean_name.to_string(),
        display_name: resolved_display,
        source,
        mod_version: resolved_version,
        sha1_hex: resolved_sha1,
        updated_at,
    };

    records.retain(|r| !r.internal_name.eq_ignore_ascii_case(clean_name));
    records.push(record);

    records.sort_by(|a, b| {
        a.internal_name
            .to_lowercase()
            .cmp(&b.internal_name.to_lowercase())
            .then_with(|| a.internal_name.cmp(&b.internal_name))
    });

    let json = serde_json::to_string_pretty(&records)?;
    write_atomic(&mod_sources_path(paths), json.as_bytes())?;
    Ok(records)
}

pub fn remove_mod_source(
    paths: &LauncherPaths,
    internal_name: &str,
) -> LauncherResult<Vec<ModSourceRecord>> {
    let clean_name = internal_name.trim();
    let mut records = load_mod_sources(paths);
    let before = records.len();
    records.retain(|r| !r.internal_name.eq_ignore_ascii_case(clean_name));

    let path = mod_sources_path(paths);
    if records.len() != before && path.exists() {
        let json = serde_json::to_string_pretty(&records)?;
        write_atomic(&path, json.as_bytes())?;
    }

    Ok(records)
}

pub fn parse_mod_source_input(raw: &str) -> LauncherResult<SharedModSource> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return Err(LauncherError::InvalidInput(
            "Nguồn mod không được để trống hoặc chứa ký tự điều khiển".to_string(),
        ));
    }

    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Ok(SharedModSource::Workshop {
            id: trimmed.to_string(),
        });
    }

    if let Some((_, after_scheme)) = split_http_scheme(trimmed) {
        let (host, rest) = split_host_and_rest(after_scheme);
        let host_no_port = host.split(':').next().unwrap_or(host);
        if host_no_port.is_empty() {
            return Err(LauncherError::InvalidInput(
                "URL nguồn mod thiếu tên miền".to_string(),
            ));
        }
        if trimmed.chars().any(char::is_whitespace) {
            return Err(LauncherError::InvalidInput(
                "URL nguồn mod không được chứa khoảng trắng".to_string(),
            ));
        }

        if is_steamcommunity_host(host_no_port) {
            if let Some(id) = parse_workshop_id(trimmed) {
                return Ok(SharedModSource::Workshop { id });
            }
            return Err(LauncherError::InvalidInput(
                "URL Steam Workshop không chứa Workshop ID hợp lệ".to_string(),
            ));
        }

        let path_and_query = rest.split('#').next().unwrap_or(rest);
        let (path, _) = path_and_query.split_once('?').unwrap_or((path_and_query, ""));

        if is_github_host(host_no_port) {
            if let Some(repo) = parse_github_release_url(trimmed) {
                if path.to_ascii_lowercase().ends_with(".tmod") {
                    return Ok(SharedModSource::Github {
                        repo,
                        url: trimmed.to_string(),
                    });
                }
                return Err(LauncherError::InvalidInput(
                    "URL release GitHub phải trỏ tới file .tmod".to_string(),
                ));
            }

            if let Some(repo) = parse_github_repo_path(path) {
                return Ok(SharedModSource::Github {
                    repo,
                    url: String::new(),
                });
            }
        }

        if !path.is_empty() && path.to_ascii_lowercase().ends_with(".tmod") {
            return Ok(SharedModSource::Url {
                url: trimmed.to_string(),
            });
        }

        return Err(LauncherError::InvalidInput(
            "URL phải là link Steam Workshop, repo GitHub hoặc link trực tiếp tới file .tmod"
                .to_string(),
        ));
    }

    if !trimmed.contains("://") && !trimmed.chars().any(char::is_whitespace) {
        let bare_steam = trimmed.trim_start_matches("www.");
        if bare_steam
            .to_ascii_lowercase()
            .starts_with("steamcommunity.com/")
        {
            if let Some(id) = parse_workshop_id(trimmed) {
                return Ok(SharedModSource::Workshop { id });
            }
        }

        if !trimmed.to_ascii_lowercase().ends_with(".tmod") && is_valid_github_repo(trimmed) {
            return Ok(SharedModSource::Github {
                repo: trimmed.to_string(),
                url: String::new(),
            });
        }
    }

    Err(LauncherError::InvalidInput(
        "Không nhận diện được nguồn mod (hỗ trợ Workshop ID/URL, GitHub owner/repo hoặc link .tmod)"
            .to_string(),
    ))
}

pub fn resolve_mod_source_for_meta(
    meta: &TmodMetadata,
    mod_sources: &[ModSourceRecord],
    workshop_records: &[WorkshopManifestRecord],
    staged: &[StagedWorkshopMod],
) -> SharedModSource {
    if !meta.internal_name.trim().is_empty() {
        if let Some(src) =
            resolve_recorded_source(&meta.internal_name, mod_sources, workshop_records)
        {
            return src;
        }

        if let Some(st) = staged.iter().find(|s| {
            s.meta.internal_name.eq_ignore_ascii_case(&meta.internal_name)
                && ((!meta.sha1_hex.is_empty() && s.meta.sha1_hex == meta.sha1_hex)
                    || (meta.sha1_hex.is_empty() && s.meta.version == meta.version))
                && validate_workshop_id(&s.workshop_id).is_ok()
        }) {
            return SharedModSource::Workshop {
                id: st.workshop_id.trim().to_string(),
            };
        }
    }

    extract_source_hint_from_homepage(&meta.homepage).unwrap_or(SharedModSource::ManualOnly)
}

#[allow(clippy::too_many_arguments)]
pub fn build_modpack_share_preview(
    paths: &LauncherPaths,
    pack_name: &str,
    mod_names: &[String],
    pack_dir_opt: Option<&Path>,
    installed: &[InstalledModEntry],
    staged: &[StagedWorkshopMod],
    workshop_records: &[WorkshopManifestRecord],
    mod_sources: &[ModSourceRecord],
) -> LauncherResult<ModPackSharePreview> {
    build_modpack_share_preview_with_states(
        paths,
        pack_name,
        mod_names,
        pack_dir_opt,
        installed,
        staged,
        workshop_records,
        mod_sources,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_modpack_share_preview_with_states(
    paths: &LauncherPaths,
    pack_name: &str,
    mod_names: &[String],
    pack_dir_opt: Option<&Path>,
    installed: &[InstalledModEntry],
    staged: &[StagedWorkshopMod],
    workshop_records: &[WorkshopManifestRecord],
    mod_sources: &[ModSourceRecord],
    explicit_enabled: Option<&HashMap<String, bool>>,
) -> LauncherResult<ModPackSharePreview> {
    let clean_pack_name = sanitize_modpack_name(pack_name)?;
    let pack_metas = pack_dir_opt
        .map(scan_pack_mods_dir)
        .unwrap_or_default();

    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    let mut shared_entries = Vec::new();
    let mut workshop_ids = Vec::new();
    let mut workshop_count = 0usize;
    let mut url_count = 0usize;
    let mut manual_count = 0usize;

    for raw_name in mod_names {
        let internal_name = raw_name.trim();
        let name_lower = internal_name.to_ascii_lowercase();
        if internal_name.is_empty() || !seen.insert(name_lower.clone()) {
            continue;
        }

        let enabled = match explicit_enabled {
            Some(map) => map.get(&name_lower).copied().unwrap_or(true),
            None => true,
        };

        let disk_fallback = if pack_metas.contains_key(&name_lower)
            || installed
                .iter()
                .any(|m| m.meta.internal_name.eq_ignore_ascii_case(internal_name))
            || staged
                .iter()
                .any(|s| s.meta.internal_name.eq_ignore_ascii_case(internal_name))
        {
            None
        } else {
            let candidate = paths.tml_mods_dir().join(format!("{internal_name}.tmod"));
            candidate
                .is_file()
                .then(|| parse_tmod_file(&candidate).ok())
                .flatten()
                .filter(|m| m.internal_name.eq_ignore_ascii_case(internal_name))
        };

        let installed_meta = installed
            .iter()
            .find(|m| m.meta.internal_name.eq_ignore_ascii_case(internal_name))
            .map(|m| &m.meta);
        let meta_opt = match (pack_metas.get(&name_lower), installed_meta) {
            (Some(pack_meta), Some(inst_meta)) => {
                if tl_core::compare_mod_version_strings(&inst_meta.version, &pack_meta.version)
                    == std::cmp::Ordering::Greater
                {
                    Some(inst_meta)
                } else {
                    Some(pack_meta)
                }
            }
            (Some(pack_meta), None) => Some(pack_meta),
            (None, Some(inst_meta)) => Some(inst_meta),
            (None, None) => staged
                .iter()
                .find(|s| s.meta.internal_name.eq_ignore_ascii_case(internal_name))
                .map(|s| &s.meta)
                .or(disk_fallback.as_ref()),
        };

        let export_entry = if let Some(meta) = meta_opt {
            let source = resolve_mod_source_for_meta(meta, mod_sources, workshop_records, staged);
            let canonical_name = if meta.internal_name.trim().is_empty() {
                internal_name.to_string()
            } else {
                meta.internal_name.clone()
            };
            let display_name = if meta.display_name.trim().is_empty() {
                canonical_name.clone()
            } else {
                meta.display_name.clone()
            };
            ModPackExportEntry {
                internal_name: canonical_name,
                display_name,
                version: meta.version.clone(),
                source,
                homepage: meta.homepage.clone(),
                enabled,
            }
        } else {
            let source = resolve_saved_or_workshop_source(
                internal_name,
                mod_sources,
                workshop_records,
                staged,
            )
            .unwrap_or(SharedModSource::ManualOnly);
            let (display_name, version) =
                fallback_display_and_version(internal_name, mod_sources, workshop_records);
            ModPackExportEntry {
                internal_name: internal_name.to_string(),
                display_name,
                version,
                source,
                homepage: String::new(),
                enabled,
            }
        };

        match &export_entry.source {
            SharedModSource::Workshop { id } => {
                workshop_count += 1;
                workshop_ids.push(id.clone());
            }
            SharedModSource::Github { .. } | SharedModSource::Url { .. } => {
                url_count += 1;
            }
            SharedModSource::ManualOnly => {
                manual_count += 1;
            }
        }

        let compact_display = if export_entry.display_name == export_entry.internal_name {
            String::new()
        } else {
            export_entry.display_name.clone()
        };

        shared_entries.push(SharedModEntry {
            internal_name: export_entry.internal_name.clone(),
            display_name: compact_display,
            version: export_entry.version.clone(),
            source: export_entry.source.clone(),
            enabled: export_entry.enabled,
        });
        entries.push(export_entry);
    }

    let payload = ModPackShareKeyPayload {
        v: 1,
        name: clean_pack_name.clone(),
        mods: shared_entries,
    };
    let share_key = encode_modpack_share_key(&payload)?;

    if let Some(pack_dir) = pack_dir_opt {
        if !workshop_ids.is_empty() || pack_dir.join("Mods").join("install.txt").is_file() {
            write_modpack_install_txt(pack_dir, &workshop_ids)?;
        }
    }

    Ok(ModPackSharePreview {
        pack_name: clean_pack_name,
        payload,
        share_key,
        entries,
        workshop_count,
        url_count,
        manual_count,
    })
}

pub fn inspect_share_key_against_local(
    paths: &LauncherPaths,
    raw_key: &str,
    installed: &[InstalledModEntry],
    staged: &[StagedWorkshopMod],
) -> LauncherResult<ModPackImportPreview> {
    let payload = decode_modpack_share_key(raw_key)?;
    let suggested_pack_name =
        resolve_unique_modpack_name(&paths.tml_modpacks_dir(), &payload.name)?;

    let mut already_present_count = 0usize;
    let mut workshop_download_count = 0usize;
    let mut url_download_count = 0usize;
    let mut manual_only_count = 0usize;
    let mut installed_matches = HashSet::new();

    for entry in &payload.mods {
        let is_installed = installed
            .iter()
            .any(|m| meta_matches_shared_entry(&m.meta, entry));
        let is_staged = !is_installed
            && staged
                .iter()
                .any(|s| meta_matches_shared_entry(&s.meta, entry));

        if is_installed || is_staged {
            already_present_count += 1;
            installed_matches.insert(entry.internal_name.clone());
        } else {
            match &entry.source {
                SharedModSource::Workshop { .. } => workshop_download_count += 1,
                SharedModSource::Github { .. } | SharedModSource::Url { .. } => {
                    url_download_count += 1;
                }
                SharedModSource::ManualOnly => manual_only_count += 1,
            }
        }
    }

    Ok(ModPackImportPreview {
        payload,
        suggested_pack_name,
        already_present_count,
        workshop_download_count,
        url_download_count,
        manual_only_count,
        installed_matches,
    })
}

pub async fn search_workshop_candidates_for_mod(
    display_name: &str,
    internal_name: &str,
    proxy_url: Option<&str>,
) -> LauncherResult<Vec<WorkshopItemDetails>> {
    let clean_display = strip_terraria_color_tags(display_name);
    let clean_internal = internal_name.trim();

    let primary_query = if !clean_display.is_empty() {
        clean_display.as_str()
    } else if !clean_internal.is_empty() {
        clean_internal
    } else {
        return Err(LauncherError::InvalidInput(
            "Cần tên hiển thị hoặc tên nội bộ của mod để tìm trên Workshop".to_string(),
        ));
    };

    let mut result =
        browse_workshop_mods(primary_query, WorkshopSort::MostSubscribed, 1, proxy_url).await?;

    if result.items.is_empty()
        && !clean_internal.is_empty()
        && !clean_display.eq_ignore_ascii_case(clean_internal)
    {
        result =
            browse_workshop_mods(clean_internal, WorkshopSort::MostSubscribed, 1, proxy_url).await?;
    }

    result.items.truncate(8);
    Ok(result.items)
}

#[allow(clippy::too_many_arguments)]
pub async fn sync_modpack_from_share_key(
    paths: &LauncherPaths,
    steamcmd_exe: Option<&Path>,
    proxy_url: Option<&str>,
    github_token: Option<&str>,
    active_modpack: &str,
    payload: &ModPackShareKeyPayload,
    desired_pack_name: &str,
    on_progress: impl Fn(ModPackSyncProgress) + Send + Sync + 'static,
    on_log: impl Fn(String) + Send + Sync + 'static,
) -> LauncherResult<ModPackSyncReport> {
    tl_core::validate_share_payload(payload)?;
    let desired_base = if desired_pack_name.trim().is_empty() {
        payload.name.trim()
    } else {
        desired_pack_name.trim()
    };
    let clean_desired = sanitize_modpack_name(desired_base)?;
    paths.ensure_dirs()?;

    let progress_cb = Arc::new(on_progress);
    let log_cb = Arc::new(on_log);
    let total = payload.mods.len();
    let completed = Arc::new(AtomicUsize::new(0));

    emit_progress(
        &progress_cb,
        &completed,
        total,
        "",
        "Đang kiểm tra mod cục bộ và sao lưu cấu hình hiện tại...",
    );

    let initial_installed = scan_installed_mods(paths)?;
    let initial_metas: Vec<TmodMetadata> =
        initial_installed.iter().map(|e| e.meta.clone()).collect();

    let backup_pack_name = backup_unsaved_enabled_mods_as_modpack(
        &paths.tml_modpacks_dir(),
        &paths.tml_mods_dir(),
        active_modpack,
        &initial_metas,
        "ModCu_TruocDongBo",
    )?
    .map(|p| p.name);
    if let Some(ref bname) = backup_pack_name {
        emit_log(
            &log_cb,
            format!("Đã sao lưu danh sách mod chưa lưu trước đó vào ModPack '{bname}'."),
        );
    }

    let existing_pack_match = fs::read_dir(paths.tml_modpacks_dir())
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .find_map(|entry| {
            let path = entry.path();
            if !path.is_dir() || !path.join("Mods").is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            (is_safe_single_dir_component(&name) && name.eq_ignore_ascii_case(&clean_desired))
                .then_some(name)
        });
    let reuse_in_place = !desired_pack_name.trim().is_empty()
        || (!active_modpack.trim().is_empty()
            && active_modpack.trim().eq_ignore_ascii_case(&clean_desired));
    let target_pack_name = if reuse_in_place {
        existing_pack_match.unwrap_or(clean_desired)
    } else {
        resolve_unique_modpack_name(&paths.tml_modpacks_dir(), &clean_desired)?
    };
    let staged = scan_staged_workshop_mods(paths).unwrap_or_default();
    let mut existing_sources = load_mod_sources(paths);
    let mut existing_workshop_manifest = load_workshop_manifest(paths);

    let mut already_present = 0usize;
    let mut downloaded_workshop = 0usize;
    let mut downloaded_other = 0usize;
    let mut missing_manual = Vec::new();
    let mut failed_mods: Vec<(String, String)> = Vec::new();
    let mut failed_names = HashSet::new();
    let mut resolved_internal_names: HashMap<String, String> = HashMap::new();

    let mut pending_workshop: Vec<&SharedModEntry> = Vec::new();
    let mut pending_other: Vec<&SharedModEntry> = Vec::new();
    let mut pending_manual: Vec<&SharedModEntry> = Vec::new();

    for entry in &payload.mods {
        if let Some(installed_entry) = initial_installed
            .iter()
            .find(|m| meta_matches_shared_entry(&m.meta, entry))
        {
            already_present += 1;
            if let SharedModSource::Workshop { id } = &entry.source {
                let has_manifest_record = existing_workshop_manifest.iter().any(|r| {
                    r.published_file_id == *id
                        && r.internal_name
                            .eq_ignore_ascii_case(&installed_entry.meta.internal_name)
                });
                if !has_manifest_record
                    && record_workshop_install_with_timestamp(
                        paths,
                        None,
                        file_mtime_unix(&installed_entry.meta.file_path),
                        id,
                        std::slice::from_ref(&installed_entry.meta),
                    )
                    .is_ok()
                {
                    existing_workshop_manifest = load_workshop_manifest(paths);
                    existing_sources = load_mod_sources(paths);
                }
            }
            if !matches!(entry.source, SharedModSource::ManualOnly)
                && !existing_sources.iter().any(|r| {
                    r.internal_name.eq_ignore_ascii_case(&entry.internal_name)
                        && r.source == entry.source
                })
            {
                if let Ok(updated) = upsert_mod_source(
                    paths,
                    &installed_entry.meta.internal_name,
                    &installed_entry.meta.display_name,
                    &installed_entry.meta.version,
                    &installed_entry.meta.sha1_hex,
                    entry.source.clone(),
                ) {
                    existing_sources = updated;
                }
            }
            completed.fetch_add(1, Ordering::Relaxed);
            emit_progress(
                &progress_cb,
                &completed,
                total,
                entry_label(entry),
                "Đã có sẵn trong thư mục Mods",
            );
            continue;
        }

        if let Some(staged_mod) = find_matching_staged_mod(&staged, entry) {
            emit_log(
                &log_cb,
                format!(
                    "Sao chép {} từ bộ nhớ tạm SteamCMD (ID {}).",
                    entry_label(entry),
                    staged_mod.workshop_id
                ),
            );
            let staged_ts = staged_mod
                .time_updated
                .filter(|&t| t > 0)
                .or_else(|| file_mtime_unix(&staged_mod.meta.file_path));
            match import_local_mod_files(
                std::slice::from_ref(&staged_mod.meta.file_path),
                paths,
                false,
            ) {
                Ok(imported) => {
                    let _ = record_workshop_install_with_timestamp(
                        paths,
                        None,
                        staged_ts,
                        &staged_mod.workshop_id,
                        &imported,
                    );
                    already_present += 1;
                }
                Err(e) => {
                    failed_mods.push((entry.internal_name.clone(), e.to_string()));
                    failed_names.insert(entry.internal_name.clone());
                    emit_log(
                        &log_cb,
                        format!("Lỗi sao chép {} từ staging: {e}", entry.internal_name),
                    );
                }
            }
            completed.fetch_add(1, Ordering::Relaxed);
            emit_progress(
                &progress_cb,
                &completed,
                total,
                entry_label(entry),
                "Đã lấy từ bộ nhớ tạm SteamCMD",
            );
            continue;
        }

        match &entry.source {
            SharedModSource::Workshop { .. } => pending_workshop.push(entry),
            SharedModSource::Github { .. } | SharedModSource::Url { .. } => {
                pending_other.push(entry);
            }
            SharedModSource::ManualOnly => pending_manual.push(entry),
        }
    }

    if !pending_workshop.is_empty() {
        let workshop_outcome = run_batched_workshop_sync(
            paths,
            steamcmd_exe,
            proxy_url,
            &pending_workshop,
            total,
            &completed,
            &progress_cb,
            &log_cb,
        )
        .await;

        downloaded_workshop += workshop_outcome.downloaded;
        for (name, err) in workshop_outcome.failed {
            failed_names.insert(name.clone());
            failed_mods.push((name, err));
        }
        resolved_internal_names.extend(workshop_outcome.resolved_names);
    }

    for entry in pending_other {
        let label = entry_label(entry);
        emit_progress(
            &progress_cb,
            &completed,
            total,
            label,
            "Đang tải mod từ GitHub / URL...",
        );

        let res = download_other_mod_entry(
            paths,
            proxy_url,
            github_token,
            entry,
            &completed,
            total,
            &progress_cb,
            &log_cb,
        )
        .await;
        match res {
            Ok(meta) => {
                downloaded_other += 1;
                if meta.internal_name != entry.internal_name && !meta.internal_name.is_empty() {
                    resolved_internal_names
                        .insert(entry.internal_name.clone(), meta.internal_name);
                }
            }
            Err(e) => {
                let msg = e.to_string();
                emit_log(&log_cb, format!("Lỗi tải {}: {msg}", entry.internal_name));
                failed_names.insert(entry.internal_name.clone());
                failed_mods.push((entry.internal_name.clone(), msg));
            }
        }

        completed.fetch_add(1, Ordering::Relaxed);
        emit_progress(
            &progress_cb,
            &completed,
            total,
            label,
            "Đã xử lý xong mod ngoài Workshop",
        );
    }

    let mut missing_manual_names = HashSet::new();
    for entry in pending_manual {
        if let Some(existing) = initial_installed
            .iter()
            .find(|m| m.meta.internal_name.eq_ignore_ascii_case(&entry.internal_name))
        {
            emit_log(
                &log_cb,
                format!(
                    "Thiếu mod thủ công '{}' (đang có v{}, cần v{}): cần cài file .tmod từ chủ phòng.",
                    entry.internal_name, existing.meta.version, entry.version
                ),
            );
        } else {
            emit_log(
                &log_cb,
                format!(
                    "Thiếu mod thủ công '{}': cần cài file .tmod từ chủ phòng.",
                    entry.internal_name
                ),
            );
        }
        missing_manual_names.insert(entry.internal_name.clone());
        missing_manual.push(entry.internal_name.clone());
        completed.fetch_add(1, Ordering::Relaxed);
        emit_progress(
            &progress_cb,
            &completed,
            total,
            entry_label(entry),
            "Thiếu mod thủ công",
        );
    }

    let all_installed = scan_installed_mods(paths)?;
    let all_metas: Vec<TmodMetadata> = all_installed.into_iter().map(|e| e.meta).collect();
    let installed_by_lower: HashMap<String, String> = all_metas
        .iter()
        .map(|m| (m.internal_name.to_ascii_lowercase(), m.internal_name.clone()))
        .collect();
    let failed_lower: HashSet<String> = failed_names
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let missing_manual_lower: HashSet<String> = missing_manual_names
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let resolved_lower: HashMap<String, String> = resolved_internal_names
        .into_iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v))
        .collect();

    let mut seen_enabled = HashSet::new();
    let mut pack_enabled_names = Vec::new();
    let mut workshop_ids = Vec::new();

    for entry in &payload.mods {
        let entry_lower = entry.internal_name.to_ascii_lowercase();
        if failed_lower.contains(&entry_lower) || missing_manual_lower.contains(&entry_lower) {
            continue;
        }
        if let SharedModSource::Workshop { id } = &entry.source {
            workshop_ids.push(id.clone());
        }
        if entry.enabled {
            let target_lower = resolved_lower
                .get(&entry_lower)
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or(entry_lower);
            if let Some(canonical_name) = installed_by_lower.get(&target_lower) {
                if seen_enabled.insert(canonical_name.to_ascii_lowercase()) {
                    pack_enabled_names.push(canonical_name.clone());
                }
            }
        }
    }

    let pack = create_modpack(
        &paths.tml_modpacks_dir(),
        &paths.tml_mods_dir(),
        &target_pack_name,
        &pack_enabled_names,
        &all_metas,
    )?;
    write_modpack_install_txt(&pack.path, &workshop_ids)?;
    let activated_enabled = activate_modpack(&pack.path, &paths.tml_mods_dir(), false)?;

    completed.store(total, Ordering::Relaxed);
    emit_progress(
        &progress_cb,
        &completed,
        total,
        "",
        "Hoàn tất đồng bộ ModPack",
    );

    Ok(ModPackSyncReport {
        pack_name: pack.name,
        backup_pack_name,
        total_mods: total,
        already_present,
        downloaded_workshop,
        downloaded_other,
        missing_manual,
        failed_mods,
        enabled_count: activated_enabled.len(),
    })
}

struct WorkshopBatchOutcome {
    downloaded: usize,
    failed: Vec<(String, String)>,
    resolved_names: HashMap<String, String>,
}

#[allow(clippy::too_many_arguments)]
async fn run_batched_workshop_sync(
    paths: &LauncherPaths,
    steamcmd_exe: Option<&Path>,
    proxy_url: Option<&str>,
    pending_workshop: &[&SharedModEntry],
    total: usize,
    completed: &Arc<AtomicUsize>,
    progress_cb: &Arc<impl Fn(ModPackSyncProgress) + Send + Sync + 'static>,
    log_cb: &Arc<impl Fn(String) + Send + Sync + 'static>,
) -> WorkshopBatchOutcome {
    let mut outcome = WorkshopBatchOutcome {
        downloaded: 0,
        failed: Vec::new(),
        resolved_names: HashMap::new(),
    };

    let Some(exe) = steamcmd_exe.filter(|p| p.is_file()) else {
        let reason = match steamcmd_exe {
            Some(p) => format!("Không tìm thấy steamcmd.exe tại {}", p.display()),
            None => "Chưa cài đặt SteamCMD để tải mod từ Steam Workshop".to_string(),
        };
        emit_log(log_cb, reason.clone());
        for entry in pending_workshop {
            outcome
                .failed
                .push((entry.internal_name.clone(), reason.clone()));
            completed.fetch_add(1, Ordering::Relaxed);
            emit_progress(
                progress_cb,
                completed,
                total,
                entry_label(entry),
                "Thiếu SteamCMD",
            );
        }
        return outcome;
    };

    let mut unique_ids = Vec::new();
    let mut entries_by_id: HashMap<String, Vec<&SharedModEntry>> = HashMap::new();
    for entry in pending_workshop {
        if let SharedModSource::Workshop { id } = &entry.source {
            let bucket = entries_by_id.entry(id.clone()).or_default();
            if bucket.is_empty() {
                unique_ids.push(id.clone());
            }
            bucket.push(*entry);
        }
    }

    let staging_dir = paths.steamcmd_dir.join("staging");
    if let Err(e) = fs::create_dir_all(&staging_dir) {
        let msg = format!("Không tạo được thư mục staging SteamCMD: {e}");
        emit_log(log_cb, msg.clone());
        for entry in pending_workshop {
            outcome
                .failed
                .push((entry.internal_name.clone(), msg.clone()));
            completed.fetch_add(1, Ordering::Relaxed);
        }
        return outcome;
    }

    let initial_stage = format!("Đang tải {} mod Workshop qua SteamCMD...", unique_ids.len());
    emit_log(
        log_cb,
        format!(
            "Bắt đầu tải gộp {} Workshop item qua SteamCMD...",
            unique_ids.len()
        ),
    );
    emit_progress(progress_cb, completed, total, "", &initial_stage);

    let staging_content = workshop_staging_content_dir(paths);
    let before_snapshots: HashMap<String, TmodFileSnapshot> = unique_ids
        .iter()
        .map(|wid| (wid.clone(), snapshot_tmod_files(&staging_content.join(wid))))
        .collect();

    let content_log_path = steamcmd_content_log_path(exe);
    let initial_log_offset = content_log_path
        .as_deref()
        .map(current_file_len)
        .unwrap_or(0);

    let mut cmd = Command::new(exe);
    cmd.arg("+force_install_dir")
        .arg(&staging_dir)
        .arg("+login")
        .arg("anonymous");
    for wid in &unique_ids {
        cmd.arg("+workshop_download_item")
            .arg(TMODLOADER_STEAM_APP_ID.to_string())
            .arg(wid);
    }
    cmd.arg("+quit");
    apply_proxy_env(&mut cmd, proxy_url);

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("Không thể chạy steamcmd.exe: {e}");
            emit_log(log_cb, msg.clone());
            for entry in pending_workshop {
                outcome
                    .failed
                    .push((entry.internal_name.clone(), msg.clone()));
                completed.fetch_add(1, Ordering::Relaxed);
            }
            return outcome;
        }
    };

    let child_pid = child.id();
    let counts_by_id: HashMap<String, usize> = entries_by_id
        .iter()
        .map(|(k, v)| (k.clone(), v.len()))
        .collect();
    let labels_by_id: HashMap<String, String> = entries_by_id
        .iter()
        .map(|(k, v)| (k.clone(), entry_label(v[0]).to_string()))
        .collect();
    let live_counted = Arc::new(Mutex::new(HashSet::<String>::new()));
    let active_wid = Arc::new(Mutex::new(None::<String>));
    let tracker = Arc::new(Mutex::new({
        let mut trk = SteamContentTelemetryTracker::new(&initial_stage, None);
        trk.mark_emitted(None, &initial_stage, 0, None);
        trk
    }));

    let stdout_task = child.stdout.take().map(|stdout| {
        spawn_steamcmd_reader(
            stdout,
            total,
            Arc::clone(completed),
            Arc::clone(progress_cb),
            Arc::clone(log_cb),
            counts_by_id.clone(),
            labels_by_id.clone(),
            Arc::clone(&live_counted),
            Arc::clone(&active_wid),
            Arc::clone(&tracker),
        )
    });
    let stderr_task = child.stderr.take().map(|stderr| {
        spawn_steamcmd_reader(
            stderr,
            total,
            Arc::clone(completed),
            Arc::clone(progress_cb),
            Arc::clone(log_cb),
            counts_by_id,
            labels_by_id.clone(),
            Arc::clone(&live_counted),
            Arc::clone(&active_wid),
            Arc::clone(&tracker),
        )
    });

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_notify = Arc::new(Notify::new());
    let monitor_task = {
        let stop_flag = Arc::clone(&stop_flag);
        let stop_notify = Arc::clone(&stop_notify);
        let tracker = Arc::clone(&tracker);
        let active_wid = Arc::clone(&active_wid);
        let progress_cb = Arc::clone(progress_cb);
        let completed = Arc::clone(completed);
        tokio::spawn(async move {
            let mut sys = sysinfo::System::new().ok();
            let mut log_offset = initial_log_offset;
            let mut last_seen_wid: Option<String> = None;
            let mut ticker = tokio::time::interval(Duration::from_millis(200));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        if stop_flag.load(Ordering::Acquire) {
                            break;
                        }
                        let cur_wid = active_wid.lock().ok().and_then(|g| g.clone());
                        let lines = content_log_path
                            .as_deref()
                            .map(|p| read_new_complete_lines(p, &mut log_offset))
                            .unwrap_or_default();
                        let written = sys
                            .as_mut()
                            .zip(child_pid)
                            .and_then(|(s, pid)| read_process_written_bytes(s, pid));
                        let changed = tracker.lock().ok().and_then(|mut trk| {
                            if cur_wid != last_seen_wid {
                                if last_seen_wid.is_some()
                                    && cur_wid.is_some()
                                    && last_seen_wid != cur_wid
                                {
                                    trk.reset_for_new_item("Đang kiểm tra gói Workshop...");
                                }
                                last_seen_wid = cur_wid.clone();
                            }
                            trk.apply_telemetry(&lines, written);
                            trk.take_changed_for_key(cur_wid.as_deref())
                        });
                        if let Some((stage, current_bytes, total_bytes)) = changed {
                            let current_mod = active_wid_label(cur_wid.as_deref(), &labels_by_id);
                            emit_progress_bytes(
                                &progress_cb,
                                &completed,
                                total,
                                current_mod,
                                &stage,
                                current_bytes,
                                total_bytes,
                            );
                        }
                    }
                    _ = stop_notify.notified() => {
                        break;
                    }
                }
            }
        })
    };

    let wait_res = child.wait().await;
    stop_flag.store(true, Ordering::Release);
    stop_notify.notify_waiters();
    let _ = monitor_task.await;
    if let Some(h) = stdout_task {
        let _ = h.await;
    }
    if let Some(h) = stderr_task {
        let _ = h.await;
    }

    let (status_success, exit_code) = match wait_res {
        Ok(status) => (status.success(), status.code()),
        Err(e) => {
            let msg = format!("Lỗi chờ steamcmd.exe: {e}");
            emit_log(log_cb, msg.clone());
            for entry in pending_workshop {
                outcome
                    .failed
                    .push((entry.internal_name.clone(), msg.clone()));
            }
            return outcome;
        }
    };

    let mut details_vec: Vec<WorkshopItemDetails> = unique_ids
        .iter()
        .map(|id| WorkshopItemDetails {
            published_file_id: id.clone(),
            ..Default::default()
        })
        .collect();
    if let Ok(client) = build_http_client(proxy_url) {
        enrich_workshop_items_batch(&client, &mut details_vec).await;
    }
    let details_by_id: HashMap<String, WorkshopItemDetails> = details_vec
        .into_iter()
        .map(|d| (d.published_file_id.clone(), d))
        .collect();

    let counted_snapshot = live_counted
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();

    let target_tml_ym = detect_installed_tml_year_month(paths);
    for wid in unique_ids {
        let entries = &entries_by_id[&wid];
        let item_dir = staging_content.join(&wid);
        let wid_dir_changed = before_snapshots
            .get(&wid)
            .is_some_and(|before| has_any_tmod_changed_or_new(&item_dir, before));
        let wid_downloaded = should_accept_workshop_download_for_batch(
            &counted_snapshot,
            &wid,
            status_success,
            wid_dir_changed,
        );

        if !wid_downloaded {
            let msg = format!(
                "SteamCMD không tải được Workshop ID {wid} (mã thoát {exit_code:?})."
            );
            emit_log(log_cb, msg.clone());
            for entry in entries {
                outcome
                    .failed
                    .push((entry.internal_name.clone(), msg.clone()));
            }
            if !counted_snapshot.contains(&wid) {
                completed.fetch_add(entries.len(), Ordering::Relaxed);
                emit_progress(
                    progress_cb,
                    completed,
                    total,
                    entry_label(entries[0]),
                    &format!("Thất bại Workshop ID {wid}"),
                );
            }
            continue;
        }

        let mut found_tmods = Vec::new();
        collect_tmods_recursive(&item_dir, &mut found_tmods);

        for entry in entries {
            let best_tmod = pick_tmod_for_entry(&found_tmods, entry, &item_dir, target_tml_ym)
                .or_else(|| {
                    let picked = pick_tmod_name_fallback_for_entry(
                        &found_tmods,
                        entry,
                        &item_dir,
                        target_tml_ym,
                    )?;
                    if let Ok(picked_meta) = parse_tmod_file(&picked) {
                        emit_log(
                            log_cb,
                            format!(
                                "Lưu ý: Workshop ID {wid} tải về '{}' v{} (khác với phiên bản v{} trong mã đồng bộ).",
                                picked_meta.internal_name,
                                picked_meta.version,
                                entry.version.trim()
                            ),
                        );
                    }
                    Some(picked)
                });
            if let Some(tmod_path) = best_tmod {
                match import_local_mod_files(&[tmod_path], paths, false) {
                    Ok(imported) => {
                        let details_opt = details_by_id.get(&wid);
                        if let Err(e) =
                            record_workshop_install(paths, details_opt, &wid, &imported)
                        {
                            emit_log(
                                log_cb,
                                format!("Cảnh báo lưu metadata Workshop {wid}: {e}"),
                            );
                        }
                        if let Some(first) = imported.first() {
                            if !imported
                                .iter()
                                .any(|m| m.internal_name.eq_ignore_ascii_case(&entry.internal_name))
                                && !first.internal_name.is_empty()
                            {
                                outcome.resolved_names.insert(
                                    entry.internal_name.clone(),
                                    first.internal_name.clone(),
                                );
                            }
                        }
                        outcome.downloaded += 1;
                    }
                    Err(e) => {
                        let msg = format!("Lỗi cài .tmod Workshop {wid}: {e}");
                        emit_log(log_cb, msg.clone());
                        outcome.failed.push((entry.internal_name.clone(), msg));
                    }
                }
            } else {
                let msg = format!(
                    "SteamCMD kết thúc ({exit_code:?}) nhưng không tìm thấy .tmod hợp lệ cho '{}' (Workshop ID {wid})",
                    entry.internal_name
                );
                emit_log(log_cb, msg.clone());
                outcome.failed.push((entry.internal_name.clone(), msg));
            }
        }

        if !counted_snapshot.contains(&wid) {
            completed.fetch_add(entries.len(), Ordering::Relaxed);
            emit_progress(
                progress_cb,
                completed,
                total,
                entry_label(entries[0]),
                &format!("Đã kiểm tra Workshop ID {wid}"),
            );
        }
    }

    outcome
}

fn should_accept_workshop_download_for_batch(
    counted_snapshot: &HashSet<String>,
    wid: &str,
    status_success: bool,
    wid_dir_changed: bool,
) -> bool {
    counted_snapshot.contains(wid) || (status_success && wid_dir_changed)
}

fn active_wid_label<'a>(wid: Option<&'a str>, labels_by_id: &'a HashMap<String, String>) -> &'a str {
    match wid {
        Some(id) => labels_by_id.get(id).map(String::as_str).unwrap_or(id),
        None => "Steam Workshop",
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_steamcmd_reader<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    stream: R,
    total: usize,
    completed: Arc<AtomicUsize>,
    progress_cb: Arc<impl Fn(ModPackSyncProgress) + Send + Sync + 'static>,
    log_cb: Arc<impl Fn(String) + Send + Sync + 'static>,
    counts_by_id: HashMap<String, usize>,
    labels_by_id: HashMap<String, String>,
    live_counted: Arc<Mutex<HashSet<String>>>,
    active_wid: Arc<Mutex<Option<String>>>,
    tracker: Arc<Mutex<SteamContentTelemetryTracker>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            emit_log(&log_cb, trimmed.to_string());

            if let Some(wid) = extract_steamcmd_downloading_item_id(trimmed) {
                let prev_wid = active_wid
                    .lock()
                    .map(|mut g| g.replace(wid.clone()))
                    .unwrap_or(None);
                if prev_wid.as_deref() != Some(wid.as_str()) {
                    if let Ok(mut trk) = tracker.lock() {
                        if prev_wid.is_some()
                            || matches!(
                                trk.phase(),
                                SteamWorkshopPhase::Connecting
                                    | SteamWorkshopPhase::Reconfiguring
                                    | SteamWorkshopPhase::Finished
                            )
                        {
                            trk.reset_for_new_item("Đang kiểm tra gói Workshop...");
                            trk.mark_emitted(
                                Some(&wid),
                                "Đang kiểm tra gói Workshop...",
                                0,
                                None,
                            );
                        }
                    }
                    let label = labels_by_id
                        .get(&wid)
                        .map(String::as_str)
                        .unwrap_or(&wid);
                    emit_progress(
                        &progress_cb,
                        &completed,
                        total,
                        label,
                        "Đang kiểm tra gói Workshop...",
                    );
                }
            } else if let Some(wid) = extract_steamcmd_downloaded_item_id(trimmed) {
                if let Ok(mut guard) = active_wid.lock() {
                    if guard.as_deref() == Some(wid.as_str()) {
                        *guard = None;
                        if let Ok(mut trk) = tracker.lock() {
                            trk.reset_idle("Đang xử lý Workshop...");
                            trk.mark_emitted(
                                None,
                                &format!("Đã tải xong Workshop item {wid}"),
                                0,
                                None,
                            );
                        }
                    }
                }
                if let Some(&count) = counts_by_id.get(&wid) {
                    let newly_counted = live_counted
                        .lock()
                        .map(|mut set| set.insert(wid.clone()))
                        .unwrap_or(false);
                    if newly_counted {
                        completed.fetch_add(count, Ordering::Relaxed);
                        let label = labels_by_id
                            .get(&wid)
                            .map(String::as_str)
                            .unwrap_or(&wid);
                        emit_progress(
                            &progress_cb,
                            &completed,
                            total,
                            label,
                            &format!("Đã tải xong Workshop item {wid}"),
                        );
                    }
                }
            } else {
                let cur_wid = active_wid.lock().ok().and_then(|g| g.clone());
                let changed = tracker.lock().ok().and_then(|mut trk| {
                    trk.apply_stdout_line(trimmed)
                        .then(|| trk.take_changed_for_key(cur_wid.as_deref()))
                        .flatten()
                });
                if let Some((stage, current_bytes, total_bytes)) = changed {
                    let current_mod = active_wid_label(cur_wid.as_deref(), &labels_by_id);
                    emit_progress_bytes(
                        &progress_cb,
                        &completed,
                        total,
                        current_mod,
                        &stage,
                        current_bytes,
                        total_bytes,
                    );
                }
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
async fn download_other_mod_entry(
    paths: &LauncherPaths,
    proxy_url: Option<&str>,
    github_token: Option<&str>,
    entry: &SharedModEntry,
    completed: &Arc<AtomicUsize>,
    total: usize,
    progress_cb: &Arc<impl Fn(ModPackSyncProgress) + Send + Sync + 'static>,
    log_cb: &Arc<impl Fn(String) + Send + Sync + 'static>,
) -> LauncherResult<TmodMetadata> {
    let suggested = format!("{}.tmod", entry.internal_name);
    let make_progress_cb = || {
        let pcb = Arc::clone(progress_cb);
        let comp = Arc::clone(completed);
        let lbl = entry_label(entry).to_string();
        move |prog: ModDownloadProgress| {
            emit_progress_bytes(
                &pcb,
                &comp,
                total,
                &lbl,
                &prog.stage,
                prog.current_bytes,
                prog.total_bytes,
            );
        }
    };
    match &entry.source {
        SharedModSource::Github { repo, url } => {
            if !url.trim().is_empty() {
                emit_log(
                    log_cb,
                    format!("Đang tải {} từ GitHub release asset...", entry.internal_name),
                );
                match download_and_install_tmod_url_with_auth_and_progress(
                    url.trim(),
                    None,
                    github_token,
                    Some(&suggested),
                    Some(repo),
                    None,
                    paths,
                    proxy_url,
                    false,
                    make_progress_cb(),
                )
                .await
                {
                    Ok(meta) => return Ok(meta),
                    Err(e) => {
                        emit_log(
                            log_cb,
                            format!(
                                "Tải URL GitHub ghim thất bại ({e}), thử lấy bản mới nhất từ {repo}..."
                            ),
                        );
                    }
                }
            }

            let assets = fetch_github_latest_tmod(repo, proxy_url, github_token).await?;
            let chosen = pick_github_asset_for_mod(&assets, &entry.internal_name).ok_or_else(
                || {
                    LauncherError::NotFound(format!(
                        "Release mới nhất của {repo} có nhiều file .tmod nhưng không khớp '{}'",
                        entry.internal_name
                    ))
                },
            )?;
            emit_log(
                log_cb,
                format!(
                    "Đang tải {} ({}) từ {repo}...",
                    chosen.asset_name, chosen.tag_name
                ),
            );
            download_and_install_tmod_url_with_auth_and_progress(
                &chosen.download_url,
                chosen.api_asset_url.as_deref(),
                github_token,
                Some(&chosen.asset_name),
                Some(repo),
                (chosen.size_bytes > 0).then_some(chosen.size_bytes),
                paths,
                proxy_url,
                false,
                make_progress_cb(),
            )
            .await
        }
        SharedModSource::Url { url } => {
            emit_log(
                log_cb,
                format!("Đang tải {} từ URL trực tiếp...", entry.internal_name),
            );
            download_and_install_tmod_url_with_source_and_progress(
                url.trim(),
                Some(&suggested),
                None,
                None,
                paths,
                proxy_url,
                false,
                make_progress_cb(),
            )
            .await
        }
        _ => Err(LauncherError::InvalidInput(format!(
            "Nguồn không phải GitHub/URL cho mod '{}'",
            entry.internal_name
        ))),
    }
}

fn pick_github_asset_for_mod<'a>(
    assets: &'a [RemoteTmodAsset],
    internal_name: &str,
) -> Option<&'a RemoteTmodAsset> {
    let target_exact = format!("{internal_name}.tmod");
    if let Some(exact) = assets
        .iter()
        .find(|a| a.asset_name.eq_ignore_ascii_case(&target_exact))
    {
        return Some(exact);
    }

    let lower_name = internal_name.to_ascii_lowercase();
    if let Some(prefixed) = assets.iter().find(|a| {
        let stem = a
            .asset_name
            .strip_suffix(".tmod")
            .or_else(|| a.asset_name.strip_suffix(".TMOD"))
            .unwrap_or(&a.asset_name)
            .to_ascii_lowercase();
        stem == lower_name
            || stem.starts_with(&format!("{lower_name}-"))
            || stem.starts_with(&format!("{lower_name}_"))
            || stem.starts_with(&format!("{lower_name}."))
    }) {
        return Some(prefixed);
    }

    if let Some(contains) = assets
        .iter()
        .find(|a| a.asset_name.to_ascii_lowercase().contains(&lower_name))
    {
        return Some(contains);
    }

    if assets.len() == 1 {
        return assets.first();
    }

    None
}

fn pick_matching_tmod<F>(
    candidates: &[PathBuf],
    stop_dir: &Path,
    target_tml_ym: (u32, u32),
    predicate: F,
) -> Option<PathBuf>
where
    F: Fn(&TmodMetadata) -> bool,
{
    let matching: Vec<PathBuf> = candidates
        .iter()
        .filter(|p| parse_tmod_file(p).map(|m| predicate(&m)).unwrap_or(false))
        .cloned()
        .collect();
    if matching.is_empty() {
        return None;
    }
    pick_best_workshop_tmods(&matching, stop_dir, target_tml_ym)
        .into_iter()
        .next()
}

fn pick_tmod_for_entry(
    candidates: &[PathBuf],
    entry: &SharedModEntry,
    stop_dir: &Path,
    target_tml_ym: (u32, u32),
) -> Option<PathBuf> {
    let req_ver = entry.version.trim();
    let is_workshop = matches!(entry.source, SharedModSource::Workshop { .. });
    pick_matching_tmod(candidates, stop_dir, target_tml_ym, |m| {
        if !is_workshop {
            meta_matches_shared_entry(m, entry)
        } else {
            m.internal_name.eq_ignore_ascii_case(&entry.internal_name)
                && (req_ver.is_empty()
                    || tl_core::compare_mod_version_strings(&m.version, req_ver)
                        != std::cmp::Ordering::Less)
        }
    })
}

fn pick_tmod_name_fallback_for_entry(
    candidates: &[PathBuf],
    entry: &SharedModEntry,
    stop_dir: &Path,
    target_tml_ym: (u32, u32),
) -> Option<PathBuf> {
    pick_matching_tmod(candidates, stop_dir, target_tml_ym, |m| {
        m.internal_name.eq_ignore_ascii_case(&entry.internal_name)
    })
}

fn find_matching_staged_mod<'a>(
    staged: &'a [StagedWorkshopMod],
    entry: &SharedModEntry,
) -> Option<&'a StagedWorkshopMod> {
    match &entry.source {
        SharedModSource::Workshop { id } => staged
            .iter()
            .find(|s| s.workshop_id == *id && meta_matches_shared_entry(&s.meta, entry)),
        _ => staged
            .iter()
            .find(|s| meta_matches_shared_entry(&s.meta, entry)),
    }
}

fn meta_matches_shared_entry(meta: &TmodMetadata, entry: &SharedModEntry) -> bool {
    if !meta.internal_name.eq_ignore_ascii_case(&entry.internal_name) {
        return false;
    }
    let req_ver = entry.version.trim();
    req_ver.is_empty() || meta.version.trim() == req_ver
}

fn resolve_recorded_source(
    internal_name: &str,
    mod_sources: &[ModSourceRecord],
    workshop_records: &[WorkshopManifestRecord],
) -> Option<SharedModSource> {
    if let Some(rec) = mod_sources
        .iter()
        .rev()
        .find(|r| r.internal_name.eq_ignore_ascii_case(internal_name))
    {
        return Some(rec.source.clone());
    }

    if let Some(rec) = workshop_records.iter().rev().find(|r| {
        r.internal_name.eq_ignore_ascii_case(internal_name)
            && validate_workshop_id(&r.published_file_id).is_ok()
    }) {
        return Some(SharedModSource::Workshop {
            id: rec.published_file_id.trim().to_string(),
        });
    }

    None
}

fn resolve_saved_or_workshop_source(
    internal_name: &str,
    mod_sources: &[ModSourceRecord],
    workshop_records: &[WorkshopManifestRecord],
    staged: &[StagedWorkshopMod],
) -> Option<SharedModSource> {
    if let Some(src) = resolve_recorded_source(internal_name, mod_sources, workshop_records) {
        return Some(src);
    }

    if let Some(st) = staged.iter().find(|s| {
        s.meta.internal_name.eq_ignore_ascii_case(internal_name)
            && validate_workshop_id(&s.workshop_id).is_ok()
    }) {
        return Some(SharedModSource::Workshop {
            id: st.workshop_id.trim().to_string(),
        });
    }

    None
}

fn fallback_display_and_version(
    internal_name: &str,
    mod_sources: &[ModSourceRecord],
    workshop_records: &[WorkshopManifestRecord],
) -> (String, String) {
    if let Some(rec) = mod_sources
        .iter()
        .rev()
        .find(|r| r.internal_name.eq_ignore_ascii_case(internal_name))
    {
        let display = pick_non_empty(&rec.display_name).unwrap_or_else(|| internal_name.to_string());
        let version = pick_non_empty(&rec.mod_version).unwrap_or_default();
        return (display, version);
    }

    if let Some(rec) = workshop_records
        .iter()
        .rev()
        .find(|r| r.internal_name.eq_ignore_ascii_case(internal_name))
    {
        let display = pick_non_empty(&rec.display_name).unwrap_or_else(|| internal_name.to_string());
        let version = pick_non_empty(&rec.mod_version).unwrap_or_default();
        return (display, version);
    }

    (internal_name.to_string(), String::new())
}

fn scan_pack_mods_dir(pack_dir: &Path) -> HashMap<String, TmodMetadata> {
    let mut map = HashMap::new();
    let pack_mods = pack_dir.join("Mods");
    let Ok(entries) = fs::read_dir(pack_mods) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("tmod"))
            .unwrap_or(false)
        {
            if let Ok(meta) = parse_tmod_file(&path) {
                if !meta.internal_name.is_empty() {
                    map.insert(meta.internal_name.to_ascii_lowercase(), meta);
                }
            }
        }
    }
    map
}

pub(crate) fn strip_terraria_color_tags(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;

    while let Some(open_idx) = rest.find('[') {
        out.push_str(&rest[..open_idx]);
        let after_open = &rest[open_idx + 1..];
        let is_color_tag = after_open.len() >= 2
            && (after_open.starts_with("c/") || after_open.starts_with("C/"));

        if is_color_tag {
            if let Some(close_rel) = after_open.find(']') {
                let tag_body = &after_open[2..close_rel];
                if let Some((hex, inner)) = tag_body.split_once(':') {
                    if !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                        out.push_str(inner);
                        rest = &after_open[close_rel + 1..];
                        continue;
                    }
                }
            }
        }

        out.push('[');
        rest = after_open;
    }

    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn extract_steamcmd_downloaded_item_id(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let needle = "downloaded item ";
    let idx = lower.find(needle)?;
    let after = &line[idx + needle.len()..];
    let digits: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    (!digits.is_empty()).then_some(digits)
}

pub(crate) fn is_valid_github_repo(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    !name.contains('/') && is_valid_github_segment(owner) && is_valid_github_segment(name)
}

pub(crate) fn parse_github_release_url(url: &str) -> Option<String> {
    let (_, after_scheme) = split_http_scheme(url)?;
    let (host, rest) = split_host_and_rest(after_scheme);
    let host_no_port = host.split(':').next().unwrap_or(host);
    if !is_github_host(host_no_port) {
        return None;
    }

    let path_and_query = rest.split('#').next().unwrap_or(rest);
    let (path, _) = path_and_query.split_once('?').unwrap_or((path_and_query, ""));
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    if segments.len() < 6 {
        return None;
    }
    let owner = segments[0];
    let repo = segments[1];
    if !is_valid_github_segment(owner) || !is_valid_github_segment(repo) {
        return None;
    }
    if !segments[2].eq_ignore_ascii_case("releases")
        || !segments[3].eq_ignore_ascii_case("download")
        || segments[4].is_empty()
        || segments[5..].iter().any(|s| s.is_empty())
    {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn parse_github_repo_path(path: &str) -> Option<String> {
    let trimmed = path.trim_matches('/');
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return None;
    }
    let owner = segments[0];
    let repo = segments[1];
    if !is_valid_github_segment(owner) || !is_valid_github_segment(repo) {
        return None;
    }
    let valid_tail = match segments.len() {
        2 => true,
        3 => segments[2].eq_ignore_ascii_case("releases"),
        4 => {
            segments[2].eq_ignore_ascii_case("releases")
                && segments[3].eq_ignore_ascii_case("latest")
        }
        _ => false,
    };
    valid_tail.then(|| format!("{owner}/{repo}"))
}

fn validate_internal_name(raw: &str) -> LauncherResult<&str> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.contains(['/', '\\'])
        || trimmed.contains("..")
        || trimmed.chars().any(char::is_control)
    {
        return Err(LauncherError::InvalidInput(
            "Tên nội bộ của mod không hợp lệ".to_string(),
        ));
    }
    Ok(trimmed)
}

fn validate_mod_source(source: &SharedModSource, internal_name: &str) -> LauncherResult<()> {
    match source {
        SharedModSource::Workshop { id } => {
            validate_workshop_id(id)?;
        }
        SharedModSource::Github { repo, url } => {
            if !is_valid_github_repo(repo) {
                return Err(LauncherError::InvalidInput(format!(
                    "Repo GitHub không hợp lệ cho mod '{internal_name}'"
                )));
            }
            if !url.is_empty() && !is_valid_http_url(url) {
                return Err(LauncherError::InvalidInput(format!(
                    "URL phát hành GitHub không hợp lệ cho mod '{internal_name}'"
                )));
            }
        }
        SharedModSource::Url { url } => {
            if !is_valid_http_url(url) {
                return Err(LauncherError::InvalidInput(format!(
                    "URL tải mod không hợp lệ cho mod '{internal_name}'"
                )));
            }
        }
        SharedModSource::ManualOnly => {}
    }
    Ok(())
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

fn is_github_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("www.github.com")
}

fn is_valid_http_url(url: &str) -> bool {
    if url.chars().any(char::is_control) || url.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((_, after_scheme)) = split_http_scheme(url) else {
        return false;
    };
    let (host, _) = split_host_and_rest(after_scheme);
    !host.is_empty()
}

fn pick_non_empty(s: &str) -> Option<String> {
    let trimmed = s.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn entry_label(entry: &SharedModEntry) -> &str {
    if entry.display_name.trim().is_empty() {
        &entry.internal_name
    } else {
        &entry.display_name
    }
}

fn emit_log(cb: &Arc<impl Fn(String) + Send + Sync + 'static>, msg: String) {
    cb(msg);
}

fn emit_progress(
    cb: &Arc<impl Fn(ModPackSyncProgress) + Send + Sync + 'static>,
    completed: &Arc<AtomicUsize>,
    total: usize,
    current_mod: &str,
    stage: &str,
) {
    emit_progress_bytes(cb, completed, total, current_mod, stage, 0, None);
}

fn emit_progress_bytes(
    cb: &Arc<impl Fn(ModPackSyncProgress) + Send + Sync + 'static>,
    completed: &Arc<AtomicUsize>,
    total: usize,
    current_mod: &str,
    stage: &str,
    current_bytes: u64,
    total_bytes: Option<u64>,
) {
    cb(ModPackSyncProgress {
        completed: completed.load(Ordering::Relaxed).min(total),
        total,
        current_mod: current_mod.to_string(),
        stage: stage.to_string(),
        current_bytes,
        total_bytes,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tl_core::{load_enabled_mods, save_enabled_mods, ModSide};

    fn make_temp_paths(label: &str) -> LauncherPaths {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base_dir = std::env::temp_dir().join(format!(
            "tl_runtime_sync_{label}_{}_{}",
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

    fn build_tmod_bytes(
        internal_name: &str,
        display_name: &str,
        version: &str,
        homepage: &str,
    ) -> Vec<u8> {
        let mut info = Vec::new();
        write_dotnet_str(&mut info, "displayName");
        write_dotnet_str(&mut info, display_name);
        write_dotnet_str(&mut info, "author");
        write_dotnet_str(&mut info, "ModAuthor");
        if !homepage.is_empty() {
            write_dotnet_str(&mut info, "homepage");
            write_dotnet_str(&mut info, homepage);
        }
        write_dotnet_str(&mut info, "");

        let mut tmod = Vec::new();
        tmod.extend_from_slice(b"TMOD");
        write_dotnet_str(&mut tmod, "2026.9.3.1");
        tmod.extend_from_slice(&[0x22u8; 20]);
        tmod.extend_from_slice(&[0u8; 256]);
        tmod.extend_from_slice(&128i32.to_le_bytes());
        write_dotnet_str(&mut tmod, internal_name);
        write_dotnet_str(&mut tmod, version);
        tmod.extend_from_slice(&1i32.to_le_bytes());
        write_dotnet_str(&mut tmod, "Info");
        tmod.extend_from_slice(&(info.len() as i32).to_le_bytes());
        tmod.extend_from_slice(&(info.len() as i32).to_le_bytes());
        tmod.extend_from_slice(&info);
        tmod
    }

    fn write_tmod_file(
        path: &Path,
        internal_name: &str,
        display_name: &str,
        version: &str,
        homepage: &str,
    ) -> TmodMetadata {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let bytes = build_tmod_bytes(internal_name, display_name, version, homepage);
        fs::write(path, bytes).unwrap();
        parse_tmod_file(path).unwrap()
    }

    fn dummy_meta(internal_name: &str, display_name: &str, version: &str, homepage: &str) -> TmodMetadata {
        TmodMetadata {
            file_path: PathBuf::from(format!("{internal_name}.tmod")),
            file_name: format!("{internal_name}.tmod"),
            file_size: 128,
            tmodloader_version: "2026.9.3.1".to_string(),
            internal_name: internal_name.to_string(),
            display_name: display_name.to_string(),
            version: version.to_string(),
            author: "Author".to_string(),
            description: String::new(),
            homepage: homepage.to_string(),
            mod_references: Vec::new(),
            weak_references: Vec::new(),
            side: ModSide::Both,
            library_mod: false,
            sha1_hex: "abc123".to_string(),
            icon_png: None,
        }
    }

    #[test]
    fn mod_sources_json_persistence_and_parse_mod_source_input() {
        let paths = make_temp_paths("sources_json");
        paths.ensure_dirs().unwrap();

        assert!(load_mod_sources(&paths).is_empty());

        let saved = upsert_mod_source(
            &paths,
            "RecipeBrowser",
            "Recipe Browser",
            "0.10.0",
            "deadbeef",
            SharedModSource::Github {
                repo: "JavidPack/RecipeBrowser".to_string(),
                url: String::new(),
            },
        )
        .unwrap();
        assert_eq!(saved.len(), 1);
        assert!(mod_sources_path(&paths).is_file());

        let updated = upsert_mod_source(
            &paths,
            "RecipeBrowser",
            "",
            "",
            "",
            SharedModSource::Workshop {
                id: "2619954303".to_string(),
            },
        )
        .unwrap();
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].display_name, "Recipe Browser");
        assert_eq!(updated[0].mod_version, "0.10.0");
        assert_eq!(updated[0].sha1_hex, "deadbeef");
        assert_eq!(
            updated[0].source,
            SharedModSource::Workshop {
                id: "2619954303".to_string()
            }
        );

        let reloaded = load_mod_sources(&paths);
        assert_eq!(reloaded, updated);

        let after_remove = remove_mod_source(&paths, "RecipeBrowser").unwrap();
        assert!(after_remove.is_empty());
        assert!(load_mod_sources(&paths).is_empty());

        assert!(upsert_mod_source(&paths, "../bad", "Bad", "1.0", "", SharedModSource::ManualOnly).is_err());
        assert!(upsert_mod_source(
            &paths,
            "GoodMod",
            "Good",
            "1.0",
            "",
            SharedModSource::Workshop {
                id: "123abc".to_string()
            }
        )
        .is_err());

        assert_eq!(
            parse_mod_source_input("  2824688072  ").unwrap(),
            SharedModSource::Workshop {
                id: "2824688072".to_string()
            }
        );
        assert_eq!(
            parse_mod_source_input(
                "https://steamcommunity.com/sharedfiles/filedetails/?id=2824688072&searchtext=calamity"
            )
            .unwrap(),
            SharedModSource::Workshop {
                id: "2824688072".to_string()
            }
        );
        assert_eq!(
            parse_mod_source_input("JavidPack/RecipeBrowser").unwrap(),
            SharedModSource::Github {
                repo: "JavidPack/RecipeBrowser".to_string(),
                url: String::new(),
            }
        );
        assert_eq!(
            parse_mod_source_input("https://github.com/JavidPack/RecipeBrowser/").unwrap(),
            SharedModSource::Github {
                repo: "JavidPack/RecipeBrowser".to_string(),
                url: String::new(),
            }
        );
        assert_eq!(
            parse_mod_source_input(
                "https://github.com/blushiemagic/MagicStorage/releases/download/v0.7.0/MagicStorage.tmod"
            )
            .unwrap(),
            SharedModSource::Github {
                repo: "blushiemagic/MagicStorage".to_string(),
                url: "https://github.com/blushiemagic/MagicStorage/releases/download/v0.7.0/MagicStorage.tmod"
                    .to_string(),
            }
        );
        assert_eq!(
            parse_mod_source_input("https://cdn.example.com/mods/Custom.tmod?dl=1").unwrap(),
            SharedModSource::Url {
                url: "https://cdn.example.com/mods/Custom.tmod?dl=1".to_string()
            }
        );

        for bad in [
            "",
            "   ",
            "just_a_word",
            "owner/repo/extra",
            "ftp://example.com/Mod.tmod",
            "https://example.com/page?id=12345",
            "https://github.com/owner/repo/wiki",
            "https://github.com/owner/repo/releases/download/v1.0/archive.zip",
            "https://steamcommunity.com/id/author",
            "https://example.com/not-a-tmod.zip",
        ] {
            assert!(
                parse_mod_source_input(bad).is_err(),
                "expected invalid source for '{bad}'"
            );
        }

        assert_eq!(
            strip_terraria_color_tags("[c/FF5500:Calamity] [c/00AAFF:Mod]"),
            "Calamity Mod"
        );
        assert_eq!(
            extract_steamcmd_downloaded_item_id(
                r#"Success. Downloaded item 2824688072 to "C:\staging\2824688072" (123 bytes)"#
            ),
            Some("2824688072".to_string())
        );

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn share_preview_and_inspect_against_local_resolve_sources_and_versions() {
        let paths = make_temp_paths("preview_inspect");
        paths.ensure_dirs().unwrap();

        let pack_dir = paths.tml_modpacks_dir().join("MyPack");
        let hint_meta = write_tmod_file(
            &pack_dir.join("Mods/HintWorkshopMod.tmod"),
            "HintWorkshopMod",
            "Hint Workshop Mod",
            "1.0.0",
            "https://steamcommunity.com/sharedfiles/filedetails/?id=111222333",
        );
        let override_meta = write_tmod_file(
            &paths.tml_mods_dir().join("SavedOverrideMod.tmod"),
            "SavedOverrideMod",
            "Saved Override Mod",
            "2.1.0",
            "https://steamcommunity.com/sharedfiles/filedetails/?id=999999",
        );

        let mod_sources = vec![ModSourceRecord {
            internal_name: "SavedOverrideMod".to_string(),
            display_name: "Saved Override Mod".to_string(),
            source: SharedModSource::Github {
                repo: "Owner/SavedOverrideMod".to_string(),
                url: String::new(),
            },
            mod_version: "2.1.0".to_string(),
            sha1_hex: String::new(),
            updated_at: 100,
        }];
        let staged = vec![StagedWorkshopMod {
            workshop_id: "444555666".to_string(),
            mod_dir: paths.steamcmd_dir.join("staging/444555666"),
            meta: dummy_meta("StagedOnlyMod", "Staged Only Mod", "3.0.0", ""),
            installed: false,
            enabled: false,
            workshop_title: None,
            author: None,
            preview_url: None,
            time_updated: None,
        }];
        let workshop_records = vec![WorkshopManifestRecord {
            published_file_id: "777888999".to_string(),
            internal_name: "ManifestMissingMod".to_string(),
            file_name: "ManifestMissingMod.tmod".to_string(),
            display_name: "Manifest Missing".to_string(),
            mod_version: "0.9.0".to_string(),
            workshop_title: "Manifest Missing".to_string(),
            author: "Author".to_string(),
            preview_url: String::new(),
            time_updated: 0,
            installed_at: 100,
        }];

        let installed = vec![InstalledModEntry {
            meta: override_meta,
            enabled: true,
            missing_deps: Vec::new(),
            disabled_deps: Vec::new(),
        }];

        let mod_names = vec![
            "HintWorkshopMod".to_string(),
            "SavedOverrideMod".to_string(),
            "StagedOnlyMod".to_string(),
            "ManifestMissingMod".to_string(),
            "PureManualMod".to_string(),
        ];

        let preview = build_modpack_share_preview(
            &paths,
            "MyPack",
            &mod_names,
            Some(&pack_dir),
            &installed,
            &staged,
            &workshop_records,
            &mod_sources,
        )
        .unwrap();

        assert_eq!(preview.pack_name, "MyPack");
        assert_eq!(preview.workshop_count, 3);
        assert_eq!(preview.url_count, 1);
        assert_eq!(preview.manual_count, 1);
        assert_eq!(preview.entries[0].internal_name, hint_meta.internal_name);
        assert_eq!(
            preview.entries[0].source,
            SharedModSource::Workshop {
                id: "111222333".to_string()
            }
        );
        assert_eq!(
            preview.entries[1].source,
            SharedModSource::Github {
                repo: "Owner/SavedOverrideMod".to_string(),
                url: String::new(),
            }
        );
        assert_eq!(
            preview.entries[2].source,
            SharedModSource::Workshop {
                id: "444555666".to_string()
            }
        );
        assert_eq!(
            preview.entries[3].source,
            SharedModSource::Workshop {
                id: "777888999".to_string()
            }
        );
        assert_eq!(preview.entries[4].display_name, "PureManualMod");
        assert_eq!(preview.entries[4].source, SharedModSource::ManualOnly);

        let install_txt = fs::read_to_string(pack_dir.join("Mods/install.txt")).unwrap();
        assert_eq!(install_txt, "111222333\n444555666\n777888999\n");

        // Inspect against local machine that has SavedOverrideMod (2.1.0) installed and StagedOnlyMod (3.0.0) staged,
        // while HintWorkshopMod, ManifestMissingMod, PureManualMod are missing locally.
        let inspection =
            inspect_share_key_against_local(&paths, &preview.share_key, &installed, &staged)
                .unwrap();

        assert_eq!(inspection.suggested_pack_name, "MyPack_2");
        assert_eq!(inspection.already_present_count, 2);
        assert_eq!(inspection.workshop_download_count, 2);
        assert_eq!(inspection.url_download_count, 0);
        assert_eq!(inspection.manual_only_count, 1);
        assert!(inspection.installed_matches.contains("SavedOverrideMod"));
        assert!(inspection.installed_matches.contains("StagedOnlyMod"));
        assert!(!inspection.installed_matches.contains("HintWorkshopMod"));

        // Mismatched installed version must be counted as needing download instead of present.
        let outdated_installed = vec![InstalledModEntry {
            meta: dummy_meta("SavedOverrideMod", "Saved Override Mod", "1.0.0", ""),
            enabled: true,
            missing_deps: Vec::new(),
            disabled_deps: Vec::new(),
        }];
        let outdated_check =
            inspect_share_key_against_local(&paths, &preview.share_key, &outdated_installed, &[])
                .unwrap();
        assert_eq!(outdated_check.already_present_count, 0);
        assert_eq!(outdated_check.url_download_count, 1);
        assert_eq!(outdated_check.workshop_download_count, 3);
        assert_eq!(outdated_check.manual_only_count, 1);

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    async fn spawn_tmod_test_server(
        expected_path: &'static str,
        body: Vec<u8>,
    ) -> (u16, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let req = String::from_utf8_lossy(&buf);
                    let target = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    if target == expected_path {
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(header.as_bytes()).await;
                        let _ = socket.write_all(&body).await;
                    } else {
                        let msg = b"not found";
                        let header = format!(
                            "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            msg.len()
                        );
                        let _ = socket.write_all(header.as_bytes()).await;
                        let _ = socket.write_all(msg).await;
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });
        (port, handle)
    }

    #[test]
    fn sync_modpack_from_share_key_end_to_end_with_local_http_server() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let paths = make_temp_paths("e2e_sync");
            paths.ensure_dirs().unwrap();

            // Pre-existing unsaved enabled mod on the receiver machine.
            let old_tmod = paths.tml_mods_dir().join("OldFriendMod.tmod");
            write_tmod_file(&old_tmod, "OldFriendMod", "Old Friend Mod", "1.0.0", "");
            save_enabled_mods(
                &paths.tml_enabled_json(),
                &["OldFriendMod".to_string()],
            )
            .unwrap();

            // Pre-staged Workshop mod matching ID and version so no SteamCMD call is needed for it.
            let staged_tmod_path = paths.steamcmd_dir.join(
                "staging/steamapps/workshop/content/1281930/2824688072/2026.9/StagedWsMod.tmod",
            );
            write_tmod_file(
                &staged_tmod_path,
                "StagedWsMod",
                "Staged Workshop Mod",
                "2.0.1",
                "",
            );

            // Real HTTP server serving a valid .tmod binary over TCP.
            let remote_tmod_bytes =
                build_tmod_bytes("HttpSyncedMod", "HTTP Synced Mod", "1.4.2", "");
            let (port, server) =
                spawn_tmod_test_server("/mods/HttpSyncedMod.tmod", remote_tmod_bytes).await;

            let mod_url = format!("http://127.0.0.1:{port}/mods/HttpSyncedMod.tmod");
            let payload = ModPackShareKeyPayload {
                v: 1,
                name: "CoopPack".to_string(),
                mods: vec![
                    SharedModEntry {
                        internal_name: "HttpSyncedMod".to_string(),
                        display_name: "HTTP Synced Mod".to_string(),
                        version: "1.4.2".to_string(),
                        source: SharedModSource::Url {
                            url: mod_url.clone(),
                        },
                        enabled: true,
                    },
                    SharedModEntry {
                        internal_name: "StagedWsMod".to_string(),
                        display_name: "Staged Workshop Mod".to_string(),
                        version: "2.0.1".to_string(),
                        source: SharedModSource::Workshop {
                            id: "2824688072".to_string(),
                        },
                        enabled: true,
                    },
                    SharedModEntry {
                        internal_name: "MissingManualMod".to_string(),
                        display_name: String::new(),
                        version: "1.0.0".to_string(),
                        source: SharedModSource::ManualOnly,
                        enabled: true,
                    },
                ],
            };

            let progress_events = Arc::new(Mutex::new(Vec::<ModPackSyncProgress>::new()));
            let progress_clone = Arc::clone(&progress_events);
            let logs = Arc::new(Mutex::new(Vec::<String>::new()));
            let logs_clone = Arc::clone(&logs);

            let report = sync_modpack_from_share_key(
                &paths,
                None,
                None,
                None,
                "",
                &payload,
                "CoopPack",
                move |prog| progress_clone.lock().unwrap().push(prog),
                move |line| logs_clone.lock().unwrap().push(line),
            )
            .await
            .unwrap();

            server.abort();

            assert_eq!(report.pack_name, "CoopPack");
            assert_eq!(
                report.backup_pack_name.as_deref(),
                Some("ModCu_TruocDongBo")
            );
            assert_eq!(report.total_mods, 3);
            assert_eq!(report.already_present, 1);
            assert_eq!(report.downloaded_workshop, 0);
            assert_eq!(report.downloaded_other, 1);
            assert_eq!(report.missing_manual, vec!["MissingManualMod"]);
            assert!(report.failed_mods.is_empty());
            assert_eq!(report.enabled_count, 2);

            // Verify friend's old enabled.json was backed up cleanly.
            let backup_dir = paths.tml_modpacks_dir().join("ModCu_TruocDongBo");
            assert_eq!(
                load_enabled_mods(&backup_dir.join("Mods/enabled.json")).unwrap(),
                vec!["OldFriendMod".to_string()]
            );
            assert!(backup_dir.join("Mods/OldFriendMod.tmod").is_file());

            // Verify new isolated ModPack contents.
            let new_pack_dir = paths.tml_modpacks_dir().join("CoopPack");
            assert!(new_pack_dir.join("Mods/HttpSyncedMod.tmod").is_file());
            assert!(new_pack_dir.join("Mods/StagedWsMod.tmod").is_file());
            assert_eq!(
                load_enabled_mods(&new_pack_dir.join("Mods/enabled.json")).unwrap(),
                vec!["HttpSyncedMod".to_string(), "StagedWsMod".to_string()]
            );
            assert_eq!(
                fs::read_to_string(new_pack_dir.join("Mods/install.txt")).unwrap(),
                "2824688072\n"
            );

            // Verify active Mods/enabled.json switched to the new pack.
            assert_eq!(
                load_enabled_mods(&paths.tml_enabled_json()).unwrap(),
                vec!["HttpSyncedMod".to_string(), "StagedWsMod".to_string()]
            );

            // Verify mod_sources.json recorded both the HTTP URL and Workshop source.
            let sources = load_mod_sources(&paths);
            assert_eq!(sources.len(), 2);
            let http_src = sources
                .iter()
                .find(|s| s.internal_name == "HttpSyncedMod")
                .unwrap();
            assert_eq!(http_src.source, SharedModSource::Url { url: mod_url });
            assert_eq!(http_src.mod_version, "1.4.2");

            let ws_src = sources
                .iter()
                .find(|s| s.internal_name == "StagedWsMod")
                .unwrap();
            assert_eq!(
                ws_src.source,
                SharedModSource::Workshop {
                    id: "2824688072".to_string()
                }
            );

            let prog_list = progress_events.lock().unwrap();
            assert!(!prog_list.is_empty());
            assert!(prog_list.iter().any(|p| {
                p.current_mod == "HTTP Synced Mod"
                    && p.current_bytes > 0
                    && p.total_bytes == Some(p.current_bytes)
            }));
            assert_eq!(prog_list.last().unwrap().completed, 3);
            assert_eq!(prog_list.last().unwrap().total, 3);
            assert!(!logs.lock().unwrap().is_empty());

            let _ = fs::remove_dir_all(&paths.base_dir);
        });
    }

    #[test]
    fn download_source_tracking_and_missing_steamcmd_behavior() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let paths = make_temp_paths("download_and_missing_steamcmd");
            paths.ensure_dirs().unwrap();

            let remote_tmod_bytes =
                build_tmod_bytes("HintedGhMod", "Hinted GH Mod", "1.2.0", "");
            let (port, server) =
                spawn_tmod_test_server("/mods/HintedGhMod.tmod", remote_tmod_bytes).await;

            let url = format!("http://127.0.0.1:{port}/mods/HintedGhMod.tmod");
            let installed_meta = crate::download_and_install_tmod_url_with_source(
                &url,
                Some("HintedGhMod"),
                Some("ModOwner/HintedGhMod"),
                &paths,
                None,
                false,
            )
            .await
            .unwrap();
            server.abort();
            assert_eq!(installed_meta.internal_name, "HintedGhMod");

            let sources_after_url = load_mod_sources(&paths);
            assert_eq!(sources_after_url.len(), 1);
            assert_eq!(
                sources_after_url[0].source,
                SharedModSource::Github {
                    repo: "ModOwner/HintedGhMod".to_string(),
                    url,
                }
            );

            let ws_meta = dummy_meta("RecordedWsMod", "Recorded WS Mod", "2.0.0", "");
            crate::mods_manager::record_workshop_install(&paths, None, "2824688072", &[ws_meta])
                .unwrap();
            let sources_after_ws = load_mod_sources(&paths);
            let ws_entry = sources_after_ws
                .iter()
                .find(|s| s.internal_name == "RecordedWsMod")
                .unwrap();
            assert_eq!(
                ws_entry.source,
                SharedModSource::Workshop {
                    id: "2824688072".to_string(),
                }
            );

            let payload = ModPackShareKeyPayload {
                v: 1,
                name: "MissingSteamCmdPack".to_string(),
                mods: vec![
                    SharedModEntry {
                        internal_name: "MissingWsMod".to_string(),
                        display_name: "Missing Workshop Mod".to_string(),
                        version: "1.0.0".to_string(),
                        source: SharedModSource::Workshop {
                            id: "2619954303".to_string(),
                        },
                        enabled: true,
                    },
                    SharedModEntry {
                        internal_name: "ManualGapMod".to_string(),
                        display_name: "Manual Gap Mod".to_string(),
                        version: "1.0.0".to_string(),
                        source: SharedModSource::ManualOnly,
                        enabled: true,
                    },
                ],
            };

            let report = sync_modpack_from_share_key(
                &paths,
                None,
                None,
                None,
                "",
                &payload,
                "MissingSteamCmdPack",
                |_| {},
                |_| {},
            )
            .await
            .unwrap();

            assert_eq!(report.total_mods, 2);
            assert_eq!(report.downloaded_workshop, 0);
            assert_eq!(report.missing_manual, vec!["ManualGapMod"]);
            assert_eq!(report.failed_mods.len(), 1);
            assert_eq!(report.failed_mods[0].0, "MissingWsMod");
            assert!(report.failed_mods[0].1.contains("SteamCMD"));

            let install_txt = fs::read_to_string(
                paths
                    .tml_modpacks_dir()
                    .join(&report.pack_name)
                    .join("Mods")
                    .join("install.txt"),
            )
            .unwrap_or_default();
            assert!(!install_txt.contains("2619954303"));

            let _ = fs::remove_dir_all(&paths.base_dir);
        });
    }

    #[test]
    fn manual_only_version_mismatch_is_reported_missing_and_excluded_from_pack() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let paths = make_temp_paths("manual_mismatch");
            paths.ensure_dirs().unwrap();

            write_tmod_file(
                &paths.tml_mods_dir().join("CustomPrivateMod.tmod"),
                "CustomPrivateMod",
                "Custom Private Mod",
                "1.0.0",
                "",
            );

            let payload = ModPackShareKeyPayload {
                v: 1,
                name: "ManualMismatchPack".to_string(),
                mods: vec![SharedModEntry {
                    internal_name: "CustomPrivateMod".to_string(),
                    display_name: "Custom Private Mod".to_string(),
                    version: "2.0.0".to_string(),
                    source: SharedModSource::ManualOnly,
                    enabled: true,
                }],
            };
            let share_key = encode_modpack_share_key(&payload).unwrap();
            let installed = scan_installed_mods(&paths).unwrap();
            let check =
                inspect_share_key_against_local(&paths, &share_key, &installed, &[]).unwrap();
            assert_eq!(check.already_present_count, 0);
            assert_eq!(check.manual_only_count, 1);

            let report = sync_modpack_from_share_key(
                &paths,
                None,
                None,
                None,
                "",
                &payload,
                "ManualMismatchPack",
                |_| {},
                |_| {},
            )
            .await
            .unwrap();

            assert_eq!(report.already_present, 0);
            assert_eq!(report.missing_manual, vec!["CustomPrivateMod"]);
            let pack_enabled = tl_core::load_enabled_mods(
                &paths
                    .tml_modpacks_dir()
                    .join(&report.pack_name)
                    .join("Mods")
                    .join("enabled.json"),
            )
            .unwrap_or_default();
            assert!(pack_enabled.is_empty());

            let _ = fs::remove_dir_all(&paths.base_dir);
        });
    }

    #[test]
    fn pick_tmod_for_entry_requires_matching_name_and_version_never_unrelated() {
        let paths = make_temp_paths("pick_tmod_entry");
        paths.ensure_dirs().unwrap();
        let item_dir = workshop_staging_content_dir(&paths).join("987654321");
        let unrelated = item_dir.join("2026.9").join("UnrelatedMod.tmod");
        let target_v1 = item_dir.join("2026.9").join("MyTargetMod.tmod");
        write_tmod_file(&unrelated, "UnrelatedMod", "Unrelated Mod", "1.0.0", "");
        write_tmod_file(&target_v1, "MyTargetMod", "My Target Mod", "1.0.0", "");
        let candidates = vec![unrelated.clone(), target_v1.clone()];

        let missing_entry = SharedModEntry {
            internal_name: "CompletelyDifferent".to_string(),
            display_name: "Completely Different".to_string(),
            version: "".to_string(),
            source: SharedModSource::Workshop {
                id: "987654321".to_string(),
            },
            enabled: true,
        };
        assert_eq!(
            pick_tmod_for_entry(&candidates, &missing_entry, &item_dir, (2026, 9)),
            None
        );

        let wrong_version = SharedModEntry {
            internal_name: "mytargetmod".to_string(),
            display_name: "My Target Mod".to_string(),
            version: "2.0.0".to_string(),
            source: SharedModSource::Workshop {
                id: "987654321".to_string(),
            },
            enabled: true,
        };
        assert_eq!(
            pick_tmod_for_entry(&candidates, &wrong_version, &item_dir, (2026, 9)),
            None
        );

        let exact_match_ci = SharedModEntry {
            internal_name: "mytargetmod".to_string(),
            display_name: "My Target Mod".to_string(),
            version: "1.0.0".to_string(),
            source: SharedModSource::Workshop {
                id: "987654321".to_string(),
            },
            enabled: true,
        };
        assert_eq!(
            pick_tmod_for_entry(&candidates, &exact_match_ci, &item_dir, (2026, 9)),
            Some(target_v1)
        );

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn batched_workshop_acceptance_requires_marker_or_success_with_change() {
        let mut counted = HashSet::new();
        assert!(!should_accept_workshop_download_for_batch(
            &counted, "111", false, false
        ));
        assert!(!should_accept_workshop_download_for_batch(
            &counted, "111", false, true
        ));
        assert!(!should_accept_workshop_download_for_batch(
            &counted, "111", true, false
        ));
        assert!(should_accept_workshop_download_for_batch(
            &counted, "111", true, true
        ));

        counted.insert("111".to_string());
        assert!(should_accept_workshop_download_for_batch(
            &counted, "111", false, false
        ));
    }

    #[test]
    fn sync_modpack_case_insensitive_present_and_omits_failed_workshop_from_install_txt() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let paths = make_temp_paths("install_txt_filter");
            paths.ensure_dirs().unwrap();

            write_tmod_file(
                &paths.tml_mods_dir().join("ExistingWsMod.tmod"),
                "ExistingWsMod",
                "Existing Ws Mod",
                "1.2.3",
                "",
            );

            let payload = ModPackShareKeyPayload {
                v: 1,
                name: "FilterInstallTxtPack".to_string(),
                mods: vec![
                    SharedModEntry {
                        internal_name: "existingwsmod".to_string(),
                        display_name: "Existing Ws Mod".to_string(),
                        version: "1.2.3".to_string(),
                        source: SharedModSource::Workshop {
                            id: "111111111".to_string(),
                        },
                        enabled: true,
                    },
                    SharedModEntry {
                        internal_name: "FailedWsMod".to_string(),
                        display_name: "Failed Ws Mod".to_string(),
                        version: "1.0.0".to_string(),
                        source: SharedModSource::Workshop {
                            id: "222222222".to_string(),
                        },
                        enabled: true,
                    },
                ],
            };

            let report = sync_modpack_from_share_key(
                &paths,
                None,
                None,
                None,
                "",
                &payload,
                "FilterInstallTxtPack",
                |_| {},
                |_| {},
            )
            .await
            .unwrap();

            assert_eq!(report.already_present, 1);
            assert_eq!(report.failed_mods.len(), 1);
            assert_eq!(report.failed_mods[0].0, "FailedWsMod");

            let pack_mods_dir = paths.tml_modpacks_dir().join(&report.pack_name).join("Mods");
            let pack_enabled =
                tl_core::load_enabled_mods(&pack_mods_dir.join("enabled.json")).unwrap_or_default();
            assert_eq!(pack_enabled, vec!["ExistingWsMod".to_string()]);

            let install_txt = fs::read_to_string(pack_mods_dir.join("install.txt")).unwrap();
            assert!(install_txt.contains("111111111"));
            assert!(!install_txt.contains("222222222"));

            let _ = fs::remove_dir_all(&paths.base_dir);
        });
    }

    #[test]
    fn build_modpack_share_preview_with_states_and_sync_preserves_disabled_mods() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let paths_a = make_temp_paths("mixed_states_a");
            let paths_b = make_temp_paths("mixed_states_b");
            paths_a.ensure_dirs().unwrap();
            paths_b.ensure_dirs().unwrap();

            let enabled_bytes = build_tmod_bytes("EnabledUrlMod", "Enabled URL Mod", "1.0.0", "");
            let disabled_url_bytes =
                build_tmod_bytes("DisabledUrlMod", "Disabled URL Mod", "1.1.0", "");
            let (port_a, srv_a) =
                spawn_tmod_test_server("/mods/EnabledUrlMod.tmod", enabled_bytes).await;
            let (port_b, srv_b) =
                spawn_tmod_test_server("/mods/DisabledUrlMod.tmod", disabled_url_bytes).await;

            let enabled_url = format!("http://127.0.0.1:{port_a}/mods/EnabledUrlMod.tmod");
            let disabled_url = format!("http://127.0.0.1:{port_b}/mods/DisabledUrlMod.tmod");

            let pack_a_dir = paths_a.tml_modpacks_dir().join("MixedPack");
            let meta_enabled = write_tmod_file(
                &pack_a_dir.join("Mods/EnabledUrlMod.tmod"),
                "EnabledUrlMod",
                "Enabled URL Mod",
                "1.0.0",
                &enabled_url,
            );
            let meta_disabled_url = write_tmod_file(
                &pack_a_dir.join("Mods/DisabledUrlMod.tmod"),
                "DisabledUrlMod",
                "Disabled URL Mod",
                "1.1.0",
                &disabled_url,
            );
            let meta_disabled_ws = write_tmod_file(
                &pack_a_dir.join("Mods/DisabledWsMod.tmod"),
                "DisabledWsMod",
                "Disabled WS Mod",
                "2.0.0",
                "https://steamcommunity.com/sharedfiles/filedetails/?id=987654321",
            );

            let installed_a = vec![
                InstalledModEntry {
                    meta: meta_enabled,
                    enabled: true,
                    missing_deps: Vec::new(),
                    disabled_deps: Vec::new(),
                },
                InstalledModEntry {
                    meta: meta_disabled_url,
                    enabled: false,
                    missing_deps: Vec::new(),
                    disabled_deps: Vec::new(),
                },
                InstalledModEntry {
                    meta: meta_disabled_ws,
                    enabled: false,
                    missing_deps: Vec::new(),
                    disabled_deps: Vec::new(),
                },
            ];

            let mod_names = vec![
                "EnabledUrlMod".to_string(),
                "DisabledUrlMod".to_string(),
                "DisabledWsMod".to_string(),
            ];
            let mut explicit_enabled = HashMap::new();
            explicit_enabled.insert("disabledurlmod".to_string(), false);
            explicit_enabled.insert("disabledwsmod".to_string(), false);

            let preview = build_modpack_share_preview_with_states(
                &paths_a,
                "MixedPack",
                &mod_names,
                Some(&pack_a_dir),
                &installed_a,
                &[],
                &[],
                &[],
                Some(&explicit_enabled),
            )
            .unwrap();

            assert!(preview.entries[0].enabled);
            assert!(!preview.entries[1].enabled);
            assert!(!preview.entries[2].enabled);

            let decoded = decode_modpack_share_key(&preview.share_key).unwrap();
            assert!(decoded.mods[0].enabled);
            assert!(!decoded.mods[1].enabled);
            assert!(!decoded.mods[2].enabled);

            let staged_ws_b = paths_b.steamcmd_dir.join(
                "staging/steamapps/workshop/content/1281930/987654321/2026.9/DisabledWsMod.tmod",
            );
            write_tmod_file(
                &staged_ws_b,
                "DisabledWsMod",
                "Disabled WS Mod",
                "2.0.0",
                "",
            );

            let report = sync_modpack_from_share_key(
                &paths_b,
                None,
                None,
                None,
                "",
                &decoded,
                "MixedPack",
                |_| {},
                |_| {},
            )
            .await
            .unwrap();

            srv_a.abort();
            srv_b.abort();

            assert_eq!(report.total_mods, 3);
            assert_eq!(report.already_present, 1);
            assert_eq!(report.downloaded_other, 2);
            assert_eq!(report.enabled_count, 1);

            assert!(paths_b.tml_mods_dir().join("EnabledUrlMod.tmod").is_file());
            assert!(paths_b.tml_mods_dir().join("DisabledUrlMod.tmod").is_file());
            assert!(paths_b.tml_mods_dir().join("DisabledWsMod.tmod").is_file());

            let pack_b_mods = paths_b.tml_modpacks_dir().join(&report.pack_name).join("Mods");
            assert_eq!(
                load_enabled_mods(&pack_b_mods.join("enabled.json")).unwrap(),
                vec!["EnabledUrlMod".to_string()]
            );
            assert_eq!(
                load_enabled_mods(&paths_b.tml_enabled_json()).unwrap(),
                vec!["EnabledUrlMod".to_string()]
            );
            assert_eq!(
                fs::read_to_string(pack_b_mods.join("install.txt")).unwrap(),
                "987654321\n"
            );

            let mut all_disabled_map = HashMap::new();
            all_disabled_map.insert("disabledwsmod".to_string(), false);
            let all_disabled_preview = build_modpack_share_preview_with_states(
                &paths_a,
                "AllDisabled",
                &["DisabledWsMod".to_string()],
                None,
                &installed_a,
                &[],
                &[],
                &[],
                Some(&all_disabled_map),
            )
            .unwrap();
            let all_disabled_report = sync_modpack_from_share_key(
                &paths_b,
                None,
                None,
                None,
                "",
                &all_disabled_preview.payload,
                "AllDisabled",
                |_| {},
                |_| {},
            )
            .await
            .unwrap();
            assert_eq!(all_disabled_report.enabled_count, 0);
            assert!(load_enabled_mods(&paths_b.tml_enabled_json()).unwrap().is_empty());

            let _ = fs::remove_dir_all(&paths_a.base_dir);
            let _ = fs::remove_dir_all(&paths_b.base_dir);
        });
    }

    #[test]
    fn case_insensitive_mod_sources_lookup_upsert_and_sync_update() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let paths = make_temp_paths("ci_sources");
            paths.ensure_dirs().unwrap();

            upsert_mod_source(
                &paths,
                "recipebrowser",
                "Recipe Browser",
                "0.10.0",
                "sha1old",
                SharedModSource::Github {
                    repo: "JavidPack/RecipeBrowser".to_string(),
                    url: String::new(),
                },
            )
            .unwrap();

            let updated = upsert_mod_source(
                &paths,
                "RecipeBrowser",
                "",
                "",
                "",
                SharedModSource::Workshop {
                    id: "2619954303".to_string(),
                },
            )
            .unwrap();
            assert_eq!(updated.len(), 1);
            assert_eq!(updated[0].internal_name, "RecipeBrowser");
            assert_eq!(updated[0].display_name, "Recipe Browser");
            assert_eq!(updated[0].mod_version, "0.10.0");
            assert_eq!(updated[0].sha1_hex, "sha1old");

            // Collapse legacy differently-cased duplicates cleanly on upsert.
            let duplicates = vec![
                ModSourceRecord {
                    internal_name: "dupmod".to_string(),
                    display_name: "Dup Mod".to_string(),
                    source: SharedModSource::Url {
                        url: "https://example.com/old.tmod".to_string(),
                    },
                    mod_version: "1.0.0".to_string(),
                    sha1_hex: "abc".to_string(),
                    updated_at: 1,
                },
                ModSourceRecord {
                    internal_name: "DUPMOD".to_string(),
                    display_name: String::new(),
                    source: SharedModSource::Url {
                        url: "https://example.com/older.tmod".to_string(),
                    },
                    mod_version: String::new(),
                    sha1_hex: String::new(),
                    updated_at: 2,
                },
            ];
            write_atomic(
                &mod_sources_path(&paths),
                serde_json::to_string_pretty(&duplicates).unwrap().as_bytes(),
            )
            .unwrap();

            let collapsed = upsert_mod_source(
                &paths,
                "DupMod",
                "",
                "",
                "",
                SharedModSource::Workshop {
                    id: "123456789".to_string(),
                },
            )
            .unwrap();
            assert_eq!(collapsed.len(), 1);
            assert_eq!(collapsed[0].internal_name, "DupMod");
            assert_eq!(collapsed[0].display_name, "Dup Mod");
            assert_eq!(
                collapsed[0].source,
                SharedModSource::Workshop {
                    id: "123456789".to_string()
                }
            );

            let meta_ci = dummy_meta("DUPMOD", "Dup Mod", "1.0.0", "");
            assert_eq!(
                resolve_mod_source_for_meta(&meta_ci, &collapsed, &[], &[]),
                SharedModSource::Workshop {
                    id: "123456789".to_string()
                }
            );
            assert_eq!(
                fallback_display_and_version("dupmod", &collapsed, &[]),
                ("Dup Mod".to_string(), "1.0.0".to_string())
            );

            // Sync with a pre-installed mod whose mod_sources record differs must update mod_sources.
            write_tmod_file(
                &paths.tml_mods_dir().join("DupMod.tmod"),
                "DupMod",
                "Dup Mod",
                "1.0.0",
                "",
            );
            let sync_payload = ModPackShareKeyPayload {
                v: 1,
                name: "SourceRefreshPack".to_string(),
                mods: vec![SharedModEntry {
                    internal_name: "dupmod".to_string(),
                    display_name: "Dup Mod".to_string(),
                    version: "1.0.0".to_string(),
                    source: SharedModSource::Github {
                        repo: "Owner/DupMod".to_string(),
                        url: String::new(),
                    },
                    enabled: true,
                }],
            };
            let report = sync_modpack_from_share_key(
                &paths,
                None,
                None,
                None,
                "",
                &sync_payload,
                "SourceRefreshPack",
                |_| {},
                |_| {},
            )
            .await
            .unwrap();
            assert_eq!(report.already_present, 1);
            let refreshed = load_mod_sources(&paths);
            assert_eq!(refreshed.len(), 1);
            assert_eq!(
                refreshed[0].source,
                SharedModSource::Github {
                    repo: "Owner/DupMod".to_string(),
                    url: String::new(),
                }
            );

            let after_remove = remove_mod_source(&paths, "dUpMoD").unwrap();
            assert!(after_remove.is_empty());

            let _ = fs::remove_dir_all(&paths.base_dir);
        });
    }

    #[test]
    fn resolve_mod_source_for_meta_ignores_staged_with_mismatched_sha1() {
        let mut local_custom = dummy_meta(
            "CustomLocalMod",
            "Custom Local Mod",
            "1.0.0",
            "https://example.com/CustomLocalMod.tmod",
        );
        local_custom.sha1_hex = "local_custom_sha1".to_string();

        let mut staged_meta = dummy_meta("customlocalmod", "Staged WS Mod", "1.0.0", "");
        staged_meta.sha1_hex = "staged_ws_sha1".to_string();

        let staged = vec![StagedWorkshopMod {
            workshop_id: "555666777".to_string(),
            mod_dir: PathBuf::from("staging/555666777"),
            meta: staged_meta,
            installed: false,
            enabled: false,
            workshop_title: None,
            author: None,
            preview_url: None,
            time_updated: None,
        }];

        // Different SHA-1 must not hijack the local custom mod with the staged Workshop ID.
        assert_eq!(
            resolve_mod_source_for_meta(&local_custom, &[], &[], &staged),
            SharedModSource::Url {
                url: "https://example.com/CustomLocalMod.tmod".to_string()
            }
        );

        local_custom.homepage.clear();
        assert_eq!(
            resolve_mod_source_for_meta(&local_custom, &[], &[], &staged),
            SharedModSource::ManualOnly
        );

        // Matching SHA-1 resolves to the staged Workshop ID (even with different ASCII case).
        local_custom.sha1_hex = "staged_ws_sha1".to_string();
        assert_eq!(
            resolve_mod_source_for_meta(&local_custom, &[], &[], &staged),
            SharedModSource::Workshop {
                id: "555666777".to_string()
            }
        );

        // When both SHA-1 values are empty, require matching version.
        let mut empty_sha_local = dummy_meta("EmptyShaMod", "Empty SHA", "1.0.0", "");
        empty_sha_local.sha1_hex.clear();
        let mut empty_sha_staged = dummy_meta("emptyshamod", "Empty SHA", "2.0.0", "");
        empty_sha_staged.sha1_hex.clear();
        let staged_empty_sha = vec![StagedWorkshopMod {
            workshop_id: "888999000".to_string(),
            mod_dir: PathBuf::from("staging/888999000"),
            meta: empty_sha_staged,
            installed: false,
            enabled: false,
            workshop_title: None,
            author: None,
            preview_url: None,
            time_updated: None,
        }];
        assert_eq!(
            resolve_mod_source_for_meta(&empty_sha_local, &[], &[], &staged_empty_sha),
            SharedModSource::ManualOnly
        );
        empty_sha_local.version = "2.0.0".to_string();
        assert_eq!(
            resolve_mod_source_for_meta(&empty_sha_local, &[], &[], &staged_empty_sha),
            SharedModSource::Workshop {
                id: "888999000".to_string()
            }
        );
    }

    #[test]
    fn build_modpack_share_preview_picks_newer_installed_version_and_clears_stale_install_txt() {
        let paths = make_temp_paths("preview_fresh_clear_install_txt");
        paths.ensure_dirs().unwrap();

        let pack_dir = paths.tml_modpacks_dir().join("StalePack");
        write_tmod_file(
            &pack_dir.join("Mods/FreshMod.tmod"),
            "FreshMod",
            "Fresh Mod",
            "1.0.0",
            "https://example.com/old.tmod",
        );
        write_modpack_install_txt(&pack_dir, &["999888777".to_string()]).unwrap();
        assert!(!fs::read_to_string(pack_dir.join("Mods/install.txt"))
            .unwrap()
            .is_empty());

        let inst_meta = write_tmod_file(
            &paths.tml_mods_dir().join("FreshMod.tmod"),
            "FreshMod",
            "Fresh Mod",
            "2.1.0",
            "https://example.com/new.tmod",
        );
        let installed = vec![InstalledModEntry {
            meta: inst_meta,
            enabled: true,
            missing_deps: Vec::new(),
            disabled_deps: Vec::new(),
        }];

        let preview = build_modpack_share_preview_with_states(
            &paths,
            "StalePack",
            &["FreshMod".to_string()],
            Some(&pack_dir),
            &installed,
            &[],
            &[],
            &[],
            None,
        )
        .unwrap();

        assert_eq!(preview.entries.len(), 1);
        assert_eq!(preview.entries[0].version, "2.1.0");
        assert_eq!(
            preview.entries[0].source,
            SharedModSource::Url {
                url: "https://example.com/new.tmod".to_string()
            }
        );
        assert_eq!(
            fs::read_to_string(pack_dir.join("Mods/install.txt")).unwrap(),
            ""
        );

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn pick_tmod_for_entry_accepts_newer_workshop_version_and_name_fallback_ignores_unrelated() {
        let paths = make_temp_paths("pick_tmod_ws_ge");
        paths.ensure_dirs().unwrap();
        let item_dir = workshop_staging_content_dir(&paths).join("555111222");
        let unrelated = item_dir.join("2026.9").join("UnrelatedMod.tmod");
        let target_new = item_dir.join("2026.9").join("TargetMod.tmod");
        write_tmod_file(&unrelated, "UnrelatedMod", "Unrelated Mod", "9.9.9", "");
        write_tmod_file(&target_new, "TargetMod", "Target Mod", "2.0.4.2", "");
        let candidates = vec![unrelated, target_new.clone()];

        let ws_older_req = SharedModEntry {
            internal_name: "targetmod".to_string(),
            display_name: "Target Mod".to_string(),
            version: "2.0.4.1".to_string(),
            source: SharedModSource::Workshop {
                id: "555111222".to_string(),
            },
            enabled: true,
        };
        assert_eq!(
            pick_tmod_for_entry(&candidates, &ws_older_req, &item_dir, (2026, 9)),
            Some(target_new.clone())
        );

        let url_older_req = SharedModEntry {
            source: SharedModSource::Url {
                url: "https://example.com/TargetMod.tmod".to_string(),
            },
            ..ws_older_req.clone()
        };
        assert_eq!(
            pick_tmod_for_entry(&candidates, &url_older_req, &item_dir, (2026, 9)),
            None
        );

        let ws_higher_req = SharedModEntry {
            version: "3.0.0.0".to_string(),
            ..ws_older_req
        };
        assert_eq!(
            pick_tmod_for_entry(&candidates, &ws_higher_req, &item_dir, (2026, 9)),
            None
        );
        assert_eq!(
            pick_tmod_name_fallback_for_entry(&candidates, &ws_higher_req, &item_dir, (2026, 9)),
            Some(target_new)
        );

        let missing_entry = SharedModEntry {
            internal_name: "NeverMatches".to_string(),
            ..ws_higher_req
        };
        assert_eq!(
            pick_tmod_name_fallback_for_entry(&candidates, &missing_entry, &item_dir, (2026, 9)),
            None
        );

        let _ = fs::remove_dir_all(&paths.base_dir);
    }

    #[test]
    fn sync_modpack_from_share_key_updates_in_place_and_syncs_workshop_manifest() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let paths = make_temp_paths("sync_inplace_ws");
            paths.ensure_dirs().unwrap();

            let existing_pack_dir = paths.tml_modpacks_dir().join("CoopPack");
            write_tmod_file(
                &existing_pack_dir.join("Mods/ObsoleteMod.tmod"),
                "ObsoleteMod",
                "Obsolete Mod",
                "1.0.0",
                "",
            );
            tl_core::save_enabled_mods(
                &existing_pack_dir.join("Mods/enabled.json"),
                &["ObsoleteMod".to_string()],
            )
            .unwrap();
            write_modpack_install_txt(&existing_pack_dir, &["999999999".to_string()]).unwrap();

            write_tmod_file(
                &paths.tml_mods_dir().join("PresentWsMod.tmod"),
                "PresentWsMod",
                "Present WS Mod",
                "1.5.0",
                "",
            );
            let existing_ws_meta = write_tmod_file(
                &paths.tml_mods_dir().join("TrackedWsMod.tmod"),
                "TrackedWsMod",
                "Tracked WS Mod",
                "1.0.0",
                "",
            );
            record_workshop_install(
                &paths,
                Some(&WorkshopItemDetails {
                    published_file_id: "222222222".to_string(),
                    title: "Tracked WS Mod".to_string(),
                    time_updated: 1_650_000_000,
                    ..Default::default()
                }),
                "222222222",
                &[existing_ws_meta],
            )
            .unwrap();

            let staged_tmod = paths.steamcmd_dir.join(
                "staging/steamapps/workshop/content/1281930/333333333/2026.9/StagedTsMod.tmod",
            );
            write_tmod_file(&staged_tmod, "StagedTsMod", "Staged TS Mod", "2.0.0", "");
            let staged_mtime = file_mtime_unix(&staged_tmod).unwrap();

            let payload = ModPackShareKeyPayload {
                v: 1,
                name: "CoopPack".to_string(),
                mods: vec![
                    SharedModEntry {
                        internal_name: "PresentWsMod".to_string(),
                        display_name: "Present WS Mod".to_string(),
                        version: "1.5.0".to_string(),
                        source: SharedModSource::Workshop {
                            id: "111111111".to_string(),
                        },
                        enabled: true,
                    },
                    SharedModEntry {
                        internal_name: "TrackedWsMod".to_string(),
                        display_name: "Tracked WS Mod".to_string(),
                        version: "1.0.0".to_string(),
                        source: SharedModSource::Workshop {
                            id: "222222222".to_string(),
                        },
                        enabled: true,
                    },
                    SharedModEntry {
                        internal_name: "StagedTsMod".to_string(),
                        display_name: "Staged TS Mod".to_string(),
                        version: "2.0.0".to_string(),
                        source: SharedModSource::Workshop {
                            id: "333333333".to_string(),
                        },
                        enabled: true,
                    },
                ],
            };

            let report_explicit = sync_modpack_from_share_key(
                &paths,
                None,
                None,
                None,
                "",
                &payload,
                "cooppack",
                |_| {},
                |_| {},
            )
            .await
            .unwrap();

            assert_eq!(report_explicit.pack_name, "CoopPack");
            assert!(!existing_pack_dir.join("Mods/ObsoleteMod.tmod").exists());
            assert!(existing_pack_dir.join("Mods/PresentWsMod.tmod").is_file());
            assert!(existing_pack_dir.join("Mods/TrackedWsMod.tmod").is_file());
            assert!(existing_pack_dir.join("Mods/StagedTsMod.tmod").is_file());
            assert_eq!(
                fs::read_to_string(existing_pack_dir.join("Mods/install.txt")).unwrap(),
                "111111111\n222222222\n333333333\n"
            );

            let manifest = load_workshop_manifest(&paths);
            assert_eq!(manifest.len(), 3);
            let tracked = manifest
                .iter()
                .find(|r| r.published_file_id == "222222222")
                .unwrap();
            assert_eq!(tracked.time_updated, 1_650_000_000);
            let present = manifest
                .iter()
                .find(|r| r.published_file_id == "111111111")
                .unwrap();
            assert!(present.time_updated > 0);
            let staged_rec = manifest
                .iter()
                .find(|r| r.published_file_id == "333333333")
                .unwrap();
            assert_eq!(staged_rec.time_updated, staged_mtime);

            let report_active = sync_modpack_from_share_key(
                &paths,
                None,
                None,
                None,
                "cooppack",
                &payload,
                "",
                |_| {},
                |_| {},
            )
            .await
            .unwrap();
            assert_eq!(report_active.pack_name, "CoopPack");

            let report_collision = sync_modpack_from_share_key(
                &paths,
                None,
                None,
                None,
                "",
                &payload,
                "",
                |_| {},
                |_| {},
            )
            .await
            .unwrap();
            assert_eq!(report_collision.pack_name, "CoopPack_2");

            let _ = fs::remove_dir_all(&paths.base_dir);
        });
    }
}
