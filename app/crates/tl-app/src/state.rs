use crate::assets::LauncherAssets;
use crate::theme::{format_bytes, palette, ThemePalette};
use egui::{ColorImage, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use sysinfo::System;
use tl_core::{
    activate_modpack, backup_unsaved_enabled_mods_as_modpack, backup_world, create_modpack,
    decode_modpack_share_key, delete_modpack, detect_environment, list_modpacks,
    list_world_backups, parse_world_file, restore_world_backup, sanitize_modpack_name,
    write_modpack_install_txt, EnvironmentStatus, GameEdition, LauncherConfig, LauncherPaths,
    ModPackInfo, SavedJoinServer, ServerConfigData, ServerProfile, SharedModSource, ThemeMode,
    TmodMetadata, WorldBackupInfo, WorldMetadata, DEFAULT_TERRARIA_INSTALL_SOURCE,
};
use tl_runtime::{
    browse_workshop_mods, build_modpack_share_preview, check_firewall_rule_exists,
    delete_installed_mod, detect_existing_game_process,
    detect_existing_server_processes, download_and_install_tmod_url_with_source,
    download_and_install_xna, download_workshop_mod_via_steamcmd, ensure_firewall_rule,
    fetch_github_latest_tmod, fetch_terraria_branch_manifest, fetch_tmodloader_releases,
    fetch_workshop_details, fetch_workshop_image_bytes, import_local_mod_files,
    inspect_share_key_against_local, install_dotnet8_portable, install_steamcmd_with_progress,
    install_terraria_from_local_zip, install_terraria_from_source, install_tmodloader_release,
    install_xna_framework_elevated, is_local_probe_host, launch_game, load_banlist,
    load_installed_terraria_receipt, load_mod_sources, load_workshop_manifest,
    parse_mod_source_input, parse_workshop_id, probe_local_endpoint_listening,
    probe_local_tcp_listening, probe_network_snapshot, probe_tcp_endpoint,
    read_server_log_summary, record_workshop_install, remove_firewall_rule,
    resolve_ipv6_bridge_target, save_banlist, scan_installed_mods, scan_staged_workshop_mods,
    search_workshop_candidates_for_mod, set_mod_enabled, split_endpoint_host_port,
    start_direct_port_host, start_ipv4_to_ipv6_client_bridge, start_server,
    sync_modpack_from_share_key, terminate_process_by_pid, upsert_mod_source, ActiveGameProcess,
    BanEntry, DetectedExternalServer, DirectHostConfig, DirectHostStatus, DirectPortHostHandle,
    ExternalServerLogSummary, GameLaunchOptions, InstallProgress, InstallTarget, InstalledModEntry,
    InstalledTerrariaReceipt, LocalClientBridgeHandle, ModPackImportPreview, ModPackSharePreview,
    ModPackSyncProgress, ModPackSyncReport, ModSourceRecord, NetworkSnapshot, ProgressCallback,
    RemoteTmodAsset, RepaintHandle, RunningGameSnapshot, RuntimeTuning, ServerLifecycleState,
    ServerRuntimeSnapshot, ServerSupervisor, StagedWorkshopMod, TerrariaBranchManifest,
    TmodLoaderReleaseInfo, WorkshopItemDetails, WorkshopManifestRecord, WorkshopQueryResult,
    WorkshopSort,
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavTab {
    Play,
    Server,
    Mods,
    Settings,
}

impl NavTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Play => "Chơi Game",
            Self::Server => "Server Game",
            Self::Mods => "Cài Mod",
            Self::Settings => "Cài đặt",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerSubTab {
    Config,
    Network,
    Console,
}

impl ServerSubTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Config => "Cấu hình",
            Self::Network => "Mở Port & IP",
            Self::Console => "Console & Người chơi",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModsSubTab {
    Installed,
    GitHub,
    ModPacks,
    Workshop,
}

impl ModsSubTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Installed => "Đã cài",
            Self::GitHub => "GitHub & URL",
            Self::ModPacks => "ModPack",
            Self::Workshop => "Workshop",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppNotification {
    pub is_error: bool,
    pub message: String,
    pub created_at: Instant,
}

#[derive(Clone)]
pub enum WorkshopThumbState {
    Loading,
    Ready(TextureHandle),
    Failed,
}

pub enum AppEvent {
    NetworkProbed(NetworkSnapshot),
    FirewallResult(Result<String, String>),
    PortProbed {
        endpoint: String,
        result: Result<u128, String>,
    },
    GithubFetched {
        repo: String,
        result: Result<Vec<RemoteTmodAsset>, String>,
    },
    ModDownloaded(Box<Result<TmodMetadata, String>>),
    WorkshopBrowsed {
        page: u32,
        result: Result<WorkshopQueryResult, String>,
    },
    WorkshopThumbnailLoaded {
        url: String,
        result: Result<Vec<u8>, String>,
    },
    WorkshopLookedUp(Result<WorkshopItemDetails, String>),
    WorkshopLog(String),
    WorkshopFinished {
        workshop_id: String,
        details: Option<WorkshopItemDetails>,
        result: Result<Vec<TmodMetadata>, String>,
    },
    SteamCmdInstalled(Result<String, String>),
    TmlReleasesFetched(Result<Vec<TmodLoaderReleaseInfo>, String>),
    TerrariaManifestChecked(Result<TerrariaBranchManifest, String>),
    InstallProgress(InstallProgress),
    InstallFinished {
        target: InstallTarget,
        result: Result<String, String>,
    },
    ModPackSyncProgress(ModPackSyncProgress),
    ModPackSynced(Box<Result<ModPackSyncReport, String>>),
    WorkshopCandidatesLoaded {
        internal_name: String,
        result: Result<Vec<WorkshopItemDetails>, String>,
    },
}

pub struct AppState {
    pub rt: tokio::runtime::Runtime,
    pub repaint: RepaintHandle,
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    pub event_rx: mpsc::UnboundedReceiver<AppEvent>,

    pub assets: Arc<LauncherAssets>,
    pub paths: LauncherPaths,
    pub config: LauncherConfig,
    pub env: EnvironmentStatus,
    pub sys: Option<System>,

    pub active_tab: NavTab,
    pub server_tab: ServerSubTab,
    pub mods_tab: ModsSubTab,
    pub notification: Option<AppNotification>,
    pub last_poll_at: Instant,

    pub active_game: Option<ActiveGameProcess>,
    pub running_game: Option<RunningGameSnapshot>,
    pub external_game: Option<RunningGameSnapshot>,
    client_ipv6_bridge: Option<LocalClientBridgeHandle>,

    pub server_supervisors: HashMap<String, ServerSupervisor>,
    pub server_snapshots: HashMap<String, ServerRuntimeSnapshot>,
    pub external_servers: Vec<DetectedExternalServer>,
    pub external_log_summaries: HashMap<String, ExternalServerLogSummary>,
    cleared_external_log_pids: HashMap<String, u32>,
    pub last_external_scan_at: Instant,
    pub console_input: String,
    pub console_autoscroll: bool,
    pub banlist_entries: Vec<BanEntry>,
    pub manual_ban_name: String,
    pub manual_ban_ip: String,

    pub pending_auto_direct_host_profile_id: Option<String>,
    pub direct_host_handle: Option<DirectPortHostHandle>,
    pub direct_host_port_input: String,
    pub direct_bind_ip: Option<std::net::Ipv4Addr>,
    pub direct_try_upnp: bool,
    pub firewall_busy: bool,
    pub network_snapshot: NetworkSnapshot,
    pub network_loading: bool,
    network_reprobe_queued: bool,
    pub port_probe_host: String,
    pub port_probe_port: String,
    pub port_probe_status: Option<(bool, String)>,
    pub port_probe_busy: bool,

    pub installed_mods: Vec<InstalledModEntry>,
    pub mod_icons: HashMap<String, TextureHandle>,
    pub modpacks: Vec<ModPackInfo>,
    pub new_modpack_name: String,
    pub mod_search: String,
    pub mod_sources: Vec<ModSourceRecord>,
    pub share_key_input: String,
    pub share_import_preview: Option<Result<ModPackImportPreview, String>>,
    pub share_import_pack_name: String,
    pub modpack_sync_busy: bool,
    pub modpack_sync_progress: Option<ModPackSyncProgress>,
    pub modpack_sync_report: Option<ModPackSyncReport>,
    pub share_export_preview: Option<ModPackSharePreview>,
    pub share_export_target_pack: Option<String>,
    pub unlinked_source_inputs: HashMap<String, String>,
    pub workshop_candidate_target: Option<String>,
    pub workshop_candidates: Vec<WorkshopItemDetails>,
    pub workshop_candidates_busy: bool,

    pub github_repo_input: String,
    pub github_assets: Vec<RemoteTmodAsset>,
    pub github_busy: bool,
    pub direct_mod_url: String,

    pub workshop_input: String,
    pub workshop_details: Option<WorkshopItemDetails>,
    pub workshop_logs: Vec<String>,
    pub workshop_busy: bool,
    pub workshop_sort: WorkshopSort,
    pub workshop_page: u32,
    pub workshop_has_next: bool,
    pub workshop_results: Vec<WorkshopItemDetails>,
    pub workshop_browse_busy: bool,
    pub workshop_browse_error: Option<String>,
    pub workshop_initial_loaded: bool,
    pub workshop_downloading_id: Option<String>,
    pub workshop_records: Vec<WorkshopManifestRecord>,
    pub staged_workshop_mods: Vec<StagedWorkshopMod>,
    pub workshop_thumbnails: HashMap<String, WorkshopThumbState>,
    pub last_saved_proxy: String,
    pub steamcmd_busy: bool,
    pub steamcmd_progress: Option<InstallProgress>,

    pub tml_releases: Vec<TmodLoaderReleaseInfo>,
    pub selected_tml_tag: String,
    pub tml_releases_busy: bool,
    pub terraria_remote_manifest: Option<TerrariaBranchManifest>,
    pub terraria_installed_receipt: Option<InstalledTerrariaReceipt>,
    pub terraria_check_busy: bool,
    pub install_busy: bool,
    pub install_progress: Option<InstallProgress>,
    pub install_last_error: Option<String>,

    pub worlds: Vec<WorldMetadata>,
    pub vanilla_worlds: Vec<WorldMetadata>,
    pub tml_worlds: Vec<WorldMetadata>,
    pub world_backups: Vec<WorldBackupInfo>,

    pub quick_join_port_input: String,
    pub server_port_input: String,
    pub workshop_inspect_busy: bool,
    pub show_new_profile_form: bool,
    pub confirm_delete_profile_id: Option<String>,
    pub confirm_restore_backup: Option<PathBuf>,

    pub new_profile_name: String,
    pub new_profile_edition: GameEdition,
    pub new_join_name: String,
}

impl AppState {
    pub fn initialize(
        ctx: &egui::Context,
        rt: tokio::runtime::Runtime,
    ) -> Self {
        let initial_paths = LauncherPaths::resolve(None, None);
        let _ = initial_paths.ensure_dirs();
        let config = LauncherConfig::load_or_default(&initial_paths);
        let paths = LauncherPaths::resolve(
            non_empty_opt(&config.custom_terraria_dir),
            non_empty_opt(&config.custom_tmodloader_dir),
        );
        let _ = paths.ensure_dirs();
        let env = detect_environment(&paths, non_empty_opt(&config.custom_steamcmd_path));
        let terraria_installed_receipt = load_installed_terraria_receipt(&paths);
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let repaint_ctx = ctx.clone();
        let repaint = RepaintHandle::new(Arc::new(move || {
            repaint_ctx.request_repaint();
        }));

        let default_port = config
            .active_server()
            .map(|s| s.config.port)
            .filter(|&p| p > 0)
            .unwrap_or(7777);
        let quick_join_port_init = if config.quick_join_port > 0 {
            config.quick_join_port
        } else {
            7777
        };
        let last_saved_proxy = config.proxy_url.trim().to_string();

        let mut state = Self {
            rt,
            repaint,
            event_tx,
            event_rx,
            assets: Arc::new(LauncherAssets::load(ctx)),
            paths,
            config,
            env,
            sys: System::new().ok(),
            active_tab: NavTab::Play,
            server_tab: ServerSubTab::Config,
            mods_tab: ModsSubTab::Installed,
            notification: None,
            last_poll_at: Instant::now(),
            active_game: None,
            running_game: None,
            external_game: None,
            client_ipv6_bridge: None,
            server_supervisors: HashMap::new(),
            server_snapshots: HashMap::new(),
            external_servers: Vec::new(),
            external_log_summaries: HashMap::new(),
            cleared_external_log_pids: HashMap::new(),
            last_external_scan_at: Instant::now(),
            console_input: String::new(),
            console_autoscroll: true,
            banlist_entries: Vec::new(),
            manual_ban_name: String::new(),
            manual_ban_ip: String::new(),
            pending_auto_direct_host_profile_id: None,
            direct_host_handle: None,
            direct_host_port_input: default_port.to_string(),
            direct_bind_ip: None,
            direct_try_upnp: true,
            firewall_busy: false,
            network_snapshot: NetworkSnapshot::default(),
            network_loading: false,
            network_reprobe_queued: false,
            port_probe_host: String::new(),
            port_probe_port: default_port.to_string(),
            port_probe_status: None,
            port_probe_busy: false,
            installed_mods: Vec::new(),
            mod_icons: HashMap::new(),
            modpacks: Vec::new(),
            new_modpack_name: String::new(),
            mod_search: String::new(),
            mod_sources: Vec::new(),
            share_key_input: String::new(),
            share_import_preview: None,
            share_import_pack_name: String::new(),
            modpack_sync_busy: false,
            modpack_sync_progress: None,
            modpack_sync_report: None,
            share_export_preview: None,
            share_export_target_pack: None,
            unlinked_source_inputs: HashMap::new(),
            workshop_candidate_target: None,
            workshop_candidates: Vec::new(),
            workshop_candidates_busy: false,
            github_repo_input: String::new(),
            github_assets: Vec::new(),
            github_busy: false,
            direct_mod_url: String::new(),
            workshop_input: String::new(),
            workshop_details: None,
            workshop_logs: Vec::new(),
            workshop_busy: false,
            workshop_sort: WorkshopSort::Trending,
            workshop_page: 1,
            workshop_has_next: false,
            workshop_results: Vec::new(),
            workshop_browse_busy: false,
            workshop_browse_error: None,
            workshop_initial_loaded: false,
            workshop_downloading_id: None,
            workshop_records: Vec::new(),
            staged_workshop_mods: Vec::new(),
            workshop_thumbnails: HashMap::new(),
            last_saved_proxy,
            steamcmd_busy: false,
            steamcmd_progress: None,
            tml_releases: Vec::new(),
            selected_tml_tag: String::new(),
            tml_releases_busy: false,
            terraria_remote_manifest: None,
            terraria_installed_receipt,
            terraria_check_busy: false,
            install_busy: false,
            install_progress: None,
            install_last_error: None,
            worlds: Vec::new(),
            vanilla_worlds: Vec::new(),
            tml_worlds: Vec::new(),
            world_backups: Vec::new(),
            quick_join_port_input: quick_join_port_init.to_string(),
            server_port_input: default_port.to_string(),
            workshop_inspect_busy: false,
            show_new_profile_form: false,
            confirm_delete_profile_id: None,
            confirm_restore_backup: None,
            new_profile_name: String::new(),
            new_profile_edition: GameEdition::TModLoader,
            new_join_name: String::new(),
        };

        state.refresh_worlds_and_backups();
        state.refresh_mods_and_packs();
        state.refresh_banlist();
        state.scan_external_servers();
        if state.active_external_server().is_none() {
            if let Some(running_id) = state
                .external_servers
                .iter()
                .filter_map(|ext| ext.profile_id.as_ref())
                .find(|id| state.config.servers.iter().any(|p| &p.id == *id))
                .cloned()
            {
                state.select_server_profile(running_id);
            }
        }
        if !state.network_loading {
            state.spawn_network_probe();
        }
        state
    }

    pub fn current_running_game(&self) -> Option<&RunningGameSnapshot> {
        self.running_game.as_ref().or(self.external_game.as_ref())
    }

    pub fn is_game_running(&self) -> bool {
        self.active_game.is_some() || self.external_game.is_some()
    }

    pub fn active_server_supervisor(&self) -> Option<&ServerSupervisor> {
        self.server_supervisors.get(&self.config.active_server_id)
    }

    pub fn active_server_snapshot(&self) -> Option<&ServerRuntimeSnapshot> {
        self.server_snapshots.get(&self.config.active_server_id)
    }

    pub fn external_server_for_profile(&self, profile_id: &str) -> Option<&DetectedExternalServer> {
        self.external_servers
            .iter()
            .find(|d| d.profile_id.as_deref() == Some(profile_id))
    }

    pub fn active_external_server(&self) -> Option<&DetectedExternalServer> {
        if self.active_server_supervisor().is_some_and(|s| s.is_active()) {
            return None;
        }
        let active_id = self.config.active_server_id.as_str();
        if let Some(found) = self
            .external_servers
            .iter()
            .find(|d| d.profile_id.as_deref() == Some(active_id))
        {
            return Some(found);
        }
        let active_port = self
            .config
            .active_server()
            .map(|p| p.config.port)
            .unwrap_or(0);
        if active_port == 0 {
            return None;
        }
        let collides_with_supervised = self.server_supervisors.iter().any(|(id, sup)| {
            id != active_id
                && sup.is_active()
                && self
                    .server_snapshots
                    .get(id)
                    .map(|s| s.port)
                    .or_else(|| {
                        self.config
                            .servers
                            .iter()
                            .find(|p| p.id == *id)
                            .map(|p| p.config.port)
                    })
                    == Some(active_port)
        });
        if collides_with_supervised {
            return None;
        }
        self.external_servers.iter().find(|d| {
            d.port == Some(active_port)
                && d.profile_id
                    .as_deref()
                    .is_none_or(|pid| !self.config.servers.iter().any(|p| p.id == pid))
        })
    }

    pub fn active_external_log_summary(&self) -> Option<&ExternalServerLogSummary> {
        let ext = self.active_external_server()?;
        let profile_id = ext.profile_id.as_deref()?;
        self.external_log_summaries.get(profile_id)
    }

    pub fn total_running_servers_count(&self) -> usize {
        let supervised = self
            .server_supervisors
            .values()
            .filter(|sup| sup.is_active())
            .count();
        supervised + self.external_servers.len()
    }

    pub fn any_server_active(&self) -> bool {
        self.total_running_servers_count() > 0
    }

    pub fn is_profile_running(&self, profile_id: &str) -> bool {
        self.server_supervisors
            .get(profile_id)
            .is_some_and(|sup| sup.is_active())
            || self.external_server_for_profile(profile_id).is_some()
    }

    pub fn is_active_external_starting(&self) -> bool {
        self.showing_external_server()
            && self.active_external_log_summary().is_some_and(|summary| {
                !summary.is_listening
                    && (!summary.lines.is_empty() || !summary.status_detail.is_empty())
            })
    }

    pub fn running_supervised_snapshots(&self) -> Vec<(String, ServerRuntimeSnapshot)> {
        let mut out: Vec<(String, ServerRuntimeSnapshot)> = self
            .server_supervisors
            .iter()
            .filter(|(_, sup)| sup.is_active())
            .filter_map(|(id, _)| {
                self.server_snapshots
                    .get(id)
                    .cloned()
                    .map(|snap| (id.clone(), snap))
            })
            .collect();
        out.sort_by(|a, b| {
            a.1.profile_name
                .to_lowercase()
                .cmp(&b.1.profile_name.to_lowercase())
                .then_with(|| a.0.cmp(&b.0))
        });
        out
    }

    pub fn select_server_profile(&mut self, id: String) {
        if !self.config.servers.iter().any(|s| s.id == id) {
            return;
        }
        let changed = self.config.active_server_id != id;
        if changed {
            self.config.active_server_id = id;
            self.save_config();
        }
        self.confirm_delete_profile_id = None;
        self.refresh_banlist();
        let cfg_port = self
            .config
            .active_server()
            .map(|s| s.config.port)
            .filter(|&p| p > 0)
            .unwrap_or(7777);
        self.server_port_input = cfg_port.to_string();
        let port_str = self.active_server_port().to_string();
        self.port_probe_port = port_str.clone();
        if !matches!(
            self.active_direct_host_status(),
            DirectHostStatus::Running(_) | DirectHostStatus::Starting { .. }
        ) {
            self.direct_host_port_input = port_str;
        }
        if changed {
            self.spawn_network_probe();
        }
    }

    pub fn notify_ok(&mut self, msg: impl Into<String>) {
        self.notification = Some(AppNotification {
            is_error: false,
            message: msg.into(),
            created_at: Instant::now(),
        });
    }

    pub fn notify_err(&mut self, msg: impl Into<String>) {
        self.notification = Some(AppNotification {
            is_error: true,
            message: msg.into(),
            created_at: Instant::now(),
        });
    }

    pub fn save_config(&mut self) {
        match self.config.save(&self.paths) {
            Ok(()) => {
                let current_proxy = self.config.proxy_url.trim().to_string();
                if current_proxy != self.last_saved_proxy {
                    self.last_saved_proxy = current_proxy;
                    self.workshop_browse_error = None;
                    self.workshop_initial_loaded = false;
                    self.workshop_thumbnails
                        .retain(|_, v| !matches!(v, WorkshopThumbState::Failed));
                }
            }
            Err(e) => self.notify_err(format!("Lưu cấu hình thất bại: {e}")),
        }
    }

    pub fn refresh_environment(&mut self) {
        self.paths = LauncherPaths::resolve(
            non_empty_opt(&self.config.custom_terraria_dir),
            non_empty_opt(&self.config.custom_tmodloader_dir),
        );
        let _ = self.paths.ensure_dirs();
        self.env = detect_environment(
            &self.paths,
            non_empty_opt(&self.config.custom_steamcmd_path),
        );
        self.terraria_installed_receipt = load_installed_terraria_receipt(&self.paths);
        self.refresh_worlds_and_backups();
        self.refresh_mods_and_packs();
        self.refresh_banlist();
    }

    pub fn refresh_worlds_and_backups(&mut self) {
        let scan_dir = |dir: &std::path::Path| -> Vec<WorldMetadata> {
            let mut parsed = Vec::new();
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path
                        .extension()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.eq_ignore_ascii_case("wld"))
                    {
                        if let Ok(meta) = parse_world_file(&path) {
                            parsed.push(meta);
                        }
                    }
                }
            }
            parsed.sort_by_key(|a| a.world_name.to_lowercase());
            parsed
        };
        self.vanilla_worlds = scan_dir(&self.paths.vanilla_worlds_dir());
        self.tml_worlds = scan_dir(&self.paths.tml_worlds_dir());
        self.worlds = match self.config.active_edition {
            GameEdition::Vanilla => self.vanilla_worlds.clone(),
            GameEdition::TModLoader => self.tml_worlds.clone(),
        };
        self.world_backups = list_world_backups(&self.paths.backups_dir).unwrap_or_default();
        self.confirm_restore_backup = None;
    }

    pub fn worlds_for_edition(&self, edition: GameEdition) -> &[WorldMetadata] {
        match edition {
            GameEdition::Vanilla => &self.vanilla_worlds,
            GameEdition::TModLoader => &self.tml_worlds,
        }
    }

    pub fn refresh_mods_and_packs(&mut self) {
        match scan_installed_mods(&self.paths) {
            Ok(list) => self.installed_mods = list,
            Err(e) => self.notify_err(format!("Không quét được thư mục Mods: {e}")),
        }
        self.modpacks = list_modpacks(&self.paths.tml_modpacks_dir()).unwrap_or_default();
        self.workshop_records = load_workshop_manifest(&self.paths);
        self.staged_workshop_mods = scan_staged_workshop_mods(&self.paths).unwrap_or_default();
        self.mod_sources = load_mod_sources(&self.paths);
        if self.share_export_preview.is_some() {
            self.refresh_active_share_export_preview(false);
        }
        if !self.share_key_input.trim().is_empty() {
            let preview_res = inspect_share_key_against_local(
                &self.paths,
                &self.share_key_input,
                &self.installed_mods,
                &self.staged_workshop_mods,
            )
            .map_err(|e| e.to_string());
            if let Ok(ref preview) = preview_res {
                if self.share_import_pack_name.trim().is_empty() {
                    self.share_import_pack_name = preview.suggested_pack_name.clone();
                }
            }
            self.share_import_preview = Some(preview_res);
        }
    }

    pub fn refresh_banlist(&mut self) {
        let Some(profile) = self.config.active_server() else {
            self.banlist_entries.clear();
            return;
        };
        let ban_path = self.resolve_profile_banlist_path(profile);
        self.banlist_entries = load_banlist(&ban_path);
    }

    pub fn resolve_profile_banlist_path(&self, profile: &ServerProfile) -> PathBuf {
        let raw = profile.config.banlist.trim();
        if raw.is_empty() || raw.eq_ignore_ascii_case("banlist.txt") {
            self.paths.servers_dir.join(&profile.id).join("banlist.txt")
        } else {
            PathBuf::from(raw)
        }
    }

    pub fn poll_background(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.event_rx.try_recv() {
            match ev {
                AppEvent::NetworkProbed(snap) => {
                    self.network_loading = false;
                    if self.port_probe_host.trim().is_empty() {
                        if let Some((h, p)) = self
                            .active_online_endpoint()
                            .as_deref()
                            .and_then(split_endpoint_host_port)
                        {
                            self.port_probe_host = h;
                            self.port_probe_port = p.to_string();
                        } else if let Some(pub_ip) = snap.public_ip {
                            self.port_probe_host = pub_ip.to_string();
                            self.port_probe_port = self.active_server_port().to_string();
                        }
                    }
                    self.network_snapshot = snap;
                    if self.network_reprobe_queued {
                        self.spawn_network_probe();
                    }
                }
                AppEvent::FirewallResult(res) => {
                    self.firewall_busy = false;
                    let port = self.active_server_port();
                    self.network_snapshot.firewall_rule_active =
                        Some(check_firewall_rule_exists(port));
                    match res {
                        Ok(msg) => self.notify_ok(msg),
                        Err(e) => self.notify_err(e),
                    }
                }
                AppEvent::PortProbed { endpoint, result } => {
                    self.port_probe_busy = false;
                    self.port_probe_status = Some(match result {
                        Ok(ms) => {
                            let is_local = split_endpoint_host_port(&endpoint).is_some_and(
                                |(h, _)| is_local_probe_host(&h, &self.network_snapshot),
                            );
                            let msg = if is_local {
                                format!(
                                    "{endpoint} đang mở nội bộ ({ms} ms — thử tự kiểm tra trên máy này, chưa xác nhận qua Modem)"
                                )
                            } else {
                                format!("{endpoint} phản hồi ({ms} ms)")
                            };
                            (true, msg)
                        }
                        Err(e) => (false, format!("{endpoint}: {e}")),
                    });
                }
                AppEvent::GithubFetched { repo, result } => {
                    self.github_busy = false;
                    match result {
                        Ok(assets) => {
                            self.notify_ok(format!("Tìm thấy {} file .tmod từ {repo}", assets.len()));
                            self.github_assets = assets;
                        }
                        Err(e) => {
                            self.github_assets.clear();
                            self.notify_err(e);
                        }
                    }
                }
                AppEvent::ModDownloaded(boxed_res) => {
                    self.github_busy = false;
                    match *boxed_res {
                        Ok(meta) => {
                            self.refresh_mods_and_packs();
                            self.notify_ok(format!(
                                "Đã cài đặt mod {} v{}",
                                meta.display_name, meta.version
                            ));
                        }
                        Err(e) => self.notify_err(e),
                    }
                }
                AppEvent::WorkshopBrowsed { page, result } => {
                    self.workshop_browse_busy = false;
                    match result {
                        Ok(res) => {
                            self.workshop_browse_error = None;
                            self.workshop_page = if res.page > 0 { res.page } else { page };
                            self.workshop_has_next = res.has_next_page;
                            self.workshop_results = res.items;
                        }
                        Err(e) => {
                            let clean = e.strip_prefix("Network error: ").unwrap_or(&e).to_string();
                            self.workshop_browse_error = Some(clean);
                        }
                    }
                }
                AppEvent::WorkshopThumbnailLoaded { url, result } => {
                    let thumb_state = match result {
                        Ok(bytes) => match image::load_from_memory(&bytes) {
                            Ok(img) => {
                                let rgba = img.to_rgba8();
                                let (w, h) = rgba.dimensions();
                                if w == 0 || h == 0 {
                                    WorkshopThumbState::Failed
                                } else {
                                    let color_img = ColorImage::from_rgba_unmultiplied(
                                        [w as usize, h as usize],
                                        rgba.as_raw(),
                                    );
                                    let handle = ctx.load_texture(
                                        format!("ws_thumb_{url}"),
                                        color_img,
                                        TextureOptions::LINEAR,
                                    );
                                    WorkshopThumbState::Ready(handle)
                                }
                            }
                            Err(_) => WorkshopThumbState::Failed,
                        },
                        Err(_) => WorkshopThumbState::Failed,
                    };
                    self.workshop_thumbnails.insert(url, thumb_state);
                }
                AppEvent::WorkshopLookedUp(res) => {
                    self.workshop_inspect_busy = false;
                    match res {
                        Ok(details) => self.workshop_details = Some(details),
                        Err(e) => self.notify_err(e),
                    }
                }
                AppEvent::WorkshopLog(line) => {
                    self.push_workshop_log(line);
                }
                AppEvent::WorkshopFinished {
                    workshop_id,
                    details,
                    result,
                } => {
                    self.workshop_busy = false;
                    self.workshop_downloading_id = None;
                    match result {
                        Ok(mods) => {
                            if let Err(e) = record_workshop_install(
                                &self.paths,
                                details.as_ref(),
                                &workshop_id,
                                &mods,
                            ) {
                                self.refresh_mods_and_packs();
                                self.notify_err(format!(
                                    "Đã tải {} mod nhưng lỗi lưu lịch sử Workshop: {e}",
                                    mods.len()
                                ));
                            } else {
                                self.refresh_mods_and_packs();
                                self.notify_ok(format!(
                                    "Đã tải và cài {} mod từ Workshop (ID {workshop_id}).",
                                    mods.len()
                                ));
                            }
                        }
                        Err(e) => {
                            self.refresh_mods_and_packs();
                            self.notify_err(e);
                        }
                    }
                }
                AppEvent::SteamCmdInstalled(res) => {
                    self.steamcmd_busy = false;
                    self.steamcmd_progress = None;
                    self.refresh_environment();
                    match res {
                        Ok(msg) => self.notify_ok(msg),
                        Err(e) => {
                            self.workshop_logs.push(format!("Lỗi cài SteamCMD: {e}"));
                            self.notify_err(e);
                        }
                    }
                }
                AppEvent::TmlReleasesFetched(res) => {
                    self.tml_releases_busy = false;
                    match res {
                        Ok(releases) => {
                            let needs_select = self.selected_tml_tag.trim().is_empty()
                                || !releases.iter().any(|r| r.tag_name == self.selected_tml_tag);
                            if needs_select {
                                self.selected_tml_tag = pick_default_tml_release(&releases)
                                    .map(|r| r.tag_name.clone())
                                    .unwrap_or_default();
                            }
                            let count = releases.len();
                            self.tml_releases = releases;
                            if !self.install_busy {
                                self.notify_ok(format!(
                                    "Đã tải {count} bản phát hành tModLoader từ GitHub."
                                ));
                            }
                        }
                        Err(e) => {
                            if !self.install_busy {
                                self.notify_err(format!(
                                    "Không tải được danh sách tModLoader: {e}"
                                ));
                            }
                        }
                    }
                }
                AppEvent::TerrariaManifestChecked(res) => {
                    self.terraria_check_busy = false;
                    match res {
                        Ok(manifest) => {
                            let ver = manifest.version.clone();
                            let size = manifest.total_size;
                            self.terraria_remote_manifest = Some(manifest);
                            if !self.install_busy {
                                if size > 0 {
                                    self.notify_ok(format!(
                                        "Đã kiểm tra nguồn Terraria: bản {ver} ({}).",
                                        format_bytes(size)
                                    ));
                                } else {
                                    self.notify_ok(format!(
                                        "Đã kiểm tra nguồn Terraria: bản {ver}."
                                    ));
                                }
                            }
                        }
                        Err(e) => {
                            self.terraria_remote_manifest = None;
                            if !self.install_busy {
                                self.notify_err(format!(
                                    "Không kiểm tra được nguồn Terraria: {e}"
                                ));
                            }
                        }
                    }
                }
                AppEvent::InstallProgress(prog) => {
                    if prog.target == InstallTarget::SteamCmd {
                        self.steamcmd_progress = Some(prog);
                    } else {
                        self.install_progress = Some(prog);
                    }
                }
                AppEvent::InstallFinished { target, result } => {
                    self.install_busy = false;
                    self.install_progress = None;
                    self.refresh_environment();
                    match result {
                        Ok(msg) => {
                            self.install_last_error = None;
                            let text = if msg.trim().is_empty() {
                                format!("Đã cài đặt {} thành công.", target.label())
                            } else {
                                msg
                            };
                            self.notify_ok(text);
                        }
                        Err(e) => {
                            self.install_last_error = Some(e.clone());
                            self.notify_err(format!("Cài đặt {} thất bại: {e}", target.label()));
                        }
                    }
                }
                AppEvent::ModPackSyncProgress(prog) => {
                    self.modpack_sync_progress = Some(prog);
                }
                AppEvent::ModPackSynced(boxed_res) => {
                    self.modpack_sync_busy = false;
                    self.modpack_sync_progress = None;
                    match *boxed_res {
                        Ok(report) => {
                            self.config.active_modpack = report.pack_name.clone();
                            self.save_config();
                            self.refresh_mods_and_packs();
                            self.notify_ok(format_modpack_sync_summary(&report));
                            self.modpack_sync_report = Some(report);
                        }
                        Err(e) => {
                            self.refresh_mods_and_packs();
                            self.notify_err(e);
                        }
                    }
                }
                AppEvent::WorkshopCandidatesLoaded {
                    internal_name,
                    result,
                } => {
                    if self.workshop_candidate_target.as_deref() != Some(internal_name.as_str()) {
                        continue;
                    }
                    self.workshop_candidates_busy = false;
                    match result {
                        Ok(items) => {
                            if items.is_empty() {
                                self.workshop_candidates.clear();
                                self.notify_err(format!(
                                    "Không tìm thấy mod '{internal_name}' trên Steam Workshop."
                                ));
                            } else {
                                self.workshop_candidates = items;
                            }
                        }
                        Err(e) => {
                            self.workshop_candidates.clear();
                            self.notify_err(e);
                        }
                    }
                }
            }
        }

        if let Some(notif) = &self.notification {
            if notif.created_at.elapsed() > Duration::from_secs(9) {
                self.notification = None;
            }
        }

        self.handle_dropped_files(ctx);

        let now = Instant::now();
        let should_poll_procs = now.duration_since(self.last_poll_at) >= Duration::from_millis(750);
        if should_poll_procs {
            self.last_poll_at = now;

            if let Some(proc_handle) = &mut self.active_game {
                if let Some(exit) = proc_handle.poll() {
                    let edition = proc_handle.edition;
                    let tail = proc_handle.recent_output();
                    let log_file = proc_handle.log_path().to_path_buf();
                    self.active_game = None;
                    self.running_game = None;
                    self.stop_client_ipv6_bridge();
                    if exit.success() {
                        self.notify_ok(format!("{} đã đóng bình thường.", edition.label()));
                    } else {
                        let snippet = tail
                            .iter()
                            .rev()
                            .take(2)
                            .cloned()
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect::<Vec<_>>()
                            .join(" | ");
                        let msg = if snippet.is_empty() {
                            format!(
                                "{} thoát bất thường (mã {:?}). Xem log: {}",
                                edition.label(),
                                exit.code(),
                                log_file.display()
                            )
                        } else {
                            format!(
                                "{} thoát bất thường (mã {:?}): {} (log: {})",
                                edition.label(),
                                exit.code(),
                                snippet,
                                log_file.display()
                            )
                        };
                        self.notify_err(msg);
                    }
                    self.refresh_worlds_and_backups();
                } else {
                    self.running_game = Some(proc_handle.snapshot());
                    self.external_game = None;
                }
            } else if self.external_game.is_some() {
                if self.sys.is_none() {
                    self.sys = System::new().ok();
                }
                self.external_game = self
                    .sys
                    .as_mut()
                    .and_then(|sys| detect_existing_game_process(sys, &self.paths));
            }

            if !self.server_supervisors.is_empty() {
                if self.sys.is_none() {
                    self.sys = System::new().ok();
                }
                let mut any_banlist_dirty = false;
                let mut any_just_stopped = false;
                let mut any_network_dirty = false;

                let sup_entries: Vec<(String, ServerSupervisor)> = self
                    .server_supervisors
                    .iter()
                    .map(|(id, sup)| (id.clone(), sup.clone()))
                    .collect();

                for (profile_id, sup) in sup_entries {
                    let prev_snap = self.server_snapshots.get(&profile_id);
                    let prev_player_count = prev_snap.map(|s| s.players.len()).unwrap_or(0);
                    let prev_state = prev_snap.map(|s| s.state.clone());

                    let snap_opt = match self.sys.as_mut() {
                        Some(sys) => Some(sup.snapshot(sys)),
                        None => System::new()
                            .ok()
                            .map(|mut fallback| sup.snapshot(&mut fallback)),
                    };

                    if let Some(snap) = snap_opt {
                        let curr_player_count = snap.players.len();
                        let just_started_running =
                            matches!(snap.state, ServerLifecycleState::Running)
                                && !matches!(prev_state, Some(ServerLifecycleState::Running));
                        let just_stopped = matches!(
                            snap.state,
                            ServerLifecycleState::Stopped | ServerLifecycleState::Crashed(_)
                        ) && !matches!(
                            prev_state,
                            Some(ServerLifecycleState::Stopped | ServerLifecycleState::Crashed(_))
                        );

                        self.server_snapshots.insert(profile_id, snap);

                        if curr_player_count != prev_player_count || just_stopped {
                            any_banlist_dirty = true;
                        }
                        if just_stopped {
                            any_just_stopped = true;
                            any_network_dirty = true;
                        }
                        if just_started_running {
                            any_network_dirty = true;
                        }
                    }
                }

                if any_banlist_dirty {
                    self.refresh_banlist();
                }
                if any_just_stopped {
                    self.refresh_worlds_and_backups();
                }
                if any_network_dirty {
                    self.spawn_network_probe();
                }
            }
        }

        if now.saturating_duration_since(self.last_external_scan_at) >= Duration::from_secs(2) {
            self.scan_external_servers();
        }

        if let Some(target_id) = self.pending_auto_direct_host_profile_id.clone() {
            let sup_active = self
                .server_supervisors
                .get(&target_id)
                .is_some_and(|s| s.is_active());
            match self.server_snapshots.get(&target_id).map(|s| &s.state) {
                Some(ServerLifecycleState::Running) => {
                    self.pending_auto_direct_host_profile_id = None;
                    let port = self
                        .server_snapshots
                        .get(&target_id)
                        .map(|s| s.port)
                        .filter(|&p| p > 0)
                        .unwrap_or_else(|| self.active_server_port());
                    self.start_direct_host_on_port(port);
                }
                Some(ServerLifecycleState::Stopped | ServerLifecycleState::Crashed(_)) | None => {
                    self.pending_auto_direct_host_profile_id = None;
                    self.notify_err("Server đã dừng trước khi kịp mở Port mạng.");
                }
                Some(ServerLifecycleState::Starting | ServerLifecycleState::Stopping) => {
                    if !sup_active {
                        self.pending_auto_direct_host_profile_id = None;
                        self.notify_err("Server đã dừng trước khi kịp mở Port mạng.");
                    }
                }
            }
        }

        let any_task_busy = self.install_busy
            || self.steamcmd_busy
            || self.modpack_sync_busy
            || self.workshop_busy
            || self.workshop_inspect_busy
            || self.workshop_browse_busy
            || self.workshop_candidates_busy
            || self.github_busy
            || self.port_probe_busy
            || self.firewall_busy
            || self.network_loading
            || self.tml_releases_busy
            || self.terraria_check_busy
            || matches!(
                self.active_direct_host_status(),
                DirectHostStatus::Starting { .. }
            );
        let repaint_interval = if any_task_busy {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(500)
        };
        ctx.request_repaint_after(repaint_interval);
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = dropped.into_iter().map(|f| f.path().to_path_buf()).collect();
        if paths.is_empty() {
            return;
        }
        match import_local_mod_files(&paths, &self.paths, true) {
            Ok(imported) if !imported.is_empty() => {
                self.refresh_mods_and_packs();
                self.active_tab = NavTab::Mods;
                self.mods_tab = ModsSubTab::Installed;
                self.notify_ok(format!("Đã nhập và bật {} mod.", imported.len()));
            }
            Ok(_) => self.notify_err("Không tìm thấy file .tmod hợp lệ trong các file vừa kéo thả."),
            Err(e) => self.notify_err(format!("Lỗi nhập mod: {e}")),
        }
    }

    pub fn active_server_port(&self) -> u16 {
        let port = if self
            .active_server_supervisor()
            .is_some_and(|s| s.is_active())
        {
            self.active_server_snapshot()
                .map(|s| s.port)
                .or_else(|| self.config.active_server().map(|p| p.config.port))
        } else {
            self.config
                .active_server()
                .map(|p| p.config.port)
                .filter(|&p| p > 0)
                .or_else(|| self.active_external_server().and_then(|e| e.port))
        };
        port.filter(|&p| p > 0).unwrap_or(7777)
    }

    pub fn mod_icon_texture(
        &mut self,
        ctx: &egui::Context,
        meta: &TmodMetadata,
    ) -> Option<egui::TextureHandle> {
        if let Some(tex) = self.mod_icons.get(&meta.sha1_hex) {
            return Some(tex.clone());
        }
        let bytes = meta.icon_png.as_ref()?;
        let img = image::load_from_memory(bytes).ok()?.to_rgba8();
        let (w, h) = img.dimensions();
        let color_img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
        let handle = ctx.load_texture(
            format!("tmod_icon_{}", meta.sha1_hex),
            color_img,
            TextureOptions::LINEAR,
        );
        self.mod_icons.insert(meta.sha1_hex.clone(), handle.clone());
        Some(handle)
    }

    pub fn palette(&self) -> ThemePalette {
        palette(self.config.theme_mode)
    }

    pub fn set_theme_mode(&mut self, ctx: &egui::Context, mode: ThemeMode) {
        if self.config.theme_mode == mode {
            return;
        }
        self.config.theme_mode = mode;
        crate::theme::apply_theme(ctx, mode);
        self.save_config();
    }

    fn stop_client_ipv6_bridge(&mut self) {
        if let Some(bridge) = self.client_ipv6_bridge.take() {
            bridge.stop();
        }
    }

    pub fn launch_selected_game(&mut self, join_server: Option<(String, u16, String)>) {
        if self.active_game.is_some() {
            self.notify_err("Game đang chạy. Hãy đóng phiên hiện tại trước khi mở mới.");
            return;
        }
        if let Some(ext) = &self.external_game {
            let msg = format!(
                "{} đang mở sẵn (PID {}). Hãy kết nối trực tiếp trong menu Multiplayer của game hoặc nhấn 'Đóng Game' trước khi mở mới.",
                ext.edition.label(),
                ext.pid
            );
            self.notify_err(msg);
            return;
        }
        self.stop_client_ipv6_bridge();
        if self.config.active_edition == GameEdition::TModLoader {
            let selected_pack = self.config.active_modpack.trim().to_string();
            if !selected_pack.is_empty()
                && self
                    .paths
                    .tml_modpacks_dir()
                    .join(&selected_pack)
                    .join("Mods")
                    .is_dir()
            {
                let _ = activate_modpack(
                    &self.paths.tml_modpacks_dir().join(&selected_pack),
                    &self.paths.tml_mods_dir(),
                    true,
                );
            }
        }
        let (join_host, join_port, join_password) = match join_server {
            Some((raw_host, raw_port, password)) => {
                let (h, p) = split_host_port_input(&raw_host, raw_port);
                (h, p, password)
            }
            None => {
                let fallback_p = self
                    .quick_join_port_input
                    .trim()
                    .parse::<u16>()
                    .ok()
                    .filter(|&p| p > 0)
                    .unwrap_or(self.config.quick_join_port);
                let (h, p) = split_host_port_input(&self.config.quick_join_host, fallback_p);
                let final_p = if p > 0 { p } else { 7777 };
                self.config.quick_join_host = h.clone();
                self.config.quick_join_port = final_p;
                self.quick_join_port_input = final_p.to_string();
                (h, final_p, self.config.quick_join_password.clone())
            }
        };
        self.save_config();

        // Terraria client connects via IPv4; bridge IPv6 targets over a local 127.0.0.1 forwarder.
        let target_port = if join_port > 0 { join_port } else { 7777 };
        let resolved_v6 = if !join_host.trim().is_empty() {
            self.rt
                .block_on(resolve_ipv6_bridge_target(&join_host, target_port))
        } else {
            None
        };
        let (effective_join_host, effective_join_port) = if let Some(v6_target) = resolved_v6 {
            match self
                .rt
                .block_on(start_ipv4_to_ipv6_client_bridge(v6_target, target_port))
            {
                Ok(bridge) => {
                    let local_port = bridge.local_port();
                    self.client_ipv6_bridge = Some(bridge);
                    ("127.0.0.1".to_string(), local_port)
                }
                Err(err) => {
                    self.notify_err(err.to_string());
                    return;
                }
            }
        } else {
            (join_host, join_port)
        };

        let opts = GameLaunchOptions {
            edition: self.config.active_edition,
            modpack: self.config.active_modpack.clone(),
            join_host: effective_join_host,
            join_port: effective_join_port,
            join_password,
            extra_args: String::new(),
            graphics_backend: self.config.tml_graphics_backend,
            dotnet_tiered_pgo: self.config.dotnet_tiered_pgo,
            dotnet_server_gc: self.config.dotnet_server_gc,
            process_priority: self.config.game_process_priority,
        };

        match launch_game(&self.paths, &opts) {
            Ok(mut proc_handle) => {
                self.running_game = Some(proc_handle.snapshot());
                self.active_game = Some(proc_handle);
                self.external_game = None;
                self.notify_ok(format!("Đã khởi chạy {}.", self.config.active_edition.label()));
            }
            Err(e) => {
                self.stop_client_ipv6_bridge();
                self.notify_err(e.to_string());
            }
        }
    }

    pub fn kill_running_game(&mut self) {
        self.stop_client_ipv6_bridge();
        if let Some(mut proc_handle) = self.active_game.take() {
            let _ = proc_handle.kill();
            self.running_game = None;
            self.external_game = None;
            self.notify_ok("Đã đóng tiến trình game.");
        } else if let Some(ext) = self.external_game.take() {
            if self.sys.is_none() {
                self.sys = System::new().ok();
            }
            let killed = self
                .sys
                .as_mut()
                .is_some_and(|sys| terminate_process_by_pid(sys, ext.pid));
            if killed {
                self.notify_ok("Đã đóng tiến trình game.");
            } else {
                let still_there = self
                    .sys
                    .as_mut()
                    .and_then(|sys| detect_existing_game_process(sys, &self.paths));
                if still_there.is_none() {
                    self.notify_ok("Đã đóng tiến trình game.");
                } else {
                    self.external_game = still_there;
                    self.notify_err(format!("Không thể đóng tiến trình game PID {}.", ext.pid));
                }
            }
        }
    }

    fn make_progress_callback(&self) -> ProgressCallback {
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        Arc::new(move |prog: InstallProgress| {
            let _ = tx.send(AppEvent::InstallProgress(prog));
            wake.wake();
        })
    }

    fn can_start_install(&mut self, overwriting_edition: Option<GameEdition>) -> bool {
        if let Some(edition) = overwriting_edition {
            if self.is_game_running() {
                self.notify_err("Vui lòng đóng game trước khi cài đặt/cập nhật.");
                return false;
            }
            let supervised_conflict = self.server_supervisors.iter().any(|(id, sup)| {
                if !sup.is_active() {
                    return false;
                }
                let running_edition = self
                    .server_snapshots
                    .get(id)
                    .map(|s| s.edition)
                    .or_else(|| {
                        self.config
                            .servers
                            .iter()
                            .find(|p| p.id == *id)
                            .map(|p| p.edition)
                    });
                running_edition == Some(edition)
            });
            let external_conflict = self
                .external_servers
                .iter()
                .any(|ext| ext.edition == edition);
            if supervised_conflict || external_conflict {
                self.notify_err(format!(
                    "Vui lòng tắt server {} đang chạy trước khi cài đặt/cập nhật.",
                    edition.label()
                ));
                return false;
            }
        }
        if self.install_busy {
            self.notify_err("Đang có tiến trình cài đặt khác đang chạy, vui lòng chờ.");
            return false;
        }
        true
    }

    pub fn refresh_tmodloader_releases(&mut self) {
        if self.tml_releases_busy {
            return;
        }
        self.tml_releases_busy = true;
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let res = fetch_tmodloader_releases(proxy.as_deref())
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::TmlReleasesFetched(res));
            wake.wake();
        });
    }

    pub fn install_tmodloader_selected(&mut self) {
        if !self.can_start_install(Some(GameEdition::TModLoader)) {
            return;
        }
        let also_install_dotnet = self.env.dotnet8_version.is_none();
        let selected_release = if self.tml_releases.is_empty() {
            None
        } else {
            self.tml_releases
                .iter()
                .find(|r| r.tag_name == self.selected_tml_tag)
                .or_else(|| pick_default_tml_release(&self.tml_releases))
                .cloned()
        };

        self.install_busy = true;
        self.install_last_error = None;
        self.install_progress = Some(InstallProgress {
            target: InstallTarget::TModLoader,
            stage: if let Some(rel) = &selected_release {
                format!("Chuẩn bị cài tModLoader {}...", rel.tag_name)
            } else {
                "Đang lấy bản tModLoader ổn định mới nhất...".to_string()
            },
            current: 0,
            total: None,
        });

        let paths = self.paths.clone();
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        let selected_tag = self.selected_tml_tag.trim().to_string();
        let cb = self.make_progress_callback();
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();

        self.rt.spawn(async move {
            let result = match selected_release {
                Some(release) => install_tmodloader_release(
                    &paths,
                    &release,
                    proxy.as_deref(),
                    also_install_dotnet,
                    cb,
                )
                .await
                .map_err(|e| e.to_string()),
                None => match fetch_tmodloader_releases(proxy.as_deref()).await {
                    Ok(releases) => {
                        let chosen = if !selected_tag.is_empty() {
                            releases
                                .iter()
                                .find(|r| r.tag_name == selected_tag)
                                .or_else(|| pick_default_tml_release(&releases))
                                .cloned()
                        } else {
                            pick_default_tml_release(&releases).cloned()
                        };
                        let _ = tx.send(AppEvent::TmlReleasesFetched(Ok(releases)));
                        wake.wake();
                        match chosen {
                            Some(release) => install_tmodloader_release(
                                &paths,
                                &release,
                                proxy.as_deref(),
                                also_install_dotnet,
                                cb,
                            )
                            .await
                            .map_err(|e| e.to_string()),
                            None => Err(
                                "Không tìm thấy bản phát hành tModLoader nào trên GitHub."
                                    .to_string(),
                            ),
                        }
                    }
                    Err(e) => Err(e.to_string()),
                },
            };
            let _ = tx.send(AppEvent::InstallFinished {
                target: InstallTarget::TModLoader,
                result,
            });
            wake.wake();
        });
    }

    pub fn install_dotnet8(&mut self) {
        if !self.can_start_install(None) {
            return;
        }
        self.install_busy = true;
        self.install_last_error = None;
        self.install_progress = Some(InstallProgress {
            target: InstallTarget::DotNet8,
            stage: "Đang chuẩn bị cài .NET 8 Portable...".to_string(),
            current: 0,
            total: None,
        });
        let paths = self.paths.clone();
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        let cb = self.make_progress_callback();
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let result = install_dotnet8_portable(&paths, proxy.as_deref(), cb)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::InstallFinished {
                target: InstallTarget::DotNet8,
                result,
            });
            wake.wake();
        });
    }

    pub fn effective_terraria_source(&self) -> &str {
        non_empty_opt(&self.config.terraria_install_source)
            .unwrap_or(DEFAULT_TERRARIA_INSTALL_SOURCE)
    }

    pub fn refresh_terraria_update_info(&mut self) {
        if self.terraria_check_busy {
            return;
        }
        self.terraria_check_busy = true;
        let source = self.effective_terraria_source().to_string();
        let token = non_empty_opt(&self.config.github_release_token).map(ToString::to_string);
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let res = fetch_terraria_branch_manifest(&source, token.as_deref(), proxy.as_deref())
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::TerrariaManifestChecked(res));
            wake.wake();
        });
    }

    pub fn install_terraria_url(&mut self) {
        if !self.can_start_install(Some(GameEdition::Vanilla)) {
            return;
        }
        let source = self.effective_terraria_source().to_string();
        self.save_config();
        self.install_busy = true;
        self.install_last_error = None;
        self.install_progress = Some(InstallProgress {
            target: InstallTarget::Terraria,
            stage: "Đang kết nối nguồn tải Terraria...".to_string(),
            current: 0,
            total: None,
        });
        let paths = self.paths.clone();
        let token = non_empty_opt(&self.config.github_release_token).map(ToString::to_string);
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        let cb = self.make_progress_callback();
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let result = install_terraria_from_source(
                &paths,
                &source,
                token.as_deref(),
                proxy.as_deref(),
                cb,
            )
            .await
            .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::InstallFinished {
                target: InstallTarget::Terraria,
                result,
            });
            wake.wake();
        });
    }

    pub fn pick_and_install_terraria_zip(&mut self) {
        if !self.can_start_install(Some(GameEdition::Vanilla)) {
            return;
        }
        let Some(zip_path) = rfd::FileDialog::new()
            .add_filter("Terraria Archive", &["zip"])
            .pick_file()
        else {
            return;
        };
        self.install_busy = true;
        self.install_last_error = None;
        self.install_progress = Some(InstallProgress {
            target: InstallTarget::Terraria,
            stage: "Đang giải nén gói Terraria...".to_string(),
            current: 0,
            total: None,
        });
        let paths = self.paths.clone();
        let cb = self.make_progress_callback();
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn_blocking(move || {
            let result =
                install_terraria_from_local_zip(&paths, &zip_path, cb).map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::InstallFinished {
                target: InstallTarget::Terraria,
                result,
            });
            wake.wake();
        });
    }

    pub fn install_xna(&mut self) {
        let msi = self.paths.xna_msi_path();
        if msi.is_file() {
            match install_xna_framework_elevated(&msi) {
                Ok(()) => self.notify_ok(
                    "Đã mở trình cài đặt XNA 4.0. Nhấn 'Làm mới kiểm tra' sau khi cài xong.",
                ),
                Err(e) => self.notify_err(e.to_string()),
            }
            return;
        }
        if !self.can_start_install(None) {
            return;
        }
        self.install_busy = true;
        self.install_last_error = None;
        self.install_progress = Some(InstallProgress {
            target: InstallTarget::Xna40,
            stage: "Đang tải Microsoft XNA Framework 4.0...".to_string(),
            current: 0,
            total: None,
        });
        let paths = self.paths.clone();
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        let cb = self.make_progress_callback();
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let result = download_and_install_xna(&paths, proxy.as_deref(), cb)
                .await
                .map(|()| {
                    "Đã chạy trình cài đặt XNA 4.0. Nhấn 'Làm mới kiểm tra' sau khi cài xong."
                        .to_string()
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::InstallFinished {
                target: InstallTarget::Xna40,
                result,
            });
            wake.wake();
        });
    }

    pub fn start_selected_server(&mut self) {
        let Some(profile) = self.config.active_server().cloned() else {
            self.notify_err("Chưa có cấu hình server nào.");
            return;
        };
        if self
            .server_supervisors
            .get(&profile.id)
            .is_some_and(|s| s.is_active())
        {
            self.notify_err("Server của hồ sơ này đang chạy.");
            return;
        }
        if let Some(ext) = self.external_server_for_profile(&profile.id) {
            self.notify_err(format!(
                "Server của hồ sơ này đang chạy từ phiên trước (PID {}). Hãy tắt trước khi khởi chạy mới.",
                ext.pid
            ));
            return;
        }
        self.save_config();
        if profile.config.port == 0 {
            self.notify_err("Cổng TCP Server phải từ 1 đến 65535");
            return;
        }
        let port_used_by_other = self.server_supervisors.iter().any(|(id, sup)| {
            id != &profile.id
                && sup.is_active()
                && self.server_snapshots.get(id).map(|s| s.port) == Some(profile.config.port)
        }) || self
            .external_servers
            .iter()
            .any(|ext| ext.port == Some(profile.config.port));
        if port_used_by_other || self.rt.block_on(probe_local_tcp_listening(profile.config.port)) {
            self.scan_external_servers();
            self.notify_err(format!(
                "Cổng {} đang được tiến trình khác sử dụng (server khác hoặc Host & Play trong game). Hãy đổi cổng hoặc tắt tiến trình đang giữ cổng này.",
                profile.config.port
            ));
            return;
        }

        let handle = self.rt.handle().clone();
        let tuning = RuntimeTuning {
            tiered_pgo: self.config.dotnet_tiered_pgo,
            server_gc: self.config.dotnet_server_gc,
            process_priority: self.config.game_process_priority,
            disable_builtin_upnp: true,
        };
        let profile_id = profile.id.clone();
        let profile_name = profile.name.clone();
        match start_server(
            &handle,
            self.paths.clone(),
            profile,
            tuning,
            self.repaint.clone(),
        ) {
            Ok(sup) => {
                if self.sys.is_none() {
                    self.sys = System::new().ok();
                }
                if let Some(snap) = self.sys.as_mut().map(|sys| sup.snapshot(sys)) {
                    self.server_snapshots.insert(profile_id.clone(), snap);
                } else {
                    self.server_snapshots.remove(&profile_id);
                }
                self.server_supervisors.insert(profile_id.clone(), sup);
                self.external_log_summaries.remove(&profile_id);
                self.cleared_external_log_pids.remove(&profile_id);
                self.external_servers
                    .retain(|e| e.profile_id.as_deref() != Some(profile_id.as_str()));
                self.notify_ok(format!("Đang khởi chạy {profile_name}..."));
            }
            Err(e) => self.notify_err(e.to_string()),
        }
    }

    pub fn scan_external_servers(&mut self) {
        self.last_external_scan_at = Instant::now();
        if self.sys.is_none() {
            self.sys = System::new().ok();
        }
        self.external_game = if self.active_game.is_some() {
            None
        } else {
            self.sys
                .as_mut()
                .and_then(|sys| detect_existing_game_process(sys, &self.paths))
        };

        let mut supervised_pids = HashSet::new();
        let mut supervised_profile_ids = HashSet::new();
        for (profile_id, sup) in &self.server_supervisors {
            if sup.is_active() {
                supervised_profile_ids.insert(profile_id.clone());
                if let Some(pid) = self.server_snapshots.get(profile_id).and_then(|s| s.pid) {
                    supervised_pids.insert(pid);
                }
            }
        }

        let detected = match self.sys.as_mut() {
            Some(sys) => detect_existing_server_processes(sys, &self.paths),
            None => Vec::new(),
        };

        let next: Vec<DetectedExternalServer> = detected
            .into_iter()
            .filter(|d| {
                !supervised_pids.contains(&d.pid)
                    && !d
                        .profile_id
                        .as_deref()
                        .is_some_and(|pid| supervised_profile_ids.contains(pid))
            })
            .collect();

        let mut next_summaries = HashMap::new();
        for ext in &next {
            if let Some(profile_id) = ext.profile_id.as_deref() {
                let mut summary = read_server_log_summary(&self.paths, profile_id, 250);
                if self.cleared_external_log_pids.get(profile_id) == Some(&ext.pid) {
                    summary.lines.clear();
                }
                next_summaries.insert(profile_id.to_string(), summary);
            }
        }
        self.cleared_external_log_pids.retain(|profile_id, pid| {
            next.iter().any(|ext| {
                ext.profile_id.as_deref() == Some(profile_id.as_str()) && ext.pid == *pid
            })
        });
        self.external_log_summaries = next_summaries;

        let prev_keys: Vec<(u32, Option<u16>)> = self
            .external_servers
            .iter()
            .map(|e| (e.pid, e.port))
            .collect();
        let next_keys: Vec<(u32, Option<u16>)> = next.iter().map(|e| (e.pid, e.port)).collect();
        let changed = prev_keys != next_keys;
        self.external_servers = next;
        if changed {
            self.spawn_network_probe();
        }
    }

    fn profile_effective_port(&self, profile_id: &str) -> Option<u16> {
        self.server_snapshots
            .get(profile_id)
            .map(|s| s.port)
            .filter(|&p| p > 0)
            .or_else(|| {
                self.config
                    .servers
                    .iter()
                    .find(|p| p.id == profile_id)
                    .map(|p| p.config.port)
                    .filter(|&p| p > 0)
            })
            .or_else(|| {
                self.external_server_for_profile(profile_id)
                    .and_then(|e| e.port)
                    .filter(|&p| p > 0)
            })
    }

    fn is_port_used_by_other_active_server(
        &self,
        port: u16,
        ignore_profile_id: Option<&str>,
        ignore_ext_pid: Option<u32>,
    ) -> bool {
        if port == 0 {
            return false;
        }
        let used_by_supervised = self.server_supervisors.iter().any(|(id, sup)| {
            if ignore_profile_id == Some(id.as_str()) || !sup.is_active() {
                return false;
            }
            let snap = self.server_snapshots.get(id);
            if snap.is_some_and(|s| {
                matches!(
                    s.state,
                    ServerLifecycleState::Stopping
                        | ServerLifecycleState::Stopped
                        | ServerLifecycleState::Crashed(_)
                )
            }) {
                return false;
            }
            let srv_port = snap
                .map(|s| s.port)
                .filter(|&p| p > 0)
                .or_else(|| {
                    self.config
                        .servers
                        .iter()
                        .find(|p| p.id == *id)
                        .map(|p| p.config.port)
                });
            srv_port == Some(port)
        });
        if used_by_supervised {
            return true;
        }
        self.external_servers.iter().any(|ext| {
            if ignore_ext_pid == Some(ext.pid) {
                return false;
            }
            if let (Some(ign_id), Some(ext_id)) = (ignore_profile_id, ext.profile_id.as_deref()) {
                if ign_id == ext_id {
                    return false;
                }
            }
            let ext_port = ext.port.or_else(|| {
                ext.profile_id
                    .as_deref()
                    .and_then(|pid| self.config.servers.iter().find(|p| p.id == pid))
                    .map(|p| p.config.port)
            });
            ext_port == Some(port)
        })
    }

    fn cleanup_host_for_stopped_port(
        &mut self,
        port: u16,
        ignore_profile_id: Option<&str>,
        ignore_ext_pid: Option<u32>,
    ) {
        if self.is_port_used_by_other_active_server(port, ignore_profile_id, ignore_ext_pid) {
            return;
        }
        let stop_direct = match self.active_direct_host_status() {
            DirectHostStatus::Starting { local_port, .. } => local_port == port,
            DirectHostStatus::Running(info) => info.local_port == port,
            DirectHostStatus::Stopped | DirectHostStatus::Error(_) => {
                self.direct_host_handle.is_some()
            }
            DirectHostStatus::Idle => false,
        };
        if stop_direct {
            self.stop_direct_host();
        }
    }

    pub fn stop_external_pid(&mut self, pid: u32) {
        let Some(ext) = self
            .external_servers
            .iter()
            .find(|e| e.pid == pid)
            .cloned()
        else {
            self.scan_external_servers();
            return;
        };
        let ext_port = ext
            .port
            .filter(|&p| p > 0)
            .or_else(|| {
                ext.profile_id
                    .as_deref()
                    .and_then(|id| self.profile_effective_port(id))
            })
            .unwrap_or_else(|| self.active_server_port());
        if self.sys.is_none() {
            self.sys = System::new().ok();
        }
        let killed = match self.sys.as_mut() {
            Some(sys) => terminate_process_by_pid(sys, ext.pid),
            None => false,
        };
        if let Some(ref ext_profile_id) = ext.profile_id {
            if self.pending_auto_direct_host_profile_id.as_deref()
                == Some(ext_profile_id.as_str())
            {
                self.pending_auto_direct_host_profile_id = None;
            }
        }
        if killed {
            self.external_servers.retain(|e| e.pid != ext.pid);
            if let Some(ref ext_profile_id) = ext.profile_id {
                if !self
                    .external_servers
                    .iter()
                    .any(|e| e.profile_id.as_deref() == Some(ext_profile_id.as_str()))
                {
                    self.external_log_summaries.remove(ext_profile_id);
                    self.cleared_external_log_pids.remove(ext_profile_id);
                }
            }
            self.cleanup_host_for_stopped_port(ext_port, ext.profile_id.as_deref(), Some(ext.pid));
            self.spawn_network_probe();
            self.notify_ok(format!(
                "Đã tắt {} phiên trước (PID {}).",
                ext.edition.label(),
                ext.pid
            ));
        } else {
            self.scan_external_servers();
            if !self.external_servers.iter().any(|e| e.pid == ext.pid) {
                self.cleanup_host_for_stopped_port(
                    ext_port,
                    ext.profile_id.as_deref(),
                    Some(ext.pid),
                );
                self.spawn_network_probe();
            }
            self.notify_err(format!(
                "Không thể tắt tiến trình server PID {}. Tiến trình có thể đã đóng hoặc thiếu quyền.",
                ext.pid
            ));
        }
    }

    pub fn stop_external_server(&mut self) {
        if let Some(pid) = self.active_external_server().map(|e| e.pid) {
            self.stop_external_pid(pid);
        }
    }

    pub fn stop_server_profile(&mut self, profile_id: &str) {
        if self.pending_auto_direct_host_profile_id.as_deref() == Some(profile_id) {
            self.pending_auto_direct_host_profile_id = None;
        }
        let is_sup_active = self
            .server_supervisors
            .get(profile_id)
            .is_some_and(|s| s.is_active());
        if is_sup_active {
            let port = self
                .profile_effective_port(profile_id)
                .unwrap_or_else(|| self.active_server_port());
            let was_stopping = self
                .server_supervisors
                .get(profile_id)
                .is_some_and(|s| s.is_stopping());
            if let Some(sup) = self.server_supervisors.get(profile_id) {
                sup.request_graceful_stop();
            }
            self.cleanup_host_for_stopped_port(port, Some(profile_id), None);
            let name = self
                .config
                .servers
                .iter()
                .find(|p| p.id == profile_id)
                .map(|p| p.name.as_str())
                .unwrap_or("server");
            if was_stopping {
                self.notify_ok(format!(
                    "Server chưa thoát kịp, đã buộc dừng {name}."
                ));
            } else {
                self.notify_ok(format!("Đang lưu thế giới & tắt {name}..."));
            }
            return;
        }
        if let Some(pid) = self
            .external_server_for_profile(profile_id)
            .map(|e| e.pid)
        {
            self.stop_external_pid(pid);
            return;
        }
        if profile_id == self.config.active_server_id {
            if let Some(pid) = self.active_external_server().map(|e| e.pid) {
                self.stop_external_pid(pid);
            }
        }
    }

    pub fn stop_selected_server(&mut self) {
        let id = self.config.active_server_id.clone();
        self.stop_server_profile(&id);
    }

    pub fn force_kill_server_profile(&mut self, profile_id: &str) {
        if self.pending_auto_direct_host_profile_id.as_deref() == Some(profile_id) {
            self.pending_auto_direct_host_profile_id = None;
        }
        let is_sup_active = self
            .server_supervisors
            .get(profile_id)
            .is_some_and(|s| s.is_active());
        if is_sup_active {
            let port = self
                .profile_effective_port(profile_id)
                .unwrap_or_else(|| self.active_server_port());
            if let Some(sup) = self.server_supervisors.get(profile_id) {
                sup.force_kill();
            }
            self.cleanup_host_for_stopped_port(port, Some(profile_id), None);
            self.notify_ok("Đã buộc dừng tiến trình server.");
            return;
        }
        if let Some(pid) = self
            .external_server_for_profile(profile_id)
            .map(|e| e.pid)
        {
            self.stop_external_pid(pid);
        }
    }

    pub fn force_kill_server(&mut self) {
        let id = self.config.active_server_id.clone();
        self.force_kill_server_profile(&id);
    }

    pub fn stop_all_servers(&mut self) {
        self.pending_auto_direct_host_profile_id = None;
        let mut stopped_supervised = 0usize;
        for sup in self.server_supervisors.values() {
            if sup.is_active() {
                sup.request_graceful_stop();
                stopped_supervised += 1;
            }
        }

        let ext_list = self.external_servers.clone();
        let mut killed_ext = 0usize;
        let mut failed_ext_pids = Vec::new();
        if !ext_list.is_empty() {
            if self.sys.is_none() {
                self.sys = System::new().ok();
            }
            for ext in ext_list {
                let killed = match self.sys.as_mut() {
                    Some(sys) => terminate_process_by_pid(sys, ext.pid),
                    None => false,
                };
                if killed {
                    killed_ext += 1;
                    self.external_servers.retain(|e| e.pid != ext.pid);
                } else {
                    failed_ext_pids.push(ext.pid);
                }
            }
            if killed_ext > 0 {
                let remaining_profiles: HashSet<String> = self
                    .external_servers
                    .iter()
                    .filter_map(|e| e.profile_id.clone())
                    .collect();
                self.external_log_summaries
                    .retain(|k, _| remaining_profiles.contains(k));
                self.cleared_external_log_pids
                    .retain(|k, _| remaining_profiles.contains(k));
                self.spawn_network_probe();
            }
        }

        if failed_ext_pids.is_empty() {
            self.stop_direct_host();
        } else {
            self.scan_external_servers();
            let check_port = match self.active_direct_host_status() {
                DirectHostStatus::Starting { local_port, .. } => local_port,
                DirectHostStatus::Running(info) => info.local_port,
                _ => self.active_server_port(),
            };
            let still_used = self.external_servers.iter().any(|ext| {
                ext.port
                    .or_else(|| {
                        ext.profile_id
                            .as_deref()
                            .and_then(|pid| self.config.servers.iter().find(|p| p.id == pid))
                            .map(|p| p.config.port)
                    })
                    == Some(check_port)
            });
            if !still_used {
                self.stop_direct_host();
            }
        }

        if !failed_ext_pids.is_empty() {
            let pid_list = failed_ext_pids
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            self.notify_err(format!(
                "Đã dừng {stopped_supervised} server đang quản lý và {killed_ext} server phiên trước, nhưng không tắt được PID: {pid_list}."
            ));
        } else if stopped_supervised > 0 && killed_ext > 0 {
            self.notify_ok(format!(
                "Đã gửi lệnh lưu & tắt {stopped_supervised} server và tắt {killed_ext} server phiên trước."
            ));
        } else if stopped_supervised > 0 {
            self.notify_ok(format!(
                "Đã gửi lệnh save & exit cho {stopped_supervised} server."
            ));
        } else if killed_ext > 0 {
            self.notify_ok(format!("Đã tắt {killed_ext} server phiên trước."));
        } else {
            self.notify_ok("Không có server nào đang chạy.");
        }
    }

    pub fn clear_active_server_logs(&mut self) {
        let active_id = self.config.active_server_id.clone();
        if let Some(sup) = self.server_supervisors.get(&active_id) {
            if sup.is_active() || !self.showing_external_server() {
                sup.clear_logs();
                return;
            }
        }
        if let Some(ext) = self.active_external_server().cloned() {
            if let Some(profile_id) = ext.profile_id {
                self.cleared_external_log_pids
                    .insert(profile_id.clone(), ext.pid);
                if let Some(summary) = self.external_log_summaries.get_mut(&profile_id) {
                    summary.lines.clear();
                }
            }
        }
    }

    pub fn send_server_command(&mut self, cmd: &str) {
        if let Some(sup) = self.active_server_supervisor() {
            if sup.is_active() {
                if let Err(e) = sup.send_command(cmd) {
                    self.notify_err(e.to_string());
                }
                return;
            }
        }
        if self.active_external_server().is_some() {
            self.notify_err(
                "Server từ phiên trước chỉ hỗ trợ xem log; hãy tắt và khởi chạy lại từ Launcher để gửi lệnh điều khiển.",
            );
        } else {
            self.notify_err("Server chưa chạy.");
        }
    }

    pub fn kick_player(&mut self, name: &str) {
        self.send_server_command(&format!("kick {name}"));
    }

    pub fn ban_player(&mut self, name: &str) {
        self.send_server_command(&format!("ban {name}"));
        self.refresh_banlist();
    }

    pub fn remove_ban_entry(&mut self, idx: usize) {
        let Some(profile) = self.config.active_server().cloned() else {
            return;
        };
        if idx < self.banlist_entries.len() {
            self.banlist_entries.remove(idx);
            let path = self.resolve_profile_banlist_path(&profile);
            if let Err(e) = save_banlist(&path, &self.banlist_entries) {
                self.notify_err(format!("Không lưu được banlist: {e}"));
            } else {
                self.notify_ok("Đã cập nhật danh sách cấm.");
            }
        }
    }

    pub fn add_manual_ban(&mut self) {
        let ip = self.manual_ban_ip.trim().to_string();
        if ip.is_empty() {
            self.notify_err("Nhập địa chỉ IP cần cấm.");
            return;
        }
        let Some(profile) = self.config.active_server().cloned() else {
            return;
        };
        let label = if self.manual_ban_name.trim().is_empty() {
            "Cấm thủ công".to_string()
        } else {
            self.manual_ban_name.trim().to_string()
        };
        self.banlist_entries.push(BanEntry { label, ip });
        let path = self.resolve_profile_banlist_path(&profile);
        if let Err(e) = save_banlist(&path, &self.banlist_entries) {
            self.notify_err(format!("Không lưu được banlist: {e}"));
        } else {
            self.manual_ban_name.clear();
            self.manual_ban_ip.clear();
            self.notify_ok("Đã thêm vào banlist.txt.");
        }
    }

    pub fn spawn_network_probe(&mut self) {
        if self.network_loading {
            self.network_reprobe_queued = true;
            return;
        }
        self.network_loading = true;
        self.network_reprobe_queued = false;
        let port = self.active_server_port();
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let snap = probe_network_snapshot(port, proxy.as_deref()).await;
            let _ = tx.send(AppEvent::NetworkProbed(snap));
            wake.wake();
        });
    }

    pub fn ensure_firewall(&mut self, port: u16) {
        if self.firewall_busy || port == 0 {
            return;
        }
        self.firewall_busy = true;
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn_blocking(move || {
            let res = ensure_firewall_rule(port).map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::FirewallResult(res));
            wake.wake();
        });
    }

    pub fn remove_firewall(&mut self, port: u16) {
        if self.firewall_busy || port == 0 {
            return;
        }
        self.firewall_busy = true;
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn_blocking(move || {
            let res = remove_firewall_rule(port).map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::FirewallResult(res));
            wake.wake();
        });
    }

    pub fn active_direct_host_status(&self) -> DirectHostStatus {
        self.direct_host_handle
            .as_ref()
            .map(DirectPortHostHandle::current_status)
            .unwrap_or(DirectHostStatus::Idle)
    }

    pub fn active_online_endpoint(&self) -> Option<String> {
        if let DirectHostStatus::Running(info) = self.active_direct_host_status() {
            return info.preferred_online_endpoint();
        }
        None
    }

    pub fn stop_direct_host(&mut self) {
        if let Some(h) = self.direct_host_handle.take() {
            h.stop();
        }
    }

    fn start_direct_host_on_port(&mut self, local_target_port: u16) {
        if matches!(
            self.active_direct_host_status(),
            DirectHostStatus::Running(_) | DirectHostStatus::Starting { .. }
        ) {
            self.notify_ok("Port mạng đang mở.");
            return;
        }
        if local_target_port == 0 {
            self.notify_err("Cổng TCP Server phải từ 1 đến 65535");
            return;
        }
        let host_port = self
            .direct_host_port_input
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|&p| p > 0)
            .unwrap_or(local_target_port);
        if !self.firewall_busy && !check_firewall_rule_exists(host_port) {
            self.ensure_firewall(host_port);
        }
        if let Some(prev) = self.direct_host_handle.take() {
            prev.stop();
        }
        let handle = start_direct_port_host(
            self.rt.handle(),
            DirectHostConfig {
                local_target_port,
                host_port,
                bind_ipv4: self.direct_bind_ip,
                try_upnp: self.direct_try_upnp,
            },
            self.repaint.clone(),
        );
        self.direct_host_handle = Some(handle);
        self.notify_ok("Đang mở Port mạng...");
    }

    pub fn toggle_direct_host(&mut self) {
        self.pending_auto_direct_host_profile_id = None;
        if matches!(
            self.active_direct_host_status(),
            DirectHostStatus::Running(_) | DirectHostStatus::Starting { .. }
        ) {
            self.stop_direct_host();
            self.notify_ok("Đã đóng Port mạng hiện tại.");
            return;
        }
        let trimmed_port = self.direct_host_port_input.trim();
        let local_target_port = self.active_server_port();
        if !trimmed_port.is_empty() {
            let valid_port = trimmed_port.parse::<u16>().ok().filter(|&p| p > 0);
            if valid_port.is_none() {
                self.notify_err("Cổng mở không hợp lệ (1 - 65535).");
                return;
            }
        } else {
            self.direct_host_port_input = local_target_port.to_string();
        }
        self.start_direct_host_on_port(local_target_port);
    }

    pub fn start_self_host_online(&mut self) {
        let port = self.active_server_port();
        if port == 0 {
            self.notify_err("Cổng TCP Server phải từ 1 đến 65535");
            return;
        }
        let active_id = self
            .config
            .active_server()
            .map(|p| p.id.clone())
            .unwrap_or_else(|| self.config.active_server_id.clone());
        let srv_active = self
            .active_server_supervisor()
            .is_some_and(|s| s.is_active());
        if srv_active {
            match self.active_server_snapshot().map(|s| &s.state) {
                Some(ServerLifecycleState::Running) => {
                    self.pending_auto_direct_host_profile_id = None;
                    self.start_direct_host_on_port(port);
                }
                Some(ServerLifecycleState::Stopping) => {
                    self.notify_err("Server đang dừng — vui lòng chờ tắt hẳn rồi bật lại.");
                }
                _ => {
                    if matches!(
                        self.active_direct_host_status(),
                        DirectHostStatus::Running(_) | DirectHostStatus::Starting { .. }
                    ) {
                        self.notify_ok("Port mạng đang mở.");
                    } else {
                        self.pending_auto_direct_host_profile_id = Some(active_id);
                        self.notify_ok(
                            "Server đang khởi động — sẽ tự mở Port mạng ngay khi sẵn sàng.",
                        );
                    }
                }
            }
            return;
        }

        if self.rt.block_on(probe_local_tcp_listening(port)) {
            self.pending_auto_direct_host_profile_id = None;
            self.start_direct_host_on_port(port);
            self.spawn_network_probe();
            return;
        }

        self.start_selected_server();
        if self
            .server_supervisors
            .get(&active_id)
            .is_some_and(|s| s.is_active())
        {
            self.pending_auto_direct_host_profile_id = Some(active_id);
            self.notify_ok("Đang khởi chạy Server và tự động mở Port mạng...");
        }
    }

    pub fn join_own_hosted_server(&mut self) {
        if self.active_game.is_some() {
            self.notify_err("Game đang chạy. Hãy đóng phiên hiện tại trước khi mở mới.");
            return;
        }
        if let Some(ext) = &self.external_game {
            let msg = format!(
                "{} đang mở sẵn (PID {}). Hãy kết nối trực tiếp trong menu Multiplayer của game hoặc nhấn 'Đóng Game' trước khi mở mới.",
                ext.edition.label(),
                ext.pid
            );
            self.notify_err(msg);
            return;
        }
        let srv_active = self
            .active_server_supervisor()
            .is_some_and(|s| s.is_active());
        let ext_server = if srv_active {
            None
        } else {
            self.active_external_server()
        };
        let matched_profile = ext_server
            .and_then(|ext| ext.profile_id.as_deref())
            .and_then(|id| self.config.servers.iter().find(|s| s.id == id))
            .or_else(|| self.config.active_server());
        let server_password = matched_profile
            .map(|s| s.config.password.clone())
            .unwrap_or_default();
        let server_edition = if srv_active {
            self.active_server_snapshot()
                .map(|s| s.edition)
                .or_else(|| self.config.active_server().map(|s| s.edition))
                .unwrap_or(self.config.active_edition)
        } else if let Some(ext) = ext_server {
            ext.edition
        } else {
            self.config
                .active_server()
                .map(|s| s.edition)
                .unwrap_or(self.config.active_edition)
        };
        let target_port = if srv_active {
            self.active_server_port()
        } else {
            ext_server
                .and_then(|ext| ext.port)
                .filter(|&p| p > 0)
                .unwrap_or_else(|| self.active_server_port())
        };
        let edition_changed = self.config.active_edition != server_edition;
        self.config.active_edition = server_edition;
        if edition_changed {
            self.refresh_worlds_and_backups();
        }
        self.config.quick_join_host = "127.0.0.1".to_string();
        self.config.quick_join_port = target_port;
        self.quick_join_port_input = target_port.to_string();
        self.config.quick_join_password = server_password;
        self.save_config();
        self.launch_selected_game(None);
    }

    pub fn probe_port(&mut self) {
        if self.port_probe_busy {
            return;
        }
        let fallback_port = self
            .port_probe_port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|&p| p > 0)
            .unwrap_or_else(|| self.active_server_port());
        let (split_host, port) = split_host_port_input(&self.port_probe_host, fallback_port);
        if split_host.trim().is_empty() {
            self.notify_err("Nhập địa chỉ Host / IP cần kiểm tra.");
            return;
        }
        if port == 0 {
            self.notify_err("Cổng kiểm tra không hợp lệ.");
            return;
        }
        self.port_probe_host = split_host.clone();
        self.port_probe_port = port.to_string();
        let unbracketed = split_host
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .map(str::trim)
            .unwrap_or(split_host.trim());
        let clean_host = unbracketed
            .split('%')
            .next()
            .unwrap_or(unbracketed)
            .trim()
            .to_string();
        if clean_host.is_empty() {
            self.notify_err("Nhập địa chỉ Host / IP cần kiểm tra.");
            return;
        }
        let local_port = self.active_server_port();
        let is_local_target = is_local_probe_host(&clean_host, &self.network_snapshot);
        let endpoint = if clean_host.contains(':') {
            format!("[{clean_host}]:{port}")
        } else {
            format!("{clean_host}:{port}")
        };
        self.port_probe_busy = true;
        self.port_probe_status = None;
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let result = if is_local_target {
                if port == local_port && !probe_local_tcp_listening(local_port).await {
                    Err(format!(
                        "Cổng máy bạn ({clean_host}:{port}) chưa lắng nghe — hãy bật Server trước."
                    ))
                } else {
                    probe_local_endpoint_listening(&clean_host, port).await
                }
            } else {
                probe_tcp_endpoint(&clean_host, port).await
            };
            let _ = tx.send(AppEvent::PortProbed { endpoint, result });
            wake.wake();
        });
    }

    pub fn import_mods_dialog(&mut self) {
        if let Some(files) = rfd::FileDialog::new()
            .add_filter("tModLoader Mods", &["tmod", "zip"])
            .pick_files()
        {
            match import_local_mod_files(&files, &self.paths, true) {
                Ok(imported) => {
                    self.refresh_mods_and_packs();
                    self.notify_ok(format!("Đã nhập và bật {} mod.", imported.len()));
                }
                Err(e) => self.notify_err(e.to_string()),
            }
        }
    }

    pub fn toggle_mod(&mut self, internal_name: &str, enable: bool) {
        match set_mod_enabled(&self.paths, internal_name, enable) {
            Ok(_) => self.refresh_mods_and_packs(),
            Err(e) => self.notify_err(e.to_string()),
        }
    }

    pub fn delete_mod(&mut self, mod_path: &std::path::Path, internal_name: &str) {
        match delete_installed_mod(&self.paths, mod_path, internal_name) {
            Ok(()) => {
                self.refresh_mods_and_packs();
                self.notify_ok(format!("Đã xóa mod {internal_name}."));
            }
            Err(e) => self.notify_err(e.to_string()),
        }
    }

    pub fn create_new_modpack(&mut self) {
        let _ = self.create_modpack_internal();
    }

    fn create_modpack_internal(&mut self) -> Option<ModPackInfo> {
        let enabled = self.enabled_mod_names();
        let metas: Vec<TmodMetadata> = self
            .installed_mods
            .iter()
            .map(|m| m.meta.clone())
            .collect();
        let name = self.new_modpack_name.trim().to_string();
        match create_modpack(
            &self.paths.tml_modpacks_dir(),
            &self.paths.tml_mods_dir(),
            &name,
            &enabled,
            &metas,
        ) {
            Ok(pack) => {
                self.sync_install_txt_for_pack(&pack);
                self.new_modpack_name.clear();
                self.config.active_modpack = pack.name.clone();
                self.save_config();
                self.refresh_mods_and_packs();
                self.notify_ok(format!("Đã tạo ModPack '{}'.", pack.name));
                let refreshed = self
                    .modpacks
                    .iter()
                    .find(|p| p.name == pack.name)
                    .cloned()
                    .unwrap_or(pack);
                Some(refreshed)
            }
            Err(e) => {
                self.notify_err(e.to_string());
                None
            }
        }
    }

    pub fn activate_selected_modpack(&mut self, pack: &ModPackInfo) {
        let metas: Vec<TmodMetadata> = self
            .installed_mods
            .iter()
            .map(|m| m.meta.clone())
            .collect();
        let _ = backup_unsaved_enabled_mods_as_modpack(
            &self.paths.tml_modpacks_dir(),
            &self.paths.tml_mods_dir(),
            &self.config.active_modpack,
            &metas,
            "ModCu_TruocChuyen",
        );
        match activate_modpack(&pack.path, &self.paths.tml_mods_dir(), true) {
            Ok(enabled) => {
                self.config.active_modpack = pack.name.clone();
                self.save_config();
                self.refresh_mods_and_packs();
                self.notify_ok(format!(
                    "Đã kích hoạt ModPack '{}' ({} mod).",
                    pack.name,
                    enabled.len()
                ));
            }
            Err(e) => self.notify_err(e.to_string()),
        }
    }

    pub fn remove_modpack(&mut self, pack: &ModPackInfo) {
        match delete_modpack(&pack.path) {
            Ok(()) => {
                if self.config.active_modpack == pack.name {
                    self.config.active_modpack.clear();
                    self.save_config();
                }
                self.refresh_mods_and_packs();
                self.notify_ok(format!("Đã xóa ModPack '{}'.", pack.name));
            }
            Err(e) => self.notify_err(e.to_string()),
        }
    }

    pub fn update_share_key_input(&mut self) {
        if self.share_key_input.trim().is_empty() {
            self.share_import_preview = None;
            self.share_import_pack_name.clear();
            self.modpack_sync_report = None;
            return;
        }
        self.modpack_sync_report = None;
        let preview_res = inspect_share_key_against_local(
            &self.paths,
            &self.share_key_input,
            &self.installed_mods,
            &self.staged_workshop_mods,
        )
        .map_err(|e| e.to_string());
        let prev_payload = self
            .share_import_preview
            .as_ref()
            .and_then(|res| res.as_ref().ok())
            .map(|prev| &prev.payload);
        if let Ok(ref new_preview) = preview_res {
            if prev_payload != Some(&new_preview.payload)
                || self.share_import_pack_name.trim().is_empty()
            {
                self.share_import_pack_name = new_preview.suggested_pack_name.clone();
            }
        }
        self.share_import_preview = Some(preview_res);
    }

    pub fn clear_share_key_import(&mut self) {
        self.share_key_input.clear();
        self.share_import_preview = None;
        self.share_import_pack_name.clear();
        self.modpack_sync_report = None;
    }

    pub fn sync_modpack_from_key(&mut self) {
        if self.modpack_sync_busy {
            return;
        }
        let payload = match decode_modpack_share_key(&self.share_key_input) {
            Ok(p) => p,
            Err(e) => {
                self.notify_err(e.to_string());
                return;
            }
        };

        self.modpack_sync_busy = true;
        self.modpack_sync_report = None;
        self.modpack_sync_progress = Some(ModPackSyncProgress {
            completed: 0,
            total: payload.mods.len(),
            current_mod: String::new(),
            stage: "Đang chuẩn bị đồng bộ ModPack...".to_string(),
        });
        self.push_workshop_log(format!("=== Đồng bộ ModPack '{}' ===", payload.name));

        let steamcmd_exe = self
            .env
            .steamcmd_path
            .clone()
            .filter(|p| p.is_file())
            .or_else(|| {
                let p = self.paths.steamcmd_dir.join("steamcmd.exe");
                p.is_file().then_some(p)
            });
        let paths = self.paths.clone();
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        let active_modpack = self.config.active_modpack.clone();
        let desired_pack_name = self.share_import_pack_name.trim().to_string();
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        let prog_tx = tx.clone();
        let prog_wake = wake.clone();
        let log_tx = tx.clone();
        let log_wake = wake.clone();

        self.rt.spawn(async move {
            let res = sync_modpack_from_share_key(
                &paths,
                steamcmd_exe.as_deref(),
                proxy.as_deref(),
                &active_modpack,
                &payload,
                &desired_pack_name,
                move |prog| {
                    let _ = prog_tx.send(AppEvent::ModPackSyncProgress(prog));
                    prog_wake.wake();
                },
                move |line| {
                    let _ = log_tx.send(AppEvent::WorkshopLog(line));
                    log_wake.wake();
                },
            )
            .await
            .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::ModPackSynced(Box::new(res)));
            wake.wake();
        });
    }

    pub fn prepare_share_key_for_pack(&mut self, pack: &ModPackInfo) {
        self.share_export_target_pack = Some(pack.name.clone());
        self.refresh_active_share_export_preview(true);
    }

    pub fn prepare_share_key_from_enabled(&mut self) {
        let enabled = self.enabled_mod_names();
        if enabled.is_empty() {
            self.notify_err("Chưa có mod nào đang bật để tạo Share Key.");
            return;
        }
        self.share_export_target_pack = None;
        let pack_name = self.resolve_enabled_share_pack_name();
        self.apply_share_export_preview(&pack_name, &enabled, None, true);
    }

    pub fn close_share_export(&mut self) {
        self.share_export_preview = None;
        self.share_export_target_pack = None;
        self.workshop_candidate_target = None;
        self.workshop_candidates.clear();
        self.workshop_candidates_busy = false;
    }

    pub fn save_manual_mod_source(
        &mut self,
        internal_name: &str,
        display_name: &str,
        mod_version: &str,
        raw_input: &str,
    ) {
        let source = match parse_mod_source_input(raw_input) {
            Ok(s) => s,
            Err(e) => {
                self.notify_err(e.to_string());
                return;
            }
        };
        let records = match upsert_mod_source(
            &self.paths,
            internal_name,
            display_name,
            mod_version,
            "",
            source,
        ) {
            Ok(r) => r,
            Err(e) => {
                self.notify_err(e.to_string());
                return;
            }
        };
        self.mod_sources = records;
        self.unlinked_source_inputs.remove(internal_name);
        if self.workshop_candidate_target.as_deref() == Some(internal_name) {
            self.workshop_candidate_target = None;
            self.workshop_candidates.clear();
            self.workshop_candidates_busy = false;
        }
        if self.share_export_preview.is_some() {
            self.refresh_active_share_export_preview(true);
        }
        let label = if display_name.trim().is_empty() {
            internal_name
        } else {
            display_name.trim()
        };
        self.notify_ok(format!("Đã lưu nguồn tải cho mod '{label}'."));
    }

    pub fn search_mod_workshop_candidates(&mut self, internal_name: &str, display_name: &str) {
        let internal_owned = internal_name.trim().to_string();
        if internal_owned.is_empty() {
            return;
        }
        let display_owned = display_name.trim().to_string();
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);

        self.workshop_candidate_target = Some(internal_owned.clone());
        self.workshop_candidates.clear();
        self.workshop_candidates_busy = true;

        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let result = search_workshop_candidates_for_mod(
                &display_owned,
                &internal_owned,
                proxy.as_deref(),
            )
            .await
            .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::WorkshopCandidatesLoaded {
                internal_name: internal_owned,
                result,
            });
            wake.wake();
        });
    }

    fn enabled_mod_names(&self) -> Vec<String> {
        self.installed_mods
            .iter()
            .filter(|m| m.enabled)
            .map(|m| m.meta.internal_name.clone())
            .collect()
    }

    fn sync_install_txt_for_pack(&self, pack: &ModPackInfo) {
        if let Ok(preview) = build_modpack_share_preview(
            &self.paths,
            &pack.name,
            &pack.enabled_mods,
            Some(&pack.path),
            &self.installed_mods,
            &self.staged_workshop_mods,
            &self.workshop_records,
            &self.mod_sources,
        ) {
            let workshop_ids = extract_workshop_ids(&preview);
            let _ = write_modpack_install_txt(&pack.path, &workshop_ids);
        }
    }

    fn refresh_active_share_export_preview(&mut self, write_install_txt_if_pack: bool) {
        let (pack_name, mod_names, pack_path) =
            if let Some(target_name) = self.share_export_target_pack.clone() {
                let Some(pack) = self.modpacks.iter().find(|p| p.name == target_name).cloned() else {
                    self.close_share_export();
                    return;
                };
                (pack.name, pack.enabled_mods, Some(pack.path))
            } else {
                if self.share_export_preview.is_none() {
                    return;
                }
                let enabled = self.enabled_mod_names();
                if enabled.is_empty() {
                    self.close_share_export();
                    return;
                }
                (self.resolve_enabled_share_pack_name(), enabled, None)
            };

        self.apply_share_export_preview(
            &pack_name,
            &mod_names,
            pack_path.as_deref(),
            write_install_txt_if_pack,
        );
    }

    fn apply_share_export_preview(
        &mut self,
        pack_name: &str,
        mod_names: &[String],
        pack_dir: Option<&std::path::Path>,
        write_install_txt_if_pack: bool,
    ) {
        match build_modpack_share_preview(
            &self.paths,
            pack_name,
            mod_names,
            pack_dir,
            &self.installed_mods,
            &self.staged_workshop_mods,
            &self.workshop_records,
            &self.mod_sources,
        ) {
            Ok(preview) => {
                if write_install_txt_if_pack {
                    if let Some(dir) = pack_dir {
                        let workshop_ids = extract_workshop_ids(&preview);
                        if let Err(e) = write_modpack_install_txt(dir, &workshop_ids) {
                            self.notify_err(format!("Không ghi được install.txt cho ModPack: {e}"));
                        } else {
                            self.modpacks =
                                list_modpacks(&self.paths.tml_modpacks_dir()).unwrap_or_default();
                        }
                    }
                }
                self.prune_resolved_workshop_candidate(&preview);
                self.share_export_preview = Some(preview);
            }
            Err(e) => {
                self.close_share_export();
                if write_install_txt_if_pack {
                    self.notify_err(e.to_string());
                }
            }
        }
    }

    fn resolve_enabled_share_pack_name(&self) -> String {
        if let Ok(name) = sanitize_modpack_name(&self.new_modpack_name) {
            return name;
        }
        if let Ok(name) = sanitize_modpack_name(&self.config.active_modpack) {
            return name;
        }
        "SharedModPack".to_string()
    }

    fn prune_resolved_workshop_candidate(&mut self, preview: &ModPackSharePreview) {
        if let Some(target) = self.workshop_candidate_target.as_deref() {
            let still_manual = preview.entries.iter().any(|e| {
                e.internal_name == target && matches!(e.source, SharedModSource::ManualOnly)
            });
            if !still_manual {
                self.workshop_candidate_target = None;
                self.workshop_candidates.clear();
                self.workshop_candidates_busy = false;
            }
        }
    }

    fn push_workshop_log(&mut self, line: String) {
        while self.workshop_logs.len() >= 250 {
            self.workshop_logs.remove(0);
        }
        self.workshop_logs.push(line);
    }

    pub fn fetch_github_repo(&mut self, repo_raw: &str) {
        if self.github_busy {
            return;
        }
        let repo = repo_raw.trim().to_string();
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        self.github_repo_input = repo.clone();
        self.github_busy = true;
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let result = fetch_github_latest_tmod(&repo, proxy.as_deref())
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::GithubFetched { repo, result });
            wake.wake();
        });
    }

    pub fn download_mod_url(
        &mut self,
        url: &str,
        suggested_name: Option<String>,
        github_repo: Option<String>,
    ) {
        if self.github_busy {
            return;
        }
        let url_owned = url.trim().to_string();
        let paths = self.paths.clone();
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        self.github_busy = true;
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let res = download_and_install_tmod_url_with_source(
                &url_owned,
                suggested_name.as_deref(),
                github_repo.as_deref(),
                &paths,
                proxy.as_deref(),
                true,
            )
            .await
            .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::ModDownloaded(Box::new(res)));
            wake.wake();
        });
    }

    pub fn browse_workshop(&mut self, page: u32) {
        if self.workshop_browse_busy {
            return;
        }
        let page = page.max(1);
        self.workshop_browse_busy = true;
        self.workshop_browse_error = None;
        self.workshop_initial_loaded = true;
        self.workshop_thumbnails
            .retain(|_, v| !matches!(v, WorkshopThumbState::Failed));
        if self.workshop_thumbnails.len() > 120 {
            self.workshop_thumbnails.clear();
        }

        let query = self.workshop_input.trim().to_string();
        let sort = self.workshop_sort;
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();

        self.rt.spawn(async move {
            let result = browse_workshop_mods(&query, sort, page, proxy.as_deref())
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::WorkshopBrowsed { page, result });
            wake.wake();
        });
    }

    pub fn smart_workshop_query(&mut self) {
        if let Some(id) = parse_workshop_id(&self.workshop_input) {
            self.inspect_workshop_id(&id);
        } else {
            self.browse_workshop(1);
        }
    }

    pub fn inspect_workshop_id(&mut self, id: &str) {
        let trimmed = id.trim();
        if trimmed.is_empty() || self.workshop_inspect_busy {
            return;
        }

        self.workshop_details = self
            .workshop_results
            .iter()
            .find(|item| item.published_file_id == trimmed)
            .cloned();

        self.workshop_inspect_busy = true;
        let id_owned = trimmed.to_string();
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let res = fetch_workshop_details(&id_owned, proxy.as_deref())
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::WorkshopLookedUp(res));
            wake.wake();
        });
    }

    pub fn download_workshop(&mut self) {
        let Some(id) = parse_workshop_id(&self.workshop_input) else {
            self.notify_err("Nhập Workshop ID hoặc link Steam Workshop hợp lệ.");
            return;
        };
        self.download_workshop_item(&id, None);
    }

    pub fn download_workshop_item(
        &mut self,
        workshop_id: &str,
        details: Option<WorkshopItemDetails>,
    ) {
        let id = workshop_id.trim().to_string();
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
            self.notify_err("Workshop ID không hợp lệ.");
            return;
        }
        let Some(steamcmd_exe) = self.env.steamcmd_path.clone() else {
            self.notify_err("Chưa có SteamCMD. Nhấn 'Cài SteamCMD tự động' ở trên.");
            return;
        };
        if self.workshop_busy {
            self.notify_err("Đang xử lý tác vụ Workshop khác, vui lòng chờ xong.");
            return;
        }

        let resolved_details = details.or_else(|| {
            self.workshop_details
                .as_ref()
                .filter(|d| d.published_file_id == id)
                .cloned()
                .or_else(|| {
                    self.workshop_results
                        .iter()
                        .find(|d| d.published_file_id == id)
                        .cloned()
                })
        });

        self.workshop_busy = true;
        self.workshop_downloading_id = Some(id.clone());
        self.workshop_logs.clear();

        let paths = self.paths.clone();
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        let log_tx = tx.clone();
        let log_wake = wake.clone();

        self.rt.spawn(async move {
            let res = download_workshop_mod_via_steamcmd(
                &steamcmd_exe,
                &paths,
                &id,
                true,
                move |line| {
                    let _ = log_tx.send(AppEvent::WorkshopLog(line));
                    log_wake.wake();
                },
            )
            .await
            .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::WorkshopFinished {
                workshop_id: id,
                details: resolved_details,
                result: res,
            });
            wake.wake();
        });
    }

    pub fn request_workshop_thumbnail(
        &mut self,
        _ctx: &egui::Context,
        url: &str,
    ) -> Option<TextureHandle> {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return None;
        }
        match self.workshop_thumbnails.get(trimmed) {
            Some(WorkshopThumbState::Ready(handle)) => return Some(handle.clone()),
            Some(WorkshopThumbState::Loading | WorkshopThumbState::Failed) => return None,
            None => {}
        }

        let url_owned = trimmed.to_string();
        self.workshop_thumbnails
            .insert(url_owned.clone(), WorkshopThumbState::Loading);

        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let result = fetch_workshop_image_bytes(&url_owned, proxy.as_deref())
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::WorkshopThumbnailLoaded {
                url: url_owned,
                result,
            });
            wake.wake();
        });
        None
    }

    pub fn workshop_install_status(
        &self,
        workshop_id: &str,
        remote_time_updated: u64,
    ) -> Option<(String, bool, bool)> {
        for rec in self
            .workshop_records
            .iter()
            .rev()
            .filter(|r| r.published_file_id == workshop_id)
        {
            if let Some(entry) = self.installed_mods.iter().find(|m| {
                m.meta.internal_name == rec.internal_name || m.meta.file_name == rec.file_name
            }) {
                let update_available =
                    remote_time_updated > rec.time_updated && rec.time_updated > 0;
                return Some((
                    entry.meta.internal_name.clone(),
                    entry.enabled,
                    update_available,
                ));
            }
        }

        for staged in self
            .staged_workshop_mods
            .iter()
            .filter(|s| s.workshop_id == workshop_id && s.installed)
        {
            if let Some(entry) = self
                .installed_mods
                .iter()
                .find(|m| m.meta.internal_name == staged.meta.internal_name)
            {
                let local_ts = staged.time_updated.unwrap_or(0);
                let update_available = remote_time_updated > local_ts && local_ts > 0;
                return Some((
                    entry.meta.internal_name.clone(),
                    entry.enabled,
                    update_available,
                ));
            }
        }

        None
    }

    pub fn install_steamcmd(&mut self) {
        if self.steamcmd_busy {
            return;
        }
        self.steamcmd_busy = true;
        self.workshop_logs.clear();
        self.workshop_logs
            .push("=== Bắt đầu cài đặt SteamCMD ===".to_string());
        self.steamcmd_progress = Some(InstallProgress {
            target: InstallTarget::SteamCmd,
            stage: "Đang chuẩn bị cài đặt SteamCMD...".to_string(),
            current: 0,
            total: None,
        });

        let paths = self.paths.clone();
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        let on_progress = self.make_progress_callback();
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        let tx_l = tx.clone();
        let wake_l = wake.clone();
        let on_log: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |line| {
            let _ = tx_l.send(AppEvent::WorkshopLog(line));
            wake_l.wake();
        });

        self.rt.spawn(async move {
            let res =
                install_steamcmd_with_progress(&paths, proxy.as_deref(), on_progress, on_log)
                    .await
                    .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::SteamCmdInstalled(res));
            wake.wake();
        });
    }

    pub fn backup_world_file(&mut self, world: &WorldMetadata) {
        match backup_world(&world.file_path, &self.paths.backups_dir) {
            Ok(info) => {
                self.refresh_worlds_and_backups();
                self.notify_ok(format!("Đã sao lưu thế giới vào {}", info.file_name));
            }
            Err(e) => self.notify_err(format!("Sao lưu thất bại: {e}")),
        }
    }

    pub fn restore_backup(&mut self, backup: &WorldBackupInfo) {
        let target_dir = match self.config.active_edition {
            GameEdition::Vanilla => self.paths.vanilla_worlds_dir(),
            GameEdition::TModLoader => self.paths.tml_worlds_dir(),
        };
        match restore_world_backup(&backup.file_path, &target_dir) {
            Ok(files) => {
                self.refresh_worlds_and_backups();
                self.notify_ok(format!(
                    "Đã khôi phục {} file từ {}",
                    files.len(),
                    backup.file_name
                ));
            }
            Err(e) => self.notify_err(format!("Khôi phục thất bại: {e}")),
        }
    }

    pub fn add_server_profile(&mut self) {
        let name = self.new_profile_name.trim();
        let final_name = if name.is_empty() {
            format!("Server {}", self.config.servers.len() + 1)
        } else {
            name.to_string()
        };
        let id = format!(
            "srv-{}",
            uuid_short()
        );
        let edition = self.new_profile_edition;
        let worlds_dir = match edition {
            GameEdition::Vanilla => self.paths.vanilla_worlds_dir(),
            GameEdition::TModLoader => self.paths.tml_worlds_dir(),
        };
        let profile = ServerProfile {
            id: id.clone(),
            name: final_name,
            edition,
            auto_restart: false,
            extra_args: String::new(),
            config: ServerConfigData {
                worldpath: worlds_dir.to_string_lossy().into_owned(),
                world: worlds_dir
                    .join("NewWorld.wld")
                    .to_string_lossy()
                    .into_owned(),
                worldname: "NewWorld".to_string(),
                banlist: self
                    .paths
                    .servers_dir
                    .join(&id)
                    .join("banlist.txt")
                    .to_string_lossy()
                    .into_owned(),
                ..ServerConfigData::default()
            },
        };
        self.config.servers.push(profile);
        self.new_profile_name.clear();
        self.select_server_profile(id);
        self.notify_ok("Đã thêm cấu hình server mới.");
    }

    pub fn delete_active_server_profile(&mut self) {
        if self.config.servers.len() <= 1 {
            self.notify_err("Cần giữ ít nhất 1 cấu hình server.");
            return;
        }
        let id = self.config.active_server_id.clone();
        let is_supervised_active = self
            .server_supervisors
            .get(&id)
            .is_some_and(|s| s.is_active());
        let is_external_active = self.external_server_for_profile(&id).is_some();
        if is_supervised_active || is_external_active {
            self.notify_err("Vui lòng tắt server đang chạy của hồ sơ này trước khi xóa.");
            return;
        }
        self.server_supervisors.remove(&id);
        self.server_snapshots.remove(&id);
        self.external_log_summaries.remove(&id);
        self.cleared_external_log_pids.remove(&id);
        if self.pending_auto_direct_host_profile_id.as_deref() == Some(id.as_str()) {
            self.pending_auto_direct_host_profile_id = None;
        }
        self.config.servers.retain(|s| s.id != id);
        let next_id = self.config.servers[0].id.clone();
        self.select_server_profile(next_id);
        self.notify_ok("Đã xóa cấu hình server.");
    }

    pub fn save_quick_join_server(&mut self) {
        let fallback_port = self
            .quick_join_port_input
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|&p| p > 0)
            .unwrap_or(if self.config.quick_join_port > 0 {
                self.config.quick_join_port
            } else {
                7777
            });
        let (host, port) = split_host_port_input(&self.config.quick_join_host, fallback_port);
        if host.is_empty() {
            self.notify_err("Nhập địa chỉ Host / IP trước khi lưu server.");
            return;
        }
        let final_port = if port > 0 { port } else { 7777 };
        self.config.quick_join_host = host.clone();
        self.config.quick_join_port = final_port;
        self.quick_join_port_input = final_port.to_string();
        let default_label = if host.contains(':') {
            format!("[{host}]:{final_port}")
        } else {
            format!("{host}:{final_port}")
        };
        let name = if self.new_join_name.trim().is_empty() {
            default_label
        } else {
            self.new_join_name.trim().to_string()
        };
        if let Some(existing) = self.config.saved_join_servers.iter_mut().find(|s| {
            s.host.eq_ignore_ascii_case(&host)
                && s.port == final_port
                && s.edition == self.config.active_edition
        }) {
            existing.name = name;
            existing.password = self.config.quick_join_password.clone();
        } else {
            self.config.saved_join_servers.push(SavedJoinServer {
                name,
                host,
                port: final_port,
                password: self.config.quick_join_password.clone(),
                edition: self.config.active_edition,
            });
        }
        self.new_join_name.clear();
        self.save_config();
        self.notify_ok("Đã lưu server vào danh sách.");
    }

    pub fn shutdown_all(&mut self) {
        self.save_config();
        self.pending_auto_direct_host_profile_id = None;
        self.stop_client_ipv6_bridge();
        self.stop_direct_host();

        let active_sups: Vec<ServerSupervisor> = self
            .server_supervisors
            .values()
            .filter(|sup| sup.is_active())
            .cloned()
            .collect();
        if !active_sups.is_empty() {
            for sup in &active_sups {
                sup.request_graceful_stop();
            }
            let deadline = Instant::now() + Duration::from_secs(6);
            while Instant::now() < deadline && active_sups.iter().any(|sup| sup.is_active()) {
                std::thread::sleep(Duration::from_millis(25));
            }
            for sup in &active_sups {
                if sup.is_active() {
                    sup.force_kill();
                }
            }
            let kill_deadline = Instant::now() + Duration::from_millis(600);
            while Instant::now() < kill_deadline && active_sups.iter().any(|sup| sup.is_active()) {
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        if self.sys.is_none() {
            self.sys = System::new().ok();
        }
        let mut targets = self.external_servers.clone();
        if let Some(sys) = self.sys.as_mut() {
            for ext in detect_existing_server_processes(sys, &self.paths) {
                if !targets.iter().any(|existing| existing.pid == ext.pid) {
                    targets.push(ext);
                }
            }
            for ext in targets {
                if ext.profile_id.is_some() {
                    let _ = terminate_process_by_pid(sys, ext.pid);
                }
            }
        }
    }

    pub fn showing_external_server(&self) -> bool {
        let sup_active = self
            .active_server_supervisor()
            .is_some_and(|s| s.is_active());
        !sup_active
            && self.active_external_server().is_some()
            && !matches!(
                self.active_server_snapshot().map(|s| &s.state),
                Some(ServerLifecycleState::Crashed(_))
            )
    }

    pub fn badge_color_for_server(&self) -> egui::Color32 {
        let p = self.palette();
        if self.showing_external_server() {
            if self.is_active_external_starting() {
                return p.accent_cyan;
            }
            return p.accent_emerald;
        }
        match self.active_server_snapshot().map(|s| &s.state) {
            Some(ServerLifecycleState::Running) => p.accent_emerald,
            Some(ServerLifecycleState::Starting) => p.accent_cyan,
            Some(ServerLifecycleState::Stopping) => p.accent_amber,
            Some(ServerLifecycleState::Crashed(_)) => p.accent_red,
            _ => p.text_muted,
        }
    }

    pub fn server_status_short(&self) -> &str {
        if self.showing_external_server() {
            if self.is_active_external_starting() {
                return "Đang khởi động (phiên trước)...";
            }
            return "Đang chạy (phiên trước)";
        }
        self.active_server_snapshot()
            .map(|s| s.state.label())
            .unwrap_or("Chưa bật")
    }
}

fn extract_workshop_ids(preview: &ModPackSharePreview) -> Vec<String> {
    preview
        .entries
        .iter()
        .filter_map(|e| match &e.source {
            SharedModSource::Workshop { id } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

fn format_modpack_sync_summary(report: &ModPackSyncReport) -> String {
    let mut parts = vec![format!(
        "Đã đồng bộ ModPack '{}' (bật {}/{} mod).",
        report.pack_name, report.enabled_count, report.total_mods
    )];
    let downloaded_total = report.downloaded_workshop + report.downloaded_other;
    if downloaded_total > 0 || report.already_present > 0 {
        parts.push(format!(
            "Có sẵn {}, tải mới {} (Workshop {}, nguồn khác {}).",
            report.already_present,
            downloaded_total,
            report.downloaded_workshop,
            report.downloaded_other
        ));
    }
    if let Some(ref backup) = report.backup_pack_name {
        parts.push(format!("Đã sao lưu mod cũ vào '{backup}'."));
    }
    if !report.missing_manual.is_empty() || !report.failed_mods.is_empty() {
        parts.push(format!(
            "Thiếu thủ công: {}, lỗi: {}.",
            report.missing_manual.len(),
            report.failed_mods.len()
        ));
    }
    parts.join(" ")
}

fn pick_default_tml_release(releases: &[TmodLoaderReleaseInfo]) -> Option<&TmodLoaderReleaseInfo> {
    releases
        .iter()
        .find(|r| !r.prerelease)
        .or_else(|| releases.first())
}

pub fn non_empty_opt(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

fn strip_ipv6_zone_if_valid(s: &str) -> Option<String> {
    let candidate = s.split('%').next().unwrap_or(s).trim();
    if candidate.parse::<std::net::Ipv6Addr>().is_ok() {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn split_host_port_input(raw_host: &str, fallback_port: u16) -> (String, u16) {
    let trimmed = raw_host.trim();
    if let Some(rest) = trimmed.strip_prefix('[') {
        if let Some((v6, port_str)) = rest.split_once("]:") {
            let v6 = v6.trim();
            if !v6.is_empty() {
                if let Some(port) = port_str.trim().parse::<u16>().ok().filter(|&p| p > 0) {
                    let host = strip_ipv6_zone_if_valid(v6).unwrap_or_else(|| v6.to_string());
                    return (host, port);
                }
            }
        } else if let Some(inner) = rest.strip_suffix(']') {
            if let Some(host) = strip_ipv6_zone_if_valid(inner) {
                return (host, fallback_port);
            }
        }
        return (trimmed.to_string(), fallback_port);
    }
    if trimmed.matches(':').count() != 1 {
        if let Some(host) = strip_ipv6_zone_if_valid(trimmed) {
            return (host, fallback_port);
        }
        return (trimmed.to_string(), fallback_port);
    }
    match trimmed.rsplit_once(':') {
        Some((left, right)) if !left.trim().is_empty() => right
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|&p| p > 0)
            .map(|parsed_port| (left.trim().to_string(), parsed_port))
            .unwrap_or_else(|| (trimmed.to_string(), fallback_port)),
        _ => (trimmed.to_string(), fallback_port),
    }
}

fn uuid_short() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{:x}", now & 0xFFFF_FFFF)
}

#[cfg(test)]
mod tests {
    use super::split_host_port_input;

    #[test]
    fn split_host_port_input_handles_ipv6_and_plain_hosts() {
        assert_eq!(
            split_host_port_input("play.example.com:50355", 7777),
            ("play.example.com".to_string(), 50355)
        );
        assert_eq!(
            split_host_port_input("  192.168.1.10:7778 ", 7777),
            ("192.168.1.10".to_string(), 7778)
        );
        assert_eq!(
            split_host_port_input("play.example.com", 7777),
            ("play.example.com".to_string(), 7777)
        );
        assert_eq!(
            split_host_port_input("[2405:4802:938c:caf0::1]:7778", 7777),
            ("2405:4802:938c:caf0::1".to_string(), 7778)
        );
        assert_eq!(
            split_host_port_input("[::1]:7779", 7777),
            ("::1".to_string(), 7779)
        );
        assert_eq!(
            split_host_port_input("[::1]", 7777),
            ("::1".to_string(), 7777)
        );
        assert_eq!(
            split_host_port_input("::1", 7777),
            ("::1".to_string(), 7777)
        );
        assert_eq!(
            split_host_port_input("[fe80::1%12]:7777", 7777),
            ("fe80::1".to_string(), 7777)
        );
        assert_eq!(
            split_host_port_input("[2405:4802::1%12]", 7777),
            ("2405:4802::1".to_string(), 7777)
        );
        assert_eq!(
            split_host_port_input("2405:4802::1%12", 7777),
            ("2405:4802::1".to_string(), 7777)
        );
        assert_eq!(
            split_host_port_input("play.example.com:0", 7777),
            ("play.example.com:0".to_string(), 7777)
        );
        assert_eq!(
            split_host_port_input(":50355", 7777),
            (":50355".to_string(), 7777)
        );
    }
}
