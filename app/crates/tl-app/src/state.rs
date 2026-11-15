use crate::assets::LauncherAssets;
use crate::theme::{format_bytes, palette, ThemePalette};
use egui::{ColorImage, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use sysinfo::System;
use tl_core::{
    activate_modpack, backup_unsaved_enabled_mods_as_modpack, backup_world, create_modpack,
    decode_modpack_share_key, delete_modpack, detect_environment,
    detect_exact_bundled_dotnet_runtime, inspect_world_backup_zip, is_safe_single_dir_component,
    list_modpacks, list_world_backups, parse_world_file, restore_world_backup,
    sanitize_modpack_name, sync_active_modpack_from_mods_dir, write_modpack_install_txt,
    EnvironmentStatus, GameEdition, LauncherConfig, LauncherPaths, ModPackInfo, SavedJoinServer,
    ServerConfigData, ServerProfile, SharedModSource, ThemeMode, TmodMetadata, WorldBackupInfo,
    WorldMetadata, DEFAULT_TERRARIA_INSTALL_SOURCE,
};
use tl_runtime::{
    browse_workshop_mods, build_modpack_share_preview, check_firewall_rule_exists,
    delete_installed_mod, detect_existing_game_process, detect_existing_server_processes,
    download_and_install_tmod_url_with_source, download_and_install_xna,
    download_workshop_mod_via_steamcmd, enable_mod_and_installed_deps, ensure_firewall_rule,
    extract_ban_ip_key, fetch_github_latest_tmod, fetch_terraria_branch_manifest,
    fetch_tmodloader_releases, fetch_workshop_details, fetch_workshop_image_bytes,
    import_local_mod_files, inspect_share_key_against_local, install_dotnet8_portable,
    install_steamcmd_with_progress, install_terraria_from_local_zip, install_terraria_from_source,
    install_tmodloader_release, install_xna_framework_elevated, is_local_probe_host,
    is_loopback_ban_target, launch_game, load_banlist, load_installed_terraria_receipt,
    load_mod_sources, load_workshop_manifest, normalize_ip_addr, parse_mod_source_input,
    parse_workshop_id, probe_local_endpoint_listening, probe_local_tcp_listening,
    probe_network_snapshot, probe_tcp_endpoint, read_required_tml_dotnet_version,
    read_server_log_summary, record_workshop_install, remove_firewall_rule, remove_mod_source,
    remove_workshop_manifest_entry, resolve_ipv6_bridge_target, save_banlist, scan_installed_mods,
    scan_staged_workshop_mods, search_workshop_candidates_for_mod, set_mod_enabled,
    split_endpoint_host_port, start_direct_port_host, start_ipv4_to_ipv6_client_bridge,
    start_server, sync_modpack_from_share_key, terminate_game_process_by_pid,
    terminate_process_by_pid, upsert_mod_source, ActiveGameProcess, BanEntry,
    DetectedExternalServer, DirectHostConfig, DirectHostStatus, DirectPortHostHandle,
    ExternalServerLogSummary, GameLaunchOptions, InstallProgress, InstallTarget, InstalledModEntry,
    InstalledTerrariaReceipt, LocalClientBridgeHandle, ModPackImportPreview, ModPackSharePreview,
    ModPackSyncProgress, ModPackSyncReport, ModSourceRecord, NetworkSnapshot, ProgressCallback,
    RemoteTmodAsset, RepaintHandle, RunningGameSnapshot, RuntimeTuning, ServerLifecycleState,
    ServerRuntimeSnapshot, ServerSupervisor, StagedWorkshopMod, TerrariaBranchManifest,
    TmodLoaderReleaseInfo, UpnpPortStatus, WorkshopItemDetails, WorkshopManifestRecord,
    WorkshopQueryResult, WorkshopSort,
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
    WorkshopBatchFinished {
        primary_id: String,
        installed: Vec<(String, Option<WorkshopItemDetails>, Vec<TmodMetadata>)>,
        error: Option<String>,
    },
    SteamCmdInstalled(Result<String, String>),
    TmlReleasesFetched(Result<Vec<TmodLoaderReleaseInfo>, String>),
    TerrariaManifestChecked(Result<TerrariaBranchManifest, String>),
    InstallProgress(InstallProgress),
    InstallFinished {
        target: InstallTarget,
        result: Result<String, String>,
    },
    XnaInstallFinished(Result<String, String>),
    ModPackSyncProgress(ModPackSyncProgress),
    ModPackSynced(Box<Result<ModPackSyncReport, String>>),
    WorkshopCandidatesLoaded {
        internal_name: String,
        result: Result<Vec<WorkshopItemDetails>, String>,
    },
    DelayedBanRefresh {
        profile_id: String,
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
    pub confirm_close_bridge_modal: bool,
    close_bridge_allow_exit: bool,

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

    pub pending_auto_direct_host_ports: HashMap<String, u16>,
    pub direct_host_handles: HashMap<String, DirectPortHostHandle>,
    pub direct_host_port_input: String,
    pub direct_bind_ips: HashMap<String, Option<std::net::Ipv4Addr>>,
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
        let default_try_upnp = config.active_server().map(|s| s.config.upnp).unwrap_or(true);
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
            confirm_close_bridge_modal: false,
            close_bridge_allow_exit: false,
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
            pending_auto_direct_host_ports: HashMap::new(),
            direct_host_handles: HashMap::new(),
            direct_host_port_input: default_port.to_string(),
            direct_bind_ips: HashMap::new(),
            direct_try_upnp: default_try_upnp,
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

    fn close_resource_flags(&self) -> (bool, bool, bool, bool) {
        let has_hosted_server = self.server_supervisors.values().any(|s| s.is_active())
            || !self.pending_auto_direct_host_ports.is_empty();
        let mut has_direct_host = false;
        let mut has_upnp = false;
        for handle in self.direct_host_handles.values() {
            match handle.current_status() {
                DirectHostStatus::Starting { .. } => {
                    has_direct_host = true;
                }
                DirectHostStatus::Running(info) => {
                    has_direct_host = true;
                    if matches!(
                        info.upnp_status,
                        UpnpPortStatus::Mapped { .. } | UpnpPortStatus::Mapping
                    ) {
                        has_upnp = true;
                    }
                }
                _ => {}
            }
        }
        let has_client_bridge = self.has_active_client_ipv6_bridge();
        (has_hosted_server, has_direct_host, has_upnp, has_client_bridge)
    }

    pub fn is_runtime_or_task_busy_for_paths(&self) -> bool {
        let (has_hosted_server, has_direct_host, has_upnp, has_client_bridge) =
            self.close_resource_flags();
        self.install_busy
            || self.is_steamcmd_busy()
            || self.is_game_running()
            || self.any_server_active()
            || has_hosted_server
            || has_direct_host
            || has_upnp
            || has_client_bridge
    }

    pub fn is_profile_busy_or_direct_hosted(&self, profile_id: &str) -> bool {
        if self.is_profile_running(profile_id)
            || self.pending_auto_direct_host_ports.contains_key(profile_id)
        {
            return true;
        }
        if self.direct_host_handles.get(profile_id).is_some_and(|h| {
            matches!(
                h.current_status(),
                DirectHostStatus::Running(_) | DirectHostStatus::Starting { .. }
            )
        }) {
            return true;
        }
        let Some(port) = self.profile_effective_port(profile_id).or_else(|| {
            self.config
                .servers
                .iter()
                .find(|p| p.id == profile_id)
                .map(|p| p.config.port)
                .filter(|&p| p > 0)
        }) else {
            return false;
        };
        self.direct_host_handles
            .values()
            .any(|h| match h.current_status() {
                DirectHostStatus::Starting {
                    local_port,
                    host_port,
                } => local_port == port || host_port == port,
                DirectHostStatus::Running(info) => {
                    info.local_port == port
                        || info.host_port == port
                        || matches!(
                            info.upnp_status,
                            UpnpPortStatus::Mapped { external_port, .. }
                                | UpnpPortStatus::CarrierGradeNat { external_port, .. }
                                if external_port == port
                        )
                }
                _ => false,
            })
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
        if changed {
            self.direct_try_upnp = self
                .config
                .active_server()
                .map(|s| s.config.upnp)
                .unwrap_or(true);
        }
        let cfg_port = self
            .config
            .active_server()
            .map(|s| s.config.port)
            .filter(|&p| p > 0)
            .unwrap_or(7777);
        self.server_port_input = cfg_port.to_string();
        let port_str = self.active_server_port().to_string();
        self.port_probe_port = port_str.clone();
        self.direct_host_port_input = match self.active_direct_host_status() {
            DirectHostStatus::Running(info) => info.host_port.to_string(),
            DirectHostStatus::Starting { host_port, .. } => host_port.to_string(),
            _ => port_str,
        };
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
        let candidate_paths = LauncherPaths::resolve(
            non_empty_opt(&self.config.custom_terraria_dir),
            non_empty_opt(&self.config.custom_tmodloader_dir),
        );
        let paths_changed = candidate_paths.terraria_dir != self.paths.terraria_dir
            || candidate_paths.tmodloader_dir != self.paths.tmodloader_dir;
        let requested_steamcmd = non_empty_opt(&self.config.custom_steamcmd_path);
        let candidate_env = detect_environment(&candidate_paths, requested_steamcmd);
        let steamcmd_changed = candidate_env.steamcmd_path != self.env.steamcmd_path;

        if paths_changed || (steamcmd_changed && self.is_steamcmd_busy()) {
            self.scan_external_servers();
            if self.is_runtime_or_task_busy_for_paths() {
                self.notify_err(
                    "Không thể thay đổi đường dẫn game/công cụ khi Game, Server, Cài đặt hoặc Cầu nối mạng đang chạy.",
                );
                self.terraria_installed_receipt = load_installed_terraria_receipt(&self.paths);
                self.refresh_worlds_and_backups();
                self.refresh_mods_and_packs();
                self.refresh_banlist();
                return;
            }
        }

        self.paths = candidate_paths;
        let _ = self.paths.ensure_dirs();
        self.env = if paths_changed {
            candidate_env
        } else {
            detect_environment(&self.paths, non_empty_opt(&self.config.custom_steamcmd_path))
        };
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
        self.sync_banlist_to_direct_host();
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
                            self.sync_active_modpack_if_needed(None);
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
                AppEvent::WorkshopBatchFinished {
                    primary_id,
                    installed,
                    error,
                } => {
                    self.workshop_busy = false;
                    self.workshop_downloading_id = None;
                    let mut total_mods = 0usize;
                    let mut manifest_err: Option<String> = None;
                    for (wid, details_opt, mods) in &installed {
                        total_mods += mods.len();
                        if let Err(e) =
                            record_workshop_install(&self.paths, details_opt.as_ref(), wid, mods)
                        {
                            manifest_err = Some(e.to_string());
                        }
                    }
                    if !installed.is_empty() {
                        self.sync_active_modpack_if_needed(None);
                    }
                    self.refresh_mods_and_packs();
                    match (error, manifest_err) {
                        (Some(e), _) => self.notify_err(e),
                        (None, Some(me)) => self.notify_err(format!(
                            "Đã tải {total_mods} mod nhưng lỗi lưu lịch sử Workshop: {me}"
                        )),
                        (None, None) if installed.len() > 1 => self.notify_ok(format!(
                            "Đã tải và cài {total_mods} mod ({} mục Workshop, gồm mod phụ thuộc cho ID {primary_id}).",
                            installed.len()
                        )),
                        (None, None) => self.notify_ok(format!(
                            "Đã tải và cài {total_mods} mod từ Workshop (ID {primary_id})."
                        )),
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
                AppEvent::XnaInstallFinished(res) => {
                    self.install_busy = false;
                    self.install_progress = None;
                    self.refresh_environment();
                    match res {
                        Ok(msg) => {
                            self.install_last_error = None;
                            self.notify_ok(msg);
                        }
                        Err(e) => {
                            self.install_last_error = Some(e.clone());
                            self.notify_err(e);
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
                AppEvent::DelayedBanRefresh { profile_id } => {
                    if self.config.active_server_id == profile_id {
                        self.refresh_banlist();
                    } else if let Some(handle) = self.direct_host_handles.get(&profile_id) {
                        handle.set_banned_ips(self.collect_banlist_ips_for_profile_id(&profile_id));
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

        if !self.has_active_close_resources() {
            self.confirm_close_bridge_modal = false;
            self.close_bridge_allow_exit = false;
        }

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
                let mut stopped_profiles: Vec<(String, u16, bool)> = Vec::new();
                let mut dirty_ban_profiles: Vec<String> = Vec::new();

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
                        let stopped_port = snap.port;

                        self.server_snapshots.insert(profile_id.clone(), snap);

                        if curr_player_count != prev_player_count || just_stopped {
                            any_banlist_dirty = true;
                            dirty_ban_profiles.push(profile_id.clone());
                        }
                        if just_stopped {
                            any_just_stopped = true;
                            any_network_dirty = true;
                            let was_starting =
                                matches!(prev_state, Some(ServerLifecycleState::Starting));
                            stopped_profiles.push((profile_id, stopped_port, was_starting));
                        } else if just_started_running {
                            any_network_dirty = true;
                        }
                    }
                }

                for (profile_id, stopped_port, was_starting) in stopped_profiles {
                    if self
                        .pending_auto_direct_host_ports
                        .remove(&profile_id)
                        .is_some()
                        && was_starting
                    {
                        self.notify_err("Server đã dừng trước khi kịp mở Port mạng.");
                    }
                    if self.direct_host_handles.contains_key(&profile_id) {
                        self.stop_direct_host_for_profile(&profile_id);
                    }
                    if stopped_port > 0 {
                        self.cleanup_host_for_stopped_port(stopped_port, Some(&profile_id), None);
                    }
                    if profile_id == self.config.active_server_id {
                        if let Some(cfg_port) = self
                            .config
                            .active_server()
                            .map(|p| p.config.port)
                            .filter(|&p| p > 0)
                        {
                            let cfg_port_str = cfg_port.to_string();
                            self.direct_host_port_input = cfg_port_str.clone();
                            self.port_probe_port = cfg_port_str;
                        }
                    }
                }

                if any_banlist_dirty {
                    self.refresh_banlist();
                    for profile_id in dirty_ban_profiles {
                        if profile_id != self.config.active_server_id {
                            if let Some(handle) = self.direct_host_handles.get(&profile_id) {
                                handle.set_banned_ips(
                                    self.collect_banlist_ips_for_profile_id(&profile_id),
                                );
                            }
                        }
                    }
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

        let pending_entries: Vec<(String, u16)> = self
            .pending_auto_direct_host_ports
            .iter()
            .map(|(id, port)| (id.clone(), *port))
            .collect();
        for (target_id, captured_host_port) in pending_entries {
            let sup_active = self
                .server_supervisors
                .get(&target_id)
                .is_some_and(|s| s.is_active());
            match self.server_snapshots.get(&target_id).map(|s| &s.state) {
                Some(ServerLifecycleState::Running) => {
                    self.pending_auto_direct_host_ports.remove(&target_id);
                    let local_port = self
                        .server_snapshots
                        .get(&target_id)
                        .map(|s| s.port)
                        .filter(|&p| p > 0)
                        .or_else(|| self.profile_effective_port(&target_id))
                        .unwrap_or_else(|| self.active_server_port());
                    let host_port = if captured_host_port > 0 {
                        captured_host_port
                    } else {
                        local_port
                    };
                    let try_upnp = if target_id == self.config.active_server_id {
                        self.direct_try_upnp
                    } else {
                        self.config
                            .servers
                            .iter()
                            .find(|p| p.id == target_id)
                            .map(|p| p.config.upnp)
                            .unwrap_or(true)
                    };
                    self.start_direct_host_for_profile(target_id, local_port, host_port, try_upnp);
                }
                Some(ServerLifecycleState::Stopped | ServerLifecycleState::Crashed(_)) | None => {
                    self.pending_auto_direct_host_ports.remove(&target_id);
                    self.notify_err("Server đã dừng trước khi kịp mở Port mạng.");
                }
                Some(ServerLifecycleState::Starting | ServerLifecycleState::Stopping) => {
                    if !sup_active {
                        self.pending_auto_direct_host_ports.remove(&target_id);
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
            || self
                .direct_host_handles
                .values()
                .any(|h| matches!(h.current_status(), DirectHostStatus::Starting { .. }));
        let repaint_interval = if any_task_busy {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(500)
        };
        ctx.request_repaint_after(repaint_interval);
    }

    fn worlds_dir_for_edition(&self, edition: GameEdition) -> PathBuf {
        match edition {
            GameEdition::Vanilla => self.paths.vanilla_worlds_dir(),
            GameEdition::TModLoader => self.paths.tml_worlds_dir(),
        }
    }

    fn running_game_edition(&self) -> Option<GameEdition> {
        self.active_game
            .as_ref()
            .map(|g| g.edition)
            .or_else(|| self.current_running_game().map(|g| g.edition))
    }

    fn is_edition_client_running(&self, edition: GameEdition) -> bool {
        self.active_game
            .as_ref()
            .is_some_and(|g| g.edition == edition)
            || self
                .running_game
                .as_ref()
                .is_some_and(|g| g.edition == edition)
            || self
                .external_game
                .as_ref()
                .is_some_and(|g| g.edition == edition)
    }

    fn clear_stale_mod_provenance(&self, internal_name: &str) {
        let exact = internal_name.trim();
        if exact.is_empty() {
            return;
        }
        let matching_names: Vec<String> = self
            .mod_sources
            .iter()
            .filter(|r| r.internal_name.eq_ignore_ascii_case(exact))
            .map(|r| r.internal_name.clone())
            .collect();
        for name in matching_names {
            let _ = remove_mod_source(&self.paths, &name);
        }
        let _ = remove_mod_source(&self.paths, exact);
        let _ = remove_workshop_manifest_entry(&self.paths, exact);
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

        let mut mod_tmod_paths = Vec::new();
        let mut candidate_mod_zips = Vec::new();
        let mut world_paths = Vec::new();
        let mut backup_zip_paths = Vec::new();
        let mut unsupported_labels = Vec::new();

        for path in paths {
            if !path.is_file() {
                let label = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| path.display().to_string());
                unsupported_labels.push(label);
                continue;
            }
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            match ext.as_str() {
                "tmod" => mod_tmod_paths.push(path),
                "wld" | "twld" => world_paths.push(path),
                "zip" => {
                    let (detected_ed, primary_wld, _) = inspect_world_backup_zip(&path);
                    if detected_ed.is_some() && primary_wld.is_some() {
                        backup_zip_paths.push(path);
                    } else {
                        candidate_mod_zips.push(path);
                    }
                }
                _ => {
                    let label = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(ToString::to_string)
                        .unwrap_or_else(|| ext.clone());
                    unsupported_labels.push(label);
                }
            }
        }

        let mut ok_parts = Vec::new();
        let mut err_parts = Vec::new();
        let mut imported_mods_total = 0usize;

        if !mod_tmod_paths.is_empty() || !candidate_mod_zips.is_empty() {
            if self.is_tml_mods_locked_by_runtime() {
                err_parts.push(
                    "Vui lòng đóng tModLoader và tắt Server đang dùng bộ Mod này trước khi nhập mod."
                        .to_string(),
                );
            } else {
                if !mod_tmod_paths.is_empty() {
                    match import_local_mod_files(&mod_tmod_paths, &self.paths, true) {
                        Ok(imported) => {
                            for meta in &imported {
                                self.clear_stale_mod_provenance(&meta.internal_name);
                            }
                            imported_mods_total += imported.len();
                        }
                        Err(e) => err_parts.push(format!("Lỗi nhập mod: {e}")),
                    }
                }
                for zip_path in candidate_mod_zips {
                    let zip_name = zip_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("archive.zip")
                        .to_string();
                    match import_local_mod_files(&[zip_path], &self.paths, true) {
                        Ok(imported) if !imported.is_empty() => {
                            for meta in &imported {
                                self.clear_stale_mod_provenance(&meta.internal_name);
                            }
                            imported_mods_total += imported.len();
                        }
                        Ok(_) => err_parts.push(format!(
                            "File .zip '{zip_name}' không chứa .tmod hoặc bản sao lưu thế giới hợp lệ."
                        )),
                        Err(e) => err_parts.push(format!("Lỗi nhập '{zip_name}': {e}")),
                    }
                }
                if imported_mods_total > 0 {
                    self.sync_active_modpack_if_needed(None);
                    self.refresh_mods_and_packs();
                    self.active_tab = NavTab::Mods;
                    self.mods_tab = ModsSubTab::Installed;
                    ok_parts.push(format!("Đã nhập và bật {imported_mods_total} mod"));
                }
            }
        }

        if !world_paths.is_empty() {
            self.scan_external_servers();
            let active_ed = self.config.active_edition;
            if self.install_busy {
                err_parts.push(
                    "Đang có tiến trình cài đặt chạy, không thể nhập thế giới lúc này.".to_string(),
                );
            } else if self.is_edition_client_running(active_ed)
                || self.has_running_server_for_edition(active_ed)
            {
                err_parts.push(format!(
                    "Vui lòng đóng Game và tắt Server {} đang chạy trước khi nhập tệp thế giới.",
                    active_ed.label()
                ));
            } else {
                let target_dir = self.worlds_dir_for_edition(active_ed);
                if let Err(e) = std::fs::create_dir_all(&target_dir) {
                    err_parts.push(format!("Không tạo được thư mục Worlds: {e}"));
                } else {
                    let mut copied = 0usize;
                    for src in world_paths {
                        let Some(fname) = src.file_name() else {
                            continue;
                        };
                        let dest = target_dir.join(fname);
                        if normalize_world_path_key(&src) != normalize_world_path_key(&dest) {
                            if let Err(e) = std::fs::copy(&src, &dest) {
                                err_parts.push(format!(
                                    "Không thể sao chép '{}': {e}",
                                    fname.to_string_lossy()
                                ));
                                continue;
                            }
                        }
                        copied += 1;
                    }
                    if copied > 0 {
                        self.refresh_worlds_and_backups();
                        if imported_mods_total == 0 {
                            self.active_tab = NavTab::Play;
                        }
                        ok_parts.push(format!(
                            "Đã nhập {copied} tệp thế giới vào {}",
                            active_ed.label()
                        ));
                    }
                }
            }
        }

        if !backup_zip_paths.is_empty() {
            if self.install_busy {
                err_parts.push(
                    "Đang có tiến trình cài đặt chạy, không thể nhập bản sao lưu lúc này."
                        .to_string(),
                );
            } else if let Err(e) = std::fs::create_dir_all(&self.paths.backups_dir) {
                err_parts.push(format!("Không tạo được thư mục Backups: {e}"));
            } else {
                let mut copied = 0usize;
                for src in backup_zip_paths {
                    let Some(fname) = src.file_name() else {
                        continue;
                    };
                    let mut dest = self.paths.backups_dir.join(fname);
                    if normalize_world_path_key(&src) != normalize_world_path_key(&dest) {
                        if dest.exists() {
                            let stem = src
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("backup");
                            let mut idx = 2usize;
                            loop {
                                let candidate =
                                    self.paths.backups_dir.join(format!("{stem}_{idx}.zip"));
                                if !candidate.exists()
                                    || normalize_world_path_key(&src)
                                        == normalize_world_path_key(&candidate)
                                {
                                    dest = candidate;
                                    break;
                                }
                                idx += 1;
                            }
                        }
                        if normalize_world_path_key(&src) != normalize_world_path_key(&dest) {
                            if let Err(e) = std::fs::copy(&src, &dest) {
                                err_parts.push(format!(
                                    "Không thể sao chép bản sao lưu '{}': {e}",
                                    fname.to_string_lossy()
                                ));
                                continue;
                            }
                        }
                    }
                    copied += 1;
                }
                if copied > 0 {
                    self.refresh_worlds_and_backups();
                    if imported_mods_total == 0 {
                        self.active_tab = NavTab::Play;
                    }
                    ok_parts.push(format!("Đã nhập {copied} bản sao lưu thế giới"));
                }
            }
        }

        if !unsupported_labels.is_empty() {
            err_parts.push(format!(
                "Định dạng không hỗ trợ: {}",
                unsupported_labels.join(", ")
            ));
        }

        if !err_parts.is_empty() && !ok_parts.is_empty() {
            self.notify_err(format!("{}. {}", ok_parts.join("; "), err_parts.join(" ")));
        } else if !err_parts.is_empty() {
            self.notify_err(err_parts.join(" "));
        } else if !ok_parts.is_empty() {
            self.notify_ok(format!("{}.", ok_parts.join("; ")));
        }
    }

    pub fn active_server_port(&self) -> u16 {
        let sup_active = self
            .active_server_supervisor()
            .is_some_and(|s| s.is_active());
        let snap_port = self.active_server_snapshot().map(|s| s.port);
        let showing_ext = self.showing_external_server();
        let ext_port = self.active_external_server().and_then(|e| e.port);
        let cfg_port = self.config.active_server().map(|p| p.config.port);
        resolve_active_server_port_value(sup_active, snap_port, showing_ext, ext_port, cfg_port)
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

    pub fn has_active_client_ipv6_bridge(&self) -> bool {
        self.client_ipv6_bridge.is_some() && self.is_game_running()
    }

    fn has_active_close_resources(&self) -> bool {
        let (has_hosted_server, has_direct_host, has_upnp, has_client_bridge) =
            self.close_resource_flags();
        has_hosted_server || has_direct_host || has_upnp || has_client_bridge
    }

    pub fn close_confirm_message(&self) -> &'static str {
        let (has_hosted_server, has_direct_host, has_upnp, has_client_bridge) =
            self.close_resource_flags();
        format_close_confirm_message(
            has_hosted_server,
            has_direct_host,
            has_upnp,
            has_client_bridge,
        )
    }

    pub fn should_block_close_for_active_bridge(&mut self) -> bool {
        let has_active = self.has_active_close_resources();
        let (should_block, newly_opened) = should_block_close_transition(
            self.close_bridge_allow_exit,
            has_active,
            &mut self.confirm_close_bridge_modal,
        );
        if newly_opened {
            self.notify_err(self.close_confirm_message());
        }
        should_block
    }

    pub fn confirm_close_bridge_and_exit(&mut self, ctx: &egui::Context) {
        self.close_bridge_allow_exit = true;
        self.confirm_close_bridge_modal = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    pub fn cancel_close_bridge_modal(&mut self) {
        self.confirm_close_bridge_modal = false;
    }

    fn stop_client_ipv6_bridge(&mut self) {
        if let Some(bridge) = self.client_ipv6_bridge.take() {
            bridge.stop();
        }
        self.confirm_close_bridge_modal = false;
        self.close_bridge_allow_exit = false;
    }

    pub fn launch_selected_game(&mut self, join_server: Option<(String, u16, String)>) {
        if self.install_busy {
            self.notify_err("Đang có tiến trình cài đặt chạy, không thể mở Game lúc này.");
            return;
        }
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
            if !selected_pack.is_empty() {
                let pack_dir = self.paths.tml_modpacks_dir().join(&selected_pack);
                if is_safe_single_dir_component(&selected_pack) && pack_dir.join("Mods").is_dir() {
                    match activate_modpack(&pack_dir, &self.paths.tml_mods_dir(), true) {
                        Ok(_) => self.refresh_mods_and_packs(),
                        Err(e) => {
                            self.notify_err(format!(
                                "Không thể kích hoạt ModPack '{selected_pack}': {e}"
                            ));
                            return;
                        }
                    }
                } else if !is_existing_valid_modpack(&self.paths, &selected_pack) {
                    self.notify_err(format!(
                        "ModPack '{selected_pack}' không tồn tại hoặc không hợp lệ."
                    ));
                    return;
                }
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
            match self.rt.block_on(start_ipv4_to_ipv6_client_bridge(v6_target)) {
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
                .is_some_and(|sys| terminate_game_process_by_pid(sys, ext.pid));
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

    fn has_running_server_for_edition(&self, edition: GameEdition) -> bool {
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
        supervised_conflict || external_conflict
    }

    fn check_install_edition_ready(&self, edition: Option<GameEdition>) -> Result<(), &'static str> {
        let running_game_ed = match edition {
            Some(GameEdition::TModLoader) if self.is_edition_client_running(GameEdition::TModLoader) => {
                Some(GameEdition::TModLoader)
            }
            None if self.is_edition_client_running(GameEdition::Vanilla) => Some(GameEdition::Vanilla),
            _ => self.running_game_edition(),
        };
        check_install_edition_ready_flags(
            self.install_busy,
            self.is_steamcmd_busy(),
            running_game_ed,
            self.has_running_server_for_edition(GameEdition::Vanilla),
            self.has_running_server_for_edition(GameEdition::TModLoader),
            edition,
        )
    }

    pub fn can_install_edition_ui(&self, edition: Option<GameEdition>) -> bool {
        self.check_install_edition_ready(edition).is_ok()
    }

    fn can_start_install(&mut self, overwriting_edition: Option<GameEdition>) -> bool {
        self.scan_external_servers();
        if let Err(msg) = self.check_install_edition_ready(overwriting_edition) {
            self.notify_err(msg);
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
        let token = non_empty_opt(&self.config.github_release_token).map(ToString::to_string);
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let res = fetch_tmodloader_releases(proxy.as_deref(), token.as_deref())
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
        let required_dotnet = read_required_tml_dotnet_version(&self.paths);
        let bundled_exact = detect_exact_bundled_dotnet_runtime(&self.paths, &required_dotnet);
        let also_install_dotnet = !bundled_exact
            && (self.env.dotnet8_version.is_none()
                || self
                    .paths
                    .tmodloader_dir
                    .join("dotnet")
                    .join("dotnet.exe")
                    .is_file());
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
        let token = non_empty_opt(&self.config.github_release_token).map(ToString::to_string);
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
                None => match fetch_tmodloader_releases(proxy.as_deref(), token.as_deref()).await {
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
        if !self.can_start_install(Some(GameEdition::TModLoader)) {
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
        if !self.can_start_install(Some(GameEdition::Vanilla)) {
            return;
        }
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
        if !self.can_start_install(None) {
            return;
        }
        let msi = self.paths.xna_msi_path();
        if msi.is_file() {
            self.install_busy = true;
            self.install_last_error = None;
            self.install_progress = Some(InstallProgress {
                target: InstallTarget::Xna40,
                stage: "Đang chạy trình cài đặt XNA 4.0...".to_string(),
                current: 0,
                total: None,
            });
            let tx = self.event_tx.clone();
            let wake = self.repaint.clone();
            self.rt.spawn_blocking(move || {
                let result = install_xna_framework_elevated(&msi)
                    .map(|()| {
                        "Đã chạy trình cài đặt XNA 4.0. Nhấn 'Làm mới kiểm tra' sau khi cài xong."
                            .to_string()
                    })
                    .map_err(|e| e.to_string());
                let _ = tx.send(AppEvent::XnaInstallFinished(result));
                wake.wake();
            });
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
        if self.install_busy {
            self.notify_err("Đang có tiến trình cài đặt chạy, không thể bật Server lúc này.");
            return;
        }
        self.scan_external_servers();
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
        let active_direct_ports =
            active_direct_host_ports(&self.direct_host_handles, Some(&profile.id));
        let port_used_by_other = self.server_supervisors.iter().any(|(id, sup)| {
            id != &profile.id
                && sup.is_active()
                && self.server_snapshots.get(id).map(|s| s.port) == Some(profile.config.port)
        }) || self
            .external_servers
            .iter()
            .any(|ext| ext.port == Some(profile.config.port))
            || active_direct_ports.contains(&profile.config.port);
        if port_used_by_other || self.rt.block_on(probe_local_tcp_listening(profile.config.port)) {
            self.scan_external_servers();
            self.notify_err(format!(
                "Cổng {} đang được tiến trình khác sử dụng (server khác hoặc Host & Play trong game). Hãy đổi cổng hoặc tắt tiến trình đang giữ cổng này.",
                profile.config.port
            ));
            return;
        }

        let active_sup_ids: HashSet<&str> = self
            .server_supervisors
            .iter()
            .filter(|(_, sup)| sup.is_active())
            .map(|(id, _)| id.as_str())
            .collect();
        if let Some(conflict_name) = find_conflicting_world_profile_name(
            &self.paths,
            &profile,
            &self.config.servers,
            &active_sup_ids,
            &self.server_snapshots,
            &self.external_servers,
        ) {
            self.notify_err(format!(
                "Thế giới này đang được sử dụng bởi hồ sơ server '{conflict_name}' đang chạy. Không thể chạy 2 server chung 1 tệp .wld cùng lúc!"
            ));
            return;
        }

        let handle = self.rt.handle().clone();
        let managed_by_direct_host = is_server_managed_by_direct_host(
            &profile.id,
            &self.pending_auto_direct_host_ports,
            &self.direct_host_handles,
        );
        let tuning = RuntimeTuning {
            tiered_pgo: self.config.dotnet_tiered_pgo,
            server_gc: self.config.dotnet_server_gc,
            process_priority: self.config.game_process_priority,
            disable_builtin_upnp: managed_by_direct_host,
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
            if ignore_profile_id == Some(id.as_str()) || !sup.is_active() || sup.is_stopping() {
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
        stopped_port: u16,
        stopped_profile_id: Option<&str>,
        stopped_pid: Option<u32>,
    ) {
        if let Some(pid) = stopped_profile_id {
            let another_active_same_profile = if stopped_pid.is_some() {
                self.server_supervisors
                    .get(pid)
                    .is_some_and(|s| s.is_active() && !s.is_stopping())
                    || self.external_servers.iter().any(|ext| {
                        stopped_pid != Some(ext.pid) && ext.profile_id.as_deref() == Some(pid)
                    })
            } else {
                self.external_servers
                    .iter()
                    .any(|ext| ext.profile_id.as_deref() == Some(pid))
            };

            if !another_active_same_profile {
                let handle_port =
                    self.direct_host_handles
                        .get(pid)
                        .and_then(|h| match h.current_status() {
                            DirectHostStatus::Starting { local_port, .. } => Some(local_port),
                            DirectHostStatus::Running(info) => Some(info.local_port),
                            DirectHostStatus::Stopped
                            | DirectHostStatus::Error(_)
                            | DirectHostStatus::Idle => None,
                        });
                let port_still_used = handle_port.is_some_and(|hp| {
                    self.is_port_used_by_other_active_server(hp, Some(pid), stopped_pid)
                });
                if !port_still_used {
                    self.stop_direct_host_for_profile(pid);
                }
            }
        }

        if stopped_port == 0
            || self.is_port_used_by_other_active_server(
                stopped_port,
                stopped_profile_id,
                stopped_pid,
            )
        {
            return;
        }

        let matching_ids: Vec<String> = self
            .direct_host_handles
            .iter()
            .filter_map(|(id, handle)| match handle.current_status() {
                DirectHostStatus::Starting { local_port, .. } if local_port == stopped_port => {
                    Some(id.clone())
                }
                DirectHostStatus::Running(info) if info.local_port == stopped_port => {
                    Some(id.clone())
                }
                _ => None,
            })
            .collect();

        for id in matching_ids {
            self.stop_direct_host_for_profile(&id);
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
            self.pending_auto_direct_host_ports.remove(ext_profile_id);
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
        self.pending_auto_direct_host_ports.remove(profile_id);
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
                self.notify_ok(
                    "Đang lưu thế giới & chờ server dừng — dùng nút 'Buộc dừng' nếu cần tắt ngay.",
                );
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
        self.pending_auto_direct_host_ports.remove(profile_id);
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
        self.pending_auto_direct_host_ports.clear();
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
            self.stop_all_direct_hosts();
        } else {
            self.scan_external_servers();
            let to_stop: Vec<String> = self
                .direct_host_handles
                .iter()
                .filter_map(|(id, handle)| {
                    let target_port = match handle.current_status() {
                        DirectHostStatus::Starting { local_port, .. } => Some(local_port),
                        DirectHostStatus::Running(info) => Some(info.local_port),
                        _ => None,
                    };
                    let still_used = target_port.is_some_and(|p| {
                        self.external_servers.iter().any(|ext| {
                            ext.port
                                .or_else(|| {
                                    ext.profile_id
                                        .as_deref()
                                        .and_then(|pid| {
                                            self.config.servers.iter().find(|s| s.id == pid)
                                        })
                                        .map(|s| s.config.port)
                                })
                                == Some(p)
                        })
                    });
                    if !still_used {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect();
            for id in to_stop {
                self.stop_direct_host_for_profile(&id);
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
        let safe_name = sanitize_server_console_arg(name);
        if safe_name.is_empty() {
            return;
        }
        self.send_server_command(&format!("kick {safe_name}"));
    }

    fn schedule_delayed_ban_refresh(&self, profile_id: String) {
        if profile_id.trim().is_empty() {
            return;
        }
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            tokio::time::sleep(Duration::from_millis(350)).await;
            let _ = tx.send(AppEvent::DelayedBanRefresh { profile_id });
            wake.wake();
        });
    }

    pub fn ban_player(&mut self, name: &str) {
        let target_name = name.trim();
        let safe_name = sanitize_server_console_arg(target_name);
        if safe_name.is_empty() {
            return;
        }
        let sup_active = self
            .active_server_supervisor()
            .is_some_and(|s| s.is_active());
        if !sup_active {
            self.send_server_command(&format!("ban {safe_name}"));
            return;
        }

        let player_ip_raw = self.active_server_snapshot().and_then(|snap| {
            snap.players
                .iter()
                .find(|p| p.name.trim() == target_name)
                .or_else(|| snap.players.iter().find(|p| p.name == name))
                .and_then(|p| p.ip.clone())
        });

        let resolution = resolve_player_ban_target(player_ip_raw.as_deref(), |port| {
            self.active_direct_host_handle()
                .and_then(|h| h.resolve_bridged_peer(port))
        });

        match resolution {
            PlayerBanResolution::RefuseLoopback => {
                self.notify_err(
                    "Không thể ban kết nối nội bộ (127.0.0.1 / ::1). Hãy dùng Kick nếu muốn ngắt kết nối.",
                );
            }
            PlayerBanResolution::BridgedRealIp(real_ip) => {
                let Some(profile) = self.config.active_server().cloned() else {
                    return;
                };
                let ban_path = self.resolve_profile_banlist_path(&profile);
                let mut entries = load_banlist(&ban_path);
                let already_banned = entries
                    .iter()
                    .any(|e| extract_ban_ip_key(&e.ip).map(normalize_ip_addr) == Some(real_ip));
                if !already_banned {
                    entries.push(BanEntry {
                        label: format!("Player: {safe_name}"),
                        ip: real_ip.to_string(),
                    });
                    if let Err(e) = save_banlist(&ban_path, &entries) {
                        self.notify_err(format!("Không lưu được banlist: {e}"));
                        return;
                    }
                }
                self.banlist_entries = entries;
                self.sync_banlist_to_direct_host();
                self.send_server_command(&format!("kick {safe_name}"));
                self.notify_ok(format!("Đã cấm {safe_name} ({real_ip})."));
            }
            PlayerBanResolution::DirectIp(direct_ip) => {
                self.send_server_command(&format!("ban {safe_name}"));
                self.refresh_banlist();
                if let Some(handle) = self.active_direct_host_handle() {
                    let mut ips = self.collect_banlist_ips_for_active();
                    if !ips.contains(&direct_ip) {
                        ips.push(direct_ip);
                    }
                    handle.set_banned_ips(ips);
                }
                self.schedule_delayed_ban_refresh(self.config.active_server_id.clone());
            }
            PlayerBanResolution::UnknownIp => {
                self.send_server_command(&format!("ban {safe_name}"));
                self.refresh_banlist();
                self.schedule_delayed_ban_refresh(self.config.active_server_id.clone());
            }
        }
    }

    pub fn refresh_online_players(&mut self) {
        let cmd_res = self
            .active_server_supervisor()
            .filter(|sup| sup.is_active() && sup.has_stdin())
            .map(|sup| sup.send_silent_command("playing"));
        if let Some(Err(e)) = cmd_res {
            self.notify_err(format!("Không làm mới được danh sách người chơi: {e}"));
        }
    }

    pub fn remove_ban_entry(&mut self, idx: usize) {
        let Some(profile) = self.config.active_server().cloned() else {
            return;
        };
        let Some(target) = self.banlist_entries.get(idx).cloned() else {
            return;
        };
        let path = self.resolve_profile_banlist_path(&profile);
        let mut disk_entries = load_banlist(&path);
        remove_ban_entry_from_disk_list(&mut disk_entries, &target);
        if let Err(e) = save_banlist(&path, &disk_entries) {
            self.notify_err(format!("Không lưu được banlist: {e}"));
        } else {
            self.banlist_entries = disk_entries;
            self.sync_banlist_to_direct_host();
            self.notify_ok("Đã cập nhật danh sách cấm.");
        }
    }

    pub fn add_manual_ban(&mut self) {
        let norm_ip = match validate_and_normalize_manual_ban_ip(&self.manual_ban_ip) {
            Ok(ip) => ip,
            Err(msg) => {
                self.notify_err(msg);
                return;
            }
        };
        let Some(profile) = self.config.active_server().cloned() else {
            return;
        };
        let label = if self.manual_ban_name.trim().is_empty() {
            "Cấm thủ công".to_string()
        } else {
            self.manual_ban_name.trim().to_string()
        };
        let path = self.resolve_profile_banlist_path(&profile);
        let mut next_entries = load_banlist(&path);
        let added = merge_manual_ban_entry(&mut next_entries, label, norm_ip);
        if added {
            if let Err(e) = save_banlist(&path, &next_entries) {
                self.notify_err(format!("Không lưu được banlist: {e}"));
                return;
            }
        }
        self.banlist_entries = next_entries;
        self.manual_ban_name.clear();
        self.manual_ban_ip.clear();
        self.sync_banlist_to_direct_host();

        let sup_active = self
            .active_server_supervisor()
            .is_some_and(|s| s.is_active());
        if sup_active {
            let matching_players: Vec<String> = self
                .active_server_snapshot()
                .map(|snap| {
                    snap.players
                        .iter()
                        .filter_map(|p| {
                            let resolved = resolve_player_ban_target(p.ip.as_deref(), |port| {
                                self.active_direct_host_handle()
                                    .and_then(|h| h.resolve_bridged_peer(port))
                            });
                            let matches = matches!(
                                resolved,
                                PlayerBanResolution::DirectIp(ip)
                                    | PlayerBanResolution::BridgedRealIp(ip)
                                    if ip == norm_ip
                            );
                            if matches {
                                let safe = sanitize_server_console_arg(&p.name);
                                (!safe.is_empty()).then_some(safe)
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            for safe_name in matching_players {
                self.send_server_command(&format!("kick {safe_name}"));
            }
        }

        if added {
            self.notify_ok(format!("Đã thêm {norm_ip} vào banlist.txt."));
        } else {
            self.notify_ok(format!("IP {norm_ip} đã có sẵn trong danh sách cấm."));
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

    pub fn active_direct_host_handle(&self) -> Option<&DirectPortHostHandle> {
        self.direct_host_handles.get(&self.config.active_server_id)
    }

    pub fn active_direct_host_status(&self) -> DirectHostStatus {
        self.active_direct_host_handle()
            .map(DirectPortHostHandle::current_status)
            .unwrap_or(DirectHostStatus::Idle)
    }

    pub fn has_pending_auto_direct_host(&self, profile_id: &str) -> bool {
        self.pending_auto_direct_host_ports.contains_key(profile_id)
    }

    pub fn cancel_pending_auto_direct_host(&mut self, profile_id: &str) {
        self.pending_auto_direct_host_ports.remove(profile_id);
    }

    pub fn active_direct_bind_ip(&self) -> Option<std::net::Ipv4Addr> {
        self.direct_bind_ips
            .get(&self.config.active_server_id)
            .copied()
            .flatten()
    }

    pub fn set_active_direct_bind_ip(&mut self, ip: Option<std::net::Ipv4Addr>) {
        if let Some(addr) = ip {
            self.direct_bind_ips
                .insert(self.config.active_server_id.clone(), Some(addr));
        } else {
            self.direct_bind_ips.remove(&self.config.active_server_id);
        }
    }

    pub fn is_active_profile_pending_direct_host(&self) -> bool {
        self.has_pending_auto_direct_host(&self.config.active_server_id)
    }

    pub fn format_player_ip_for_active(&self, raw: &str) -> String {
        format_player_ip_with_resolver(raw, |port| {
            self.active_direct_host_handle()
                .and_then(|h| h.resolve_bridged_peer(port))
        })
    }

    pub fn collect_banlist_ips_for_profile(&self, profile: &ServerProfile) -> Vec<IpAddr> {
        let entries = load_banlist(&self.resolve_profile_banlist_path(profile));
        collect_banlist_ips_from_entries(&entries)
    }

    fn collect_banlist_ips_for_profile_id(&self, profile_id: &str) -> Vec<IpAddr> {
        self.config
            .servers
            .iter()
            .find(|p| p.id == profile_id)
            .map(|p| self.collect_banlist_ips_for_profile(p))
            .unwrap_or_default()
    }

    fn collect_banlist_ips_for_active(&self) -> Vec<IpAddr> {
        self.config
            .active_server()
            .map(|p| self.collect_banlist_ips_for_profile(p))
            .unwrap_or_default()
    }

    pub fn sync_banlist_to_direct_host(&self) {
        if let Some(handle) = self.direct_host_handles.get(&self.config.active_server_id) {
            handle.set_banned_ips(self.collect_banlist_ips_for_active());
        }
    }

    pub fn active_online_endpoint(&self) -> Option<String> {
        if let DirectHostStatus::Running(info) = self.active_direct_host_status() {
            return info.preferred_online_endpoint();
        }
        None
    }

    pub fn stop_direct_host_for_profile(&mut self, profile_id: &str) {
        if let Some(h) = self.direct_host_handles.remove(profile_id) {
            h.stop();
        }
    }

    pub fn stop_all_direct_hosts(&mut self) {
        for (_, h) in self.direct_host_handles.drain() {
            h.stop();
        }
    }

    pub fn stop_direct_host(&mut self) {
        let active_id = self.config.active_server_id.clone();
        self.stop_direct_host_for_profile(&active_id);
    }

    fn start_direct_host_for_profile(
        &mut self,
        profile_id: String,
        local_target_port: u16,
        host_port: u16,
        try_upnp: bool,
    ) {
        if self.direct_host_handles.get(&profile_id).is_some_and(|h| {
            matches!(
                h.current_status(),
                DirectHostStatus::Running(_) | DirectHostStatus::Starting { .. }
            )
        }) {
            self.notify_ok("Port mạng đang mở.");
            return;
        }
        if local_target_port == 0 {
            self.notify_err("Cổng TCP Server phải từ 1 đến 65535");
            return;
        }
        if !self.firewall_busy && !check_firewall_rule_exists(host_port) {
            self.ensure_firewall(host_port);
        }
        self.stop_direct_host_for_profile(&profile_id);
        let initial_banned_ips = self.collect_banlist_ips_for_profile_id(&profile_id);
        let bind_ipv4 = self.direct_bind_ips.get(&profile_id).copied().flatten();
        let handle = start_direct_port_host(
            self.rt.handle(),
            DirectHostConfig {
                local_target_port,
                host_port,
                bind_ipv4,
                try_upnp,
                initial_banned_ips,
            },
            self.repaint.clone(),
        );
        self.direct_host_handles.insert(profile_id, handle);
        self.notify_ok("Đang mở Port mạng...");
    }

    fn start_direct_host_on_port(&mut self, local_target_port: u16) {
        let active_id = self
            .config
            .active_server()
            .map(|p| p.id.clone())
            .unwrap_or_else(|| self.config.active_server_id.clone());
        let host_port = self
            .direct_host_port_input
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|&p| p > 0)
            .unwrap_or(local_target_port);
        let try_upnp = self.direct_try_upnp;
        self.start_direct_host_for_profile(active_id, local_target_port, host_port, try_upnp);
    }

    pub fn toggle_direct_host(&mut self) {
        let active_id = self.config.active_server_id.clone();
        self.pending_auto_direct_host_ports.remove(&active_id);
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
        if self.install_busy {
            self.notify_err("Đang có tiến trình cài đặt chạy, không thể bật Server lúc này.");
            return;
        }
        let port = self.active_server_port();
        if port == 0 {
            self.notify_err("Cổng TCP Server phải từ 1 đến 65535");
            return;
        }
        let trimmed_host_port = self.direct_host_port_input.trim();
        let desired_host_port = if trimmed_host_port.is_empty() {
            port
        } else if let Some(valid) = trimmed_host_port.parse::<u16>().ok().filter(|&p| p > 0) {
            valid
        } else {
            self.notify_err("Cổng mở không hợp lệ (1 - 65535).");
            return;
        };
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
                    self.pending_auto_direct_host_ports.remove(&active_id);
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
                        self.pending_auto_direct_host_ports
                            .insert(active_id, desired_host_port);
                        self.notify_ok(
                            "Server đang khởi động — sẽ tự mở Port mạng ngay khi sẵn sàng.",
                        );
                    }
                }
            }
            return;
        }

        if self.rt.block_on(probe_local_tcp_listening(port)) {
            self.pending_auto_direct_host_ports.remove(&active_id);
            self.start_direct_host_on_port(port);
            self.spawn_network_probe();
            return;
        }

        self.pending_auto_direct_host_ports
            .insert(active_id.clone(), desired_host_port);
        self.start_selected_server();
        if self
            .server_supervisors
            .get(&active_id)
            .is_some_and(|s| s.is_active())
        {
            self.notify_ok("Đang khởi chạy Server và tự động mở Port mạng...");
        } else {
            self.pending_auto_direct_host_ports.remove(&active_id);
        }
    }

    pub fn join_own_hosted_server(&mut self) {
        if self.install_busy {
            self.notify_err("Đang có tiến trình cài đặt chạy, không thể mở Game lúc này.");
            return;
        }
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
        let (target_port, server_password, server_edition, server_pack) = if srv_active {
            let snap = self.active_server_snapshot();
            let active_cfg = self.config.active_server();
            let port = snap
                .map(|s| s.port)
                .filter(|&p| p > 0)
                .unwrap_or_else(|| self.active_server_port());
            let password = snap
                .map(|s| s.effective_password.clone())
                .unwrap_or_else(|| {
                    active_cfg
                        .map(|s| s.config.password.clone())
                        .unwrap_or_default()
                });
            let edition = snap
                .map(|s| s.edition)
                .or_else(|| active_cfg.map(|s| s.edition))
                .unwrap_or(self.config.active_edition);
            let pack = snap
                .map(|s| s.effective_modpack.trim().to_string())
                .unwrap_or_else(|| {
                    active_cfg
                        .map(|s| s.config.modpack.trim().to_string())
                        .unwrap_or_default()
                });
            (port, password, edition, pack)
        } else {
            let ext_server = self.active_external_server();
            let matched_profile = ext_server
                .and_then(|ext| ext.profile_id.as_deref())
                .and_then(|id| self.config.servers.iter().find(|s| s.id == id))
                .or_else(|| self.config.active_server());
            let port = ext_server
                .and_then(|ext| ext.port)
                .filter(|&p| p > 0)
                .unwrap_or_else(|| self.active_server_port());
            let password = matched_profile
                .map(|s| s.config.password.clone())
                .unwrap_or_default();
            let edition = ext_server
                .map(|ext| ext.edition)
                .or_else(|| matched_profile.map(|s| s.edition))
                .unwrap_or(self.config.active_edition);
            let pack = matched_profile
                .map(|s| s.config.modpack.trim().to_string())
                .unwrap_or_default();
            (port, password, edition, pack)
        };

        if server_edition == GameEdition::TModLoader {
            if !server_pack.is_empty() && !is_existing_valid_modpack(&self.paths, &server_pack) {
                self.notify_err(format!(
                    "ModPack '{server_pack}' của server không tồn tại hoặc bị hỏng."
                ));
                return;
            }
            if self.config.active_modpack != server_pack {
                self.config.active_modpack = server_pack;
            }
        }
        let edition_changed = self.config.active_edition != server_edition;
        self.config.active_edition = server_edition;
        if edition_changed {
            self.refresh_worlds_and_backups();
        }
        self.save_config();
        self.launch_selected_game(Some(("127.0.0.1".to_string(), target_port, server_password)));
    }

    pub fn join_saved_server(&mut self, idx: usize) {
        if self.install_busy {
            self.notify_err("Đang có tiến trình cài đặt chạy, không thể mở Game lúc này.");
            return;
        }
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
        let Some(srv) = self.config.saved_join_servers.get(idx).cloned() else {
            return;
        };
        let mut modpack_cleared = false;
        let pack_trimmed = srv.modpack.trim();
        if pack_trimmed.is_empty() {
            if !self.config.active_modpack.is_empty() {
                self.config.active_modpack.clear();
                modpack_cleared = true;
            }
        } else if srv.edition == GameEdition::TModLoader {
            if !is_existing_valid_modpack(&self.paths, pack_trimmed) {
                self.notify_err(format!(
                    "ModPack '{pack_trimmed}' của server đã lưu không còn tồn tại trên máy."
                ));
                return;
            }
            self.config.active_modpack = pack_trimmed.to_string();
        }
        let edition_changed = self.config.active_edition != srv.edition;
        self.config.active_edition = srv.edition;
        if edition_changed {
            self.refresh_worlds_and_backups();
        }
        if modpack_cleared {
            self.refresh_mods_and_packs();
        }
        self.save_config();
        self.launch_selected_game(Some((srv.host, srv.port, srv.password)));
    }

    pub fn load_saved_join_server_to_form(&mut self, idx: usize) {
        let Some((edition_changed, missing_pack)) = apply_saved_join_server_to_config(
            &self.paths,
            &mut self.config,
            &mut self.quick_join_port_input,
            idx,
        ) else {
            return;
        };
        if edition_changed {
            self.refresh_worlds_and_backups();
        }
        self.save_config();
        if let Some(missing) = missing_pack {
            self.notify_err(format!(
                "Đã nạp thông tin server vào form, nhưng ModPack '{missing}' không còn tồn tại."
            ));
        } else {
            self.notify_ok("Đã nạp thông tin server vào form.");
        }
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

    pub fn is_steamcmd_busy(&self) -> bool {
        self.steamcmd_busy || self.workshop_busy || self.modpack_sync_busy
    }

    fn sync_active_modpack_if_needed(&mut self, deleted_mod: Option<&str>) {
        let active_pack = self.config.active_modpack.trim();
        if active_pack.is_empty() || !is_safe_single_dir_component(active_pack) {
            return;
        }
        let _ = sync_active_modpack_from_mods_dir(
            &self.paths.tml_modpacks_dir(),
            &self.paths.tml_mods_dir(),
            active_pack,
            deleted_mod,
        );
    }

    fn is_tml_client_running(&self) -> bool {
        self.is_edition_client_running(GameEdition::TModLoader)
    }

    fn has_active_tml_server_matching_pack(&self, pack_matches: impl Fn(&str) -> bool) -> bool {
        let supervised_match = self.server_supervisors.iter().any(|(id, sup)| {
            if !sup.is_active() {
                return false;
            }
            if let Some(snap) = self.server_snapshots.get(id) {
                return snap.edition == GameEdition::TModLoader
                    && pack_matches(snap.effective_modpack.trim());
            }
            self.config.servers.iter().find(|p| p.id == *id).is_some_and(
                |p| p.edition == GameEdition::TModLoader && pack_matches(p.config.modpack.trim()),
            )
        });
        if supervised_match {
            return true;
        }
        self.external_servers.iter().any(|ext| {
            if ext.edition != GameEdition::TModLoader {
                return false;
            }
            let ext_pack = ext
                .profile_id
                .as_deref()
                .and_then(|pid| self.config.servers.iter().find(|p| p.id == pid))
                .map(|p| p.config.modpack.trim())
                .unwrap_or("");
            pack_matches(ext_pack)
        })
    }

    pub fn is_tml_mods_locked(&self) -> bool {
        if self.is_tml_client_running() {
            return true;
        }
        let active_pack = self.config.active_modpack.trim();
        self.has_active_tml_server_matching_pack(|srv_pack| {
            srv_pack.is_empty() || (!active_pack.is_empty() && srv_pack == active_pack)
        })
    }

    pub fn is_tml_mods_locked_by_runtime(&mut self) -> bool {
        self.scan_external_servers();
        self.is_tml_mods_locked()
    }

    pub fn import_mods_dialog(&mut self) {
        if self.is_tml_mods_locked_by_runtime() {
            self.notify_err(
                "Vui lòng đóng tModLoader và tắt Server đang dùng bộ Mod này trước khi nhập mod.",
            );
            return;
        }
        let Some(files) = rfd::FileDialog::new()
            .add_filter("tModLoader Mods", &["tmod", "zip"])
            .pick_files()
        else {
            return;
        };
        if self.is_tml_mods_locked_by_runtime() {
            self.notify_err(
                "Vui lòng đóng tModLoader và tắt Server đang dùng bộ Mod này trước khi nhập mod.",
            );
            return;
        }
        match import_local_mod_files(&files, &self.paths, true) {
            Ok(imported) => {
                for meta in &imported {
                    self.clear_stale_mod_provenance(&meta.internal_name);
                }
                self.sync_active_modpack_if_needed(None);
                self.refresh_mods_and_packs();
                self.notify_ok(format!("Đã nhập và bật {} mod.", imported.len()));
            }
            Err(e) => self.notify_err(e.to_string()),
        }
    }

    pub fn toggle_mod(&mut self, internal_name: &str, enable: bool) {
        if self.is_tml_mods_locked_by_runtime() {
            self.notify_err(
                "Vui lòng đóng tModLoader và tắt Server đang dùng bộ Mod này trước khi bật/tắt mod.",
            );
            return;
        }
        match set_mod_enabled(&self.paths, internal_name, enable) {
            Ok(_) => {
                self.sync_active_modpack_if_needed(None);
                self.refresh_mods_and_packs();
            }
            Err(e) => self.notify_err(e.to_string()),
        }
    }

    pub fn enable_mod_with_deps(&mut self, internal_name: &str) {
        if self.is_tml_mods_locked_by_runtime() {
            self.notify_err(
                "Vui lòng đóng tModLoader và tắt Server đang dùng bộ Mod này trước khi bật mod.",
            );
            return;
        }
        match enable_mod_and_installed_deps(&self.paths, internal_name) {
            Ok(newly_enabled) => {
                self.sync_active_modpack_if_needed(None);
                self.refresh_mods_and_packs();
                if newly_enabled.len() > 1 {
                    self.notify_ok(format!(
                        "Đã bật {} mod (bao gồm phụ thuộc): {}.",
                        newly_enabled.len(),
                        newly_enabled.join(", ")
                    ));
                } else if newly_enabled.len() == 1 {
                    self.notify_ok(format!("Đã bật mod {}.", newly_enabled[0]));
                } else {
                    self.notify_ok(format!(
                        "Mod {internal_name} và các phụ thuộc đã được bật sẵn."
                    ));
                }
            }
            Err(e) => self.notify_err(e.to_string()),
        }
    }

    pub fn delete_mod(&mut self, mod_path: &std::path::Path, internal_name: &str) {
        if self.is_tml_mods_locked_by_runtime() {
            self.notify_err(
                "Vui lòng đóng tModLoader và tắt Server đang dùng bộ Mod này trước khi xóa mod.",
            );
            return;
        }
        match delete_installed_mod(&self.paths, mod_path, internal_name) {
            Ok(()) => {
                self.clear_stale_mod_provenance(internal_name);
                self.sync_active_modpack_if_needed(Some(internal_name));
                self.refresh_mods_and_packs();
                self.notify_ok(format!("Đã xóa mod {internal_name}."));
            }
            Err(e) => self.notify_err(e.to_string()),
        }
    }

    pub fn deactivate_modpack(&mut self) {
        if self.is_tml_mods_locked_by_runtime() {
            self.notify_err(
                "Vui lòng đóng tModLoader và tắt Server đang dùng bộ Mod này trước khi tắt Gói Mod.",
            );
            return;
        }
        self.config.active_modpack.clear();
        self.save_config();
        self.refresh_mods_and_packs();
        self.notify_ok("Đã tắt chế độ Gói Mod cố định.");
    }

    pub fn create_new_modpack(&mut self) {
        if self.is_tml_mods_locked_by_runtime() {
            self.notify_err(
                "Vui lòng đóng tModLoader và tắt Server đang dùng bộ Mod này trước khi tạo ModPack.",
            );
            return;
        }
        let _ = self.create_modpack_internal();
    }

    fn create_modpack_internal(&mut self) -> Option<ModPackInfo> {
        if self.is_tml_mods_locked_by_runtime() {
            self.notify_err(
                "Vui lòng đóng tModLoader và tắt Server đang dùng bộ Mod này trước khi tạo ModPack.",
            );
            return None;
        }
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
        if self.is_tml_mods_locked_by_runtime()
            || self.has_active_tml_server_matching_pack(|srv_pack| {
                srv_pack.is_empty() || srv_pack == pack.name
            })
        {
            self.notify_err(
                "Vui lòng đóng tModLoader và tắt Server đang dùng bộ Mod này trước khi chuyển Gói Mod.",
            );
            return;
        }
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
        self.scan_external_servers();
        if self.is_tml_client_running() && self.config.active_modpack.trim() == pack.name {
            self.notify_err("Không thể xóa Gói Mod đang được tModLoader sử dụng. Hãy đóng game trước.");
            return;
        }
        if self.has_active_tml_server_matching_pack(|srv_pack| srv_pack == pack.name) {
            self.notify_err(
                "Không thể xóa Gói Mod đang được Server tModLoader sử dụng. Hãy tắt server trước.",
            );
            return;
        }
        match delete_modpack(&pack.path) {
            Ok(()) => {
                let mut config_changed = false;
                if self.config.active_modpack == pack.name {
                    self.config.active_modpack.clear();
                    config_changed = true;
                }
                for srv in &mut self.config.servers {
                    if srv.config.modpack.trim() == pack.name {
                        srv.config.modpack.clear();
                        config_changed = true;
                    }
                }
                if config_changed {
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
        if self.is_tml_mods_locked_by_runtime() {
            self.notify_err(
                "Vui lòng đóng tModLoader và tắt Server đang dùng bộ Mod này trước khi đồng bộ ModPack.",
            );
            return;
        }
        if self.is_steamcmd_busy() {
            self.notify_err("Đang có tác vụ SteamCMD/Workshop/ModPack khác chạy, vui lòng chờ xong.");
            return;
        }
        let payload = match decode_modpack_share_key(&self.share_key_input) {
            Ok(p) => p,
            Err(e) => {
                self.notify_err(e.to_string());
                return;
            }
        };
        let desired_pack_name = self.share_import_pack_name.trim().to_string();
        if !desired_pack_name.is_empty()
            && self.has_active_tml_server_matching_pack(|srv_pack| srv_pack == desired_pack_name)
        {
            self.notify_err(
                "Vui lòng tắt Server tModLoader đang dùng ModPack này trước khi đồng bộ.",
            );
            return;
        }

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
        let token = non_empty_opt(&self.config.github_release_token).map(ToString::to_string);
        let active_modpack = self.config.active_modpack.clone();
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
                token.as_deref(),
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
        let token = non_empty_opt(&self.config.github_release_token).map(ToString::to_string);
        self.github_repo_input = repo.clone();
        self.github_busy = true;
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let result = fetch_github_latest_tmod(&repo, proxy.as_deref(), token.as_deref())
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
        if self.is_tml_mods_locked_by_runtime() {
            self.notify_err(
                "Vui lòng đóng tModLoader và tắt Server đang dùng bộ Mod này trước khi tải mod.",
            );
            return;
        }
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

    pub fn is_workshop_id_installed(&self, workshop_id: &str) -> bool {
        let trimmed = workshop_id.trim();
        !trimmed.is_empty() && self.workshop_install_status(trimmed, 0).is_some()
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
        let seed_children = details
            .as_ref()
            .map(|d| d.child_ids.clone())
            .unwrap_or_default();
        self.start_workshop_download_queue(id, seed_children, details);
    }

    pub fn download_workshop_with_missing_deps(
        &mut self,
        workshop_id: String,
        child_ids: Vec<String>,
        primary_details: Option<WorkshopItemDetails>,
    ) {
        let primary_id = workshop_id.trim().to_string();
        if primary_id.is_empty() || !primary_id.chars().all(|c| c.is_ascii_digit()) {
            self.notify_err("Workshop ID không hợp lệ.");
            return;
        }
        self.start_workshop_download_queue(primary_id, child_ids, primary_details);
    }

    fn collect_installed_workshop_ids(&self) -> HashSet<String> {
        let mut out = HashSet::new();
        for rec in &self.workshop_records {
            let wid = rec.published_file_id.trim();
            if !wid.is_empty() && self.is_workshop_id_installed(wid) {
                out.insert(wid.to_string());
            }
        }
        for staged in &self.staged_workshop_mods {
            let wid = staged.workshop_id.trim();
            if !wid.is_empty() && self.is_workshop_id_installed(wid) {
                out.insert(wid.to_string());
            }
        }
        out
    }

    fn start_workshop_download_queue(
        &mut self,
        primary_id: String,
        seed_child_ids: Vec<String>,
        primary_details: Option<WorkshopItemDetails>,
    ) {
        if self.is_tml_mods_locked_by_runtime() {
            self.notify_err(
                "Vui lòng đóng tModLoader và tắt Server đang dùng bộ Mod này trước khi tải mod Workshop.",
            );
            return;
        }
        let Some(steamcmd_exe) = self.env.steamcmd_path.clone() else {
            self.notify_err("Chưa có SteamCMD. Nhấn 'Cài SteamCMD tự động' ở trên.");
            return;
        };
        if self.is_steamcmd_busy() {
            self.notify_err("Đang xử lý tác vụ SteamCMD/Workshop/ModPack khác, vui lòng chờ xong.");
            return;
        }

        let mut preloaded_full: HashMap<String, WorkshopItemDetails> = HashMap::new();
        let mut fallback_details: HashMap<String, WorkshopItemDetails> = HashMap::new();
        for item in &self.workshop_results {
            fallback_details.insert(item.published_file_id.clone(), item.clone());
        }
        if let Some(ref d) = self.workshop_details {
            preloaded_full.insert(d.published_file_id.clone(), d.clone());
            fallback_details.insert(d.published_file_id.clone(), d.clone());
        }
        if let Some(d) = primary_details {
            let wid = d.published_file_id.clone();
            if !d.child_ids.is_empty() || !seed_child_ids.is_empty() {
                preloaded_full.insert(wid.clone(), d.clone());
            }
            fallback_details.insert(wid, d);
        }

        let installed_wids = self.collect_installed_workshop_ids();

        self.workshop_busy = true;
        self.workshop_downloading_id = Some(primary_id.clone());
        self.workshop_logs.clear();

        let paths = self.paths.clone();
        let proxy = non_empty_opt(&self.config.proxy_url).map(ToString::to_string);
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();

        self.rt.spawn(async move {
            let _ = tx.send(AppEvent::WorkshopLog(format!(
                "=== Kiểm tra mod phụ thuộc cho Workshop ID {primary_id} ==="
            )));
            wake.wake();

            let mut bfs_queue: VecDeque<String> = VecDeque::new();
            let mut discovered: HashSet<String> = HashSet::new();
            let mut details_map: HashMap<String, WorkshopItemDetails> = HashMap::new();
            let mut adj: HashMap<String, Vec<String>> = HashMap::new();
            let mut hit_cap = false;

            discovered.insert(primary_id.clone());
            bfs_queue.push_back(primary_id.clone());

            while let Some(curr) = bfs_queue.pop_front() {
                let details_opt = match preloaded_full.get(&curr).cloned() {
                    Some(d) => Some(d),
                    None => fetch_workshop_details(&curr, proxy.as_deref())
                        .await
                        .ok()
                        .or_else(|| fallback_details.get(&curr).cloned()),
                };

                let mut children: Vec<String> = Vec::new();
                if let Some(details) = details_opt {
                    for raw_cid in &details.child_ids {
                        let cid = raw_cid.trim().to_string();
                        if is_valid_workshop_child_id(&curr, &cid, &children) {
                            children.push(cid);
                        }
                    }
                    details_map.insert(curr.clone(), details);
                }
                if curr == primary_id {
                    for raw_cid in &seed_child_ids {
                        let cid = raw_cid.trim().to_string();
                        if is_valid_workshop_child_id(&curr, &cid, &children) {
                            children.push(cid);
                        }
                    }
                }

                let mut next_children = Vec::new();
                for cid in children {
                    if discovered.contains(&cid) {
                        next_children.push(cid);
                    } else if discovered.len() < MAX_WORKSHOP_QUEUE {
                        discovered.insert(cid.clone());
                        bfs_queue.push_back(cid.clone());
                        next_children.push(cid);
                    } else {
                        hit_cap = true;
                    }
                }
                adj.insert(curr, next_children);
            }

            if hit_cap {
                let _ = tx.send(AppEvent::WorkshopLog(format!(
                    "Đã giới hạn hàng đợi tối đa {MAX_WORKSHOP_QUEUE} mục Workshop để tránh vòng phụ thuộc quá lớn."
                )));
                wake.wake();
            }

            let order = resolve_workshop_install_order(&primary_id, &adj, &installed_wids);
            let total = order.len();
            let mut installed = Vec::new();
            let mut error = None;

            for (idx, wid) in order.into_iter().enumerate() {
                if total > 1 {
                    let _ = tx.send(AppEvent::WorkshopLog(format!(
                        "=== Tải Workshop ID {wid} ({}/{total}) ===",
                        idx + 1
                    )));
                    wake.wake();
                }
                let details_opt = details_map.get(&wid).cloned();
                let log_tx = tx.clone();
                let log_wake = wake.clone();
                match download_workshop_mod_via_steamcmd(
                    &steamcmd_exe,
                    &paths,
                    &wid,
                    proxy.as_deref(),
                    true,
                    move |line| {
                        let _ = log_tx.send(AppEvent::WorkshopLog(line));
                        log_wake.wake();
                    },
                )
                .await
                {
                    Ok(mods) => installed.push((wid, details_opt, mods)),
                    Err(e) => {
                        error = Some(if total > 1 {
                            format!("Lỗi tải Workshop ID {wid}: {e}")
                        } else {
                            e.to_string()
                        });
                        break;
                    }
                }
            }

            let _ = tx.send(AppEvent::WorkshopBatchFinished {
                primary_id,
                installed,
                error,
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
        if self.install_busy {
            self.notify_err("Đang có tiến trình cài đặt khác đang chạy, vui lòng chờ.");
            return;
        }
        if self.is_steamcmd_busy() {
            self.notify_err("Đang có tác vụ SteamCMD/Workshop/ModPack khác chạy, vui lòng chờ xong.");
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

    fn is_world_in_use_by_active_server(&self, world_path: &Path) -> bool {
        let target_key = normalize_world_path_key(world_path);
        let sup_match = self.server_supervisors.iter().any(|(id, sup)| {
            if !sup.is_active() {
                return false;
            }
            let snap = self.server_snapshots.get(id);
            let snap_world = snap
                .map(|s| s.effective_world.trim())
                .filter(|s| !s.is_empty());
            if let Some(profile) = self.config.servers.iter().find(|p| p.id == *id) {
                resolve_profile_world_path_with_override(&self.paths, profile, snap_world)
                    .is_some_and(|p| normalize_world_path_key(&p) == target_key)
            } else {
                snap.and_then(|s| snap_world.map(|w| (s.edition, w)))
                    .is_some_and(|(edition, world_str)| {
                        let p = PathBuf::from(world_str);
                        let resolved = if p.is_absolute() {
                            p
                        } else {
                            self.worlds_dir_for_edition(edition).join(p)
                        };
                        normalize_world_path_key(&resolved) == target_key
                    })
            }
        });
        if sup_match {
            return true;
        }
        self.external_servers.iter().any(|ext| {
            ext.profile_id.as_deref().is_some_and(|pid| {
                self.config
                    .servers
                    .iter()
                    .find(|p| p.id == pid)
                    .and_then(|p| resolve_profile_world_path(&self.paths, p))
                    .is_some_and(|p| normalize_world_path_key(&p) == target_key)
            })
        })
    }

    fn backup_world_path(&mut self, world_path: &Path) {
        match backup_world(world_path, &self.paths.backups_dir) {
            Ok(info) => {
                self.refresh_worlds_and_backups();
                self.notify_ok(format!("Đã sao lưu thế giới vào {}", info.file_name));
            }
            Err(e) => self.notify_err(format!("Sao lưu thất bại: {e}")),
        }
    }

    pub fn backup_world_file(&mut self, world: &WorldMetadata) {
        if self.install_busy {
            self.notify_err("Đang có tiến trình cài đặt chạy, không thể sao lưu lúc này.");
            return;
        }
        self.scan_external_servers();
        if self.running_game_edition() == Some(self.config.active_edition)
            || self.is_world_in_use_by_active_server(&world.file_path)
        {
            self.notify_err(
                "Vui lòng đóng Game và tắt Server đang dùng thế giới này trước khi sao lưu.",
            );
            return;
        }
        self.backup_world_path(&world.file_path);
    }

    pub fn backup_active_server_world(&mut self) {
        if self.install_busy {
            self.notify_err("Đang có tiến trình cài đặt chạy, không thể sao lưu lúc này.");
            return;
        }
        self.scan_external_servers();
        let Some(profile) = self.config.active_server().cloned() else {
            self.notify_err("Chưa có cấu hình server nào.");
            return;
        };
        let sup_active = self
            .server_supervisors
            .get(&profile.id)
            .is_some_and(|s| s.is_active());
        let snap_world = if sup_active {
            self.server_snapshots
                .get(&profile.id)
                .map(|s| s.effective_world.as_str())
                .filter(|s| !s.trim().is_empty())
        } else {
            None
        };
        let Some(world_path) =
            resolve_profile_world_path_with_override(&self.paths, &profile, snap_world)
        else {
            self.notify_err("Hồ sơ server chưa cấu hình file thế giới (.wld).");
            return;
        };
        if self.is_profile_running(&profile.id) {
            self.notify_err(
                "Vui lòng tắt Server đang chạy trước khi sao lưu thế giới để tránh hỏng dữ liệu.",
            );
            return;
        }
        if self.running_game_edition() == Some(profile.edition)
            || self.is_world_in_use_by_active_server(&world_path)
        {
            self.notify_err(
                "Vui lòng đóng Game và tắt Server đang dùng thế giới này trước khi sao lưu.",
            );
            return;
        }
        if !world_path.is_file() {
            self.notify_err(format!(
                "File thế giới chưa tồn tại ({}). Hãy khởi chạy server để tạo thế giới hoặc chọn thế giới có sẵn.",
                world_path.display()
            ));
            return;
        }
        self.backup_world_path(&world_path);
    }

    pub fn can_restore_world_backup(&self, bak: &WorldBackupInfo) -> Result<(), &'static str> {
        let active_ed = self.config.active_edition;
        let running_ed = self
            .is_edition_client_running(active_ed)
            .then_some(active_ed);
        check_restore_world_backup_flags(
            self.install_busy,
            active_ed,
            running_ed,
            self.has_running_server_for_edition(active_ed),
            bak.detected_edition,
        )
    }

    pub fn restore_backup(&mut self, backup: &WorldBackupInfo) {
        self.scan_external_servers();
        if let Err(msg) = self.can_restore_world_backup(backup) {
            self.notify_err(msg);
            return;
        }
        let target_dir = self.worlds_dir_for_edition(self.config.active_edition);
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
        let id = format!("srv-{}", uuid_short());
        let edition = self.new_profile_edition;
        let worlds_dir = self.worlds_dir_for_edition(edition);
        let world_stem =
            generate_unique_profile_world_stem(&self.paths, &worlds_dir, &self.config.servers);
        let port = next_available_server_port(&self.config.servers);
        let profile = ServerProfile {
            id: id.clone(),
            name: final_name,
            edition,
            auto_restart: false,
            extra_args: String::new(),
            config: ServerConfigData {
                port,
                worldpath: worlds_dir.to_string_lossy().into_owned(),
                world: worlds_dir
                    .join(format!("{world_stem}.wld"))
                    .to_string_lossy()
                    .into_owned(),
                worldname: world_stem,
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
        self.scan_external_servers();
        let id = self.config.active_server_id.clone();
        if self.is_profile_busy_or_direct_hosted(&id) {
            self.notify_err("Vui lòng tắt server hoặc Port mạng đang chạy của hồ sơ này trước khi xóa.");
            return;
        }
        if let Some(sup) = self.server_supervisors.remove(&id) {
            if sup.is_active() {
                sup.request_graceful_stop();
            }
        }
        self.server_snapshots.remove(&id);
        self.external_log_summaries.remove(&id);
        self.cleared_external_log_pids.remove(&id);
        self.stop_direct_host_for_profile(&id);
        self.pending_auto_direct_host_ports.remove(&id);
        self.direct_bind_ips.remove(&id);
        if is_safe_single_dir_component(&id) {
            let server_profile_dir = self.paths.servers_dir.join(&id);
            if server_profile_dir.parent() == Some(self.paths.servers_dir.as_path())
                && server_profile_dir.is_dir()
            {
                let _ = std::fs::remove_dir_all(&server_profile_dir);
            }
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
        let modpack = if self.config.active_edition == GameEdition::TModLoader {
            self.config.active_modpack.trim().to_string()
        } else {
            String::new()
        };
        if let Some(existing) = self.config.saved_join_servers.iter_mut().find(|s| {
            s.host.eq_ignore_ascii_case(&host)
                && s.port == final_port
                && s.edition == self.config.active_edition
        }) {
            existing.name = name;
            existing.password = self.config.quick_join_password.clone();
            existing.modpack = modpack;
        } else {
            self.config.saved_join_servers.push(SavedJoinServer {
                name,
                host,
                port: final_port,
                password: self.config.quick_join_password.clone(),
                edition: self.config.active_edition,
                modpack,
            });
        }
        self.new_join_name.clear();
        self.save_config();
        self.notify_ok("Đã lưu server vào danh sách.");
    }

    pub fn shutdown_all(&mut self) {
        self.save_config();
        self.pending_auto_direct_host_ports.clear();
        self.stop_client_ipv6_bridge();

        let handles: Vec<DirectPortHostHandle> =
            self.direct_host_handles.drain().map(|(_, h)| h).collect();
        for handle in &handles {
            handle.stop();
        }
        let upnp_deadline = Instant::now() + Duration::from_millis(800);
        for handle in handles {
            let remaining = upnp_deadline.saturating_duration_since(Instant::now());
            handle.stop_and_wait(remaining);
        }

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
            let deadline = Instant::now() + Duration::from_secs(12);
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

const MAX_WORKSHOP_QUEUE: usize = 32;

fn is_valid_workshop_child_id(parent: &str, cid: &str, existing: &[String]) -> bool {
    !cid.is_empty()
        && cid.chars().all(|c| c.is_ascii_digit())
        && cid != parent
        && !existing.iter().any(|e| e == cid)
}

fn collect_workshop_postorder(
    node: &str,
    adj: &HashMap<String, Vec<String>>,
    visiting: &mut HashSet<String>,
    visited: &mut HashSet<String>,
    postorder: &mut Vec<String>,
) {
    if visited.contains(node) || !visiting.insert(node.to_string()) {
        return;
    }
    if let Some(children) = adj.get(node) {
        for child in children {
            collect_workshop_postorder(child, adj, visiting, visited, postorder);
        }
    }
    visiting.remove(node);
    visited.insert(node.to_string());
    postorder.push(node.to_string());
}

fn resolve_workshop_install_order(
    primary_id: &str,
    adj: &HashMap<String, Vec<String>>,
    installed_wids: &HashSet<String>,
) -> Vec<String> {
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    let mut postorder = Vec::new();
    collect_workshop_postorder(primary_id, adj, &mut visiting, &mut visited, &mut postorder);
    postorder
        .into_iter()
        .filter(|wid| wid == primary_id || !installed_wids.contains(wid))
        .collect()
}

fn format_close_confirm_message(
    has_hosted_server: bool,
    has_direct_host: bool,
    has_upnp: bool,
    has_client_bridge: bool,
) -> &'static str {
    match (
        has_hosted_server,
        has_direct_host,
        has_upnp,
        has_client_bridge,
    ) {
        (true, _, true, true) | (true, true, false, true) => {
            "Server đang chạy, Port mạng / UPnP và cầu nối IPv6 của game sẽ bị đóng khi thoát Launcher."
        }
        (true, _, true, false) => {
            "Server đang chạy cùng Port mạng và cổng UPnP sẽ bị tắt/gỡ khi thoát Launcher."
        }
        (true, true, false, false) => {
            "Server đang chạy và Port mạng đang mở sẽ bị tắt/ngắt kết nối khi thoát Launcher."
        }
        (true, false, false, true) => {
            "Server đang chạy và cầu nối IPv6 của game sẽ bị tắt/ngắt kết nối khi thoát Launcher."
        }
        (true, false, false, false) => {
            "Server đang chạy sẽ được lưu và tắt khi thoát Launcher."
        }
        (false, _, true, true) => {
            "Port mạng / cổng UPnP đang mở và cầu nối IPv6 của game sẽ bị đóng khi thoát Launcher."
        }
        (false, true, false, true) => {
            "Port mạng đang mở và cầu nối IPv6 của game sẽ bị ngắt kết nối khi thoát Launcher."
        }
        (false, _, true, false) => {
            "Port mạng và cổng UPnP đang mở sẽ bị đóng/gỡ khi thoát Launcher."
        }
        (false, true, false, false) => {
            "Port mạng đang mở sẽ bị đóng và ngắt kết nối người chơi khi thoát Launcher."
        }
        (false, false, false, true) | (false, false, false, false) => {
            "Game đang kết nối qua cầu nối IPv6 của Launcher — đóng sẽ ngắt kết nối server."
        }
    }
}

fn should_block_close_transition(
    allow_exit: bool,
    has_active_resources: bool,
    confirm_modal_open: &mut bool,
) -> (bool, bool) {
    if allow_exit || !has_active_resources {
        return (false, false);
    }
    let newly_opened = !*confirm_modal_open;
    if newly_opened {
        *confirm_modal_open = true;
    }
    (true, newly_opened)
}

fn check_install_edition_ready_flags(
    install_busy: bool,
    steamcmd_busy: bool,
    running_game_ed: Option<GameEdition>,
    vanilla_server_running: bool,
    tml_server_running: bool,
    target_edition: Option<GameEdition>,
) -> Result<(), &'static str> {
    if install_busy {
        return Err("Đang có tiến trình cài đặt khác đang chạy, vui lòng chờ.");
    }
    if steamcmd_busy {
        return Err("Đang có tác vụ SteamCMD/Workshop/ModPack khác chạy, vui lòng chờ xong.");
    }
    match target_edition {
        Some(GameEdition::Vanilla) => {
            if running_game_ed.is_some() {
                return Err(
                    "Vui lòng đóng Terraria và tModLoader đang chạy trước khi cài đặt/cập nhật Terraria.",
                );
            }
            if vanilla_server_running || tml_server_running {
                return Err(
                    "Vui lòng tắt Server Terraria và tModLoader đang chạy trước khi cài đặt/cập nhật Terraria.",
                );
            }
        }
        Some(GameEdition::TModLoader) => {
            if running_game_ed == Some(GameEdition::TModLoader) {
                return Err("Vui lòng đóng tModLoader đang chạy trước khi cài đặt/cập nhật.");
            }
            if tml_server_running {
                return Err(
                    "Vui lòng tắt server tModLoader đang chạy trước khi cài đặt/cập nhật.",
                );
            }
        }
        None => {
            if running_game_ed == Some(GameEdition::Vanilla) {
                return Err("Vui lòng đóng Terraria đang chạy trước khi cài đặt XNA 4.0.");
            }
            if vanilla_server_running {
                return Err("Vui lòng tắt server Terraria đang chạy trước khi cài đặt XNA 4.0.");
            }
        }
    }
    Ok(())
}

fn check_restore_world_backup_flags(
    install_busy: bool,
    active_ed: GameEdition,
    running_game_ed: Option<GameEdition>,
    server_running_for_active_ed: bool,
    detected_edition: Option<GameEdition>,
) -> Result<(), &'static str> {
    if install_busy {
        return Err("Đang có tiến trình cài đặt chạy, không thể khôi phục thế giới lúc này.");
    }
    if running_game_ed == Some(active_ed) {
        return Err("Vui lòng đóng Game đang chạy trước khi khôi phục thế giới.");
    }
    if server_running_for_active_ed {
        return Err("Vui lòng tắt Server đang chạy trước khi khôi phục thế giới.");
    }
    match detected_edition {
        Some(ed) if ed == active_ed => Ok(()),
        Some(GameEdition::Vanilla) => Err(
            "Bản sao lưu thuộc Terraria Vanilla; hãy chuyển sang Terraria để khôi phục.",
        ),
        Some(GameEdition::TModLoader) => Err(
            "Bản sao lưu thuộc tModLoader; hãy chuyển sang tModLoader để khôi phục.",
        ),
        None => Err("Không nhận diện được định dạng thế giới (.wld/.twld) trong bản sao lưu này."),
    }
}

fn is_server_managed_by_direct_host(
    profile_id: &str,
    pending_ports: &HashMap<String, u16>,
    direct_handles: &HashMap<String, DirectPortHostHandle>,
) -> bool {
    pending_ports.contains_key(profile_id)
        || direct_handles.get(profile_id).is_some_and(|h| {
            matches!(
                h.current_status(),
                DirectHostStatus::Running(_) | DirectHostStatus::Starting { .. }
            )
        })
}

fn validate_and_normalize_manual_ban_ip(raw: &str) -> Result<IpAddr, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Nhập địa chỉ IP cần cấm.");
    }
    if is_loopback_ban_target(trimmed) || trimmed.to_ascii_lowercase().starts_with("localhost:") {
        return Err(
            "Không được phép đưa IP nội bộ (127.0.0.1 / localhost / ::1) vào danh sách chặn.",
        );
    }
    let ip_str = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    let parsed = ip_str
        .parse::<IpAddr>()
        .map_err(|_| "Địa chỉ IP không hợp lệ (chỉ nhập IPv4 hoặc IPv6, không kèm cổng).")?;
    let norm = normalize_ip_addr(parsed);
    if norm.is_loopback() || norm.is_unspecified() {
        return Err(
            "Không được phép đưa IP nội bộ (127.0.0.1 / 0.0.0.0 / ::1) vào danh sách chặn.",
        );
    }
    Ok(norm)
}

fn merge_manual_ban_entry(entries: &mut Vec<BanEntry>, label: String, norm_ip: IpAddr) -> bool {
    if entries
        .iter()
        .any(|e| extract_ban_ip_key(&e.ip).map(normalize_ip_addr) == Some(norm_ip))
    {
        return false;
    }
    entries.push(BanEntry {
        label,
        ip: norm_ip.to_string(),
    });
    true
}

fn remove_ban_entry_from_disk_list(disk_entries: &mut Vec<BanEntry>, target: &BanEntry) -> bool {
    let target_norm = extract_ban_ip_key(&target.ip).map(normalize_ip_addr);
    if let Some(pos) = disk_entries.iter().position(|e| {
        (e.label == target.label && e.ip == target.ip)
            || (target_norm.is_some()
                && extract_ban_ip_key(&e.ip).map(normalize_ip_addr) == target_norm)
    }) {
        disk_entries.remove(pos);
        true
    } else {
        false
    }
}

fn next_available_server_port(servers: &[ServerProfile]) -> u16 {
    let used: HashSet<u16> = servers.iter().map(|s| s.config.port).collect();
    (7777..=u16::MAX)
        .find(|p| !used.contains(p))
        .unwrap_or(7777)
}

fn active_direct_host_ports(
    direct_handles: &HashMap<String, DirectPortHostHandle>,
    exclude_profile_id: Option<&str>,
) -> HashSet<u16> {
    direct_handles
        .iter()
        .filter(|(id, _)| exclude_profile_id != Some(id.as_str()))
        .filter_map(|(_, handle)| match handle.current_status() {
            DirectHostStatus::Starting { host_port, .. } if host_port > 0 => Some(host_port),
            DirectHostStatus::Running(info) if info.host_port > 0 => Some(info.host_port),
            _ => None,
        })
        .collect()
}

fn resolve_active_server_port_value(
    sup_active: bool,
    snap_port: Option<u16>,
    showing_external: bool,
    ext_port: Option<u16>,
    cfg_port: Option<u16>,
) -> u16 {
    let port = if sup_active {
        snap_port.filter(|&p| p > 0).or(cfg_port)
    } else if showing_external {
        ext_port.filter(|&p| p > 0).or(cfg_port)
    } else {
        cfg_port.filter(|&p| p > 0).or(ext_port)
    };
    port.filter(|&p| p > 0).unwrap_or(7777)
}

fn sanitize_server_console_arg(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|&c| c != '\r' && c != '\n')
        .collect()
}

fn collect_banlist_ips_from_entries(entries: &[BanEntry]) -> Vec<IpAddr> {
    let mut ips = Vec::new();
    for entry in entries {
        if is_loopback_ban_target(&entry.ip) {
            continue;
        }
        if let Some(ip) = extract_ban_ip_key(&entry.ip) {
            let canonical = normalize_ip_addr(ip);
            if !canonical.is_loopback() && !canonical.is_unspecified() && !ips.contains(&canonical) {
                ips.push(canonical);
            }
        }
    }
    ips
}

fn format_player_ip_with_resolver(
    raw: &str,
    resolve_peer: impl FnOnce(u16) -> Option<IpAddr>,
) -> String {
    let trimmed = raw.trim();
    if let Ok(sock) = trimmed.parse::<SocketAddr>() {
        if sock.ip().is_loopback() {
            if let Some(real_ip) = resolve_peer(sock.port()) {
                return normalize_ip_addr(real_ip).to_string();
            }
        }
        return trimmed.to_string();
    }
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return normalize_ip_addr(ip).to_string();
    }
    raw.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlayerBanResolution {
    RefuseLoopback,
    BridgedRealIp(IpAddr),
    DirectIp(IpAddr),
    UnknownIp,
}

fn resolve_player_ban_target(
    raw: Option<&str>,
    resolve_peer: impl FnOnce(u16) -> Option<IpAddr>,
) -> PlayerBanResolution {
    let Some(trimmed) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return PlayerBanResolution::UnknownIp;
    };
    if let Ok(sock) = trimmed.parse::<SocketAddr>() {
        let ip = normalize_ip_addr(sock.ip());
        if ip.is_loopback() || ip.is_unspecified() {
            return match resolve_peer(sock.port())
                .map(normalize_ip_addr)
                .filter(|r| !r.is_loopback() && !r.is_unspecified())
            {
                Some(real_ip) => PlayerBanResolution::BridgedRealIp(real_ip),
                None => PlayerBanResolution::RefuseLoopback,
            };
        }
        return PlayerBanResolution::DirectIp(ip);
    }
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        let ip = normalize_ip_addr(ip);
        if ip.is_loopback() || ip.is_unspecified() {
            return PlayerBanResolution::RefuseLoopback;
        }
        return PlayerBanResolution::DirectIp(ip);
    }
    if is_loopback_ban_target(trimmed) {
        return PlayerBanResolution::RefuseLoopback;
    }
    PlayerBanResolution::UnknownIp
}

fn resolve_profile_world_path_with_override(
    paths: &LauncherPaths,
    profile: &ServerProfile,
    effective_world_override: Option<&str>,
) -> Option<PathBuf> {
    let default_worlds_dir = match profile.edition {
        GameEdition::Vanilla => paths.vanilla_worlds_dir(),
        GameEdition::TModLoader => paths.tml_worlds_dir(),
    };
    let worlds_dir = {
        let wp = profile.config.worldpath.trim();
        if wp.is_empty() {
            default_worlds_dir
        } else {
            let p = PathBuf::from(wp);
            if p.is_absolute() {
                p
            } else {
                default_worlds_dir.join(p)
            }
        }
    };
    if let Some(raw_override) = effective_world_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let p = PathBuf::from(raw_override);
        return Some(if p.is_absolute() { p } else { worlds_dir.join(p) });
    }
    let raw_world = profile.config.world.trim();
    if !raw_world.is_empty() {
        let p = PathBuf::from(raw_world);
        return Some(if p.is_absolute() { p } else { worlds_dir.join(p) });
    }
    let world_name = profile.config.worldname.trim();
    if world_name.is_empty() {
        return None;
    }
    Some(worlds_dir.join(format!("{world_name}.wld")))
}

fn resolve_profile_world_path(paths: &LauncherPaths, profile: &ServerProfile) -> Option<PathBuf> {
    resolve_profile_world_path_with_override(paths, profile, None)
}

fn normalize_world_path_key(path: &Path) -> String {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut normalized = PathBuf::new();
    for comp in resolved.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    let s = normalized.to_string_lossy().replace('/', "\\");
    let stripped = s.strip_prefix(r"\\?\").unwrap_or(&s);
    #[cfg(windows)]
    {
        stripped.to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        stripped.to_string()
    }
}

fn find_conflicting_world_profile_name(
    paths: &LauncherPaths,
    target: &ServerProfile,
    servers: &[ServerProfile],
    active_supervised_ids: &HashSet<&str>,
    server_snapshots: &HashMap<String, ServerRuntimeSnapshot>,
    external_servers: &[DetectedExternalServer],
) -> Option<String> {
    let target_key =
        resolve_profile_world_path(paths, target).map(|p| normalize_world_path_key(&p))?;

    for other_id in active_supervised_ids {
        if *other_id == target.id {
            continue;
        }
        let snap = server_snapshots.get(*other_id);
        let snap_world = snap
            .map(|s| s.effective_world.trim())
            .filter(|s| !s.is_empty());
        if let Some(other) = servers.iter().find(|p| p.id == *other_id) {
            if let Some(other_key) =
                resolve_profile_world_path_with_override(paths, other, snap_world)
                    .map(|p| normalize_world_path_key(&p))
            {
                if other_key == target_key {
                    return Some(other.name.clone());
                }
            }
        } else if let (Some(s), Some(world_str)) = (snap, snap_world) {
            let p = PathBuf::from(world_str);
            let resolved = if p.is_absolute() {
                p
            } else {
                match s.edition {
                    GameEdition::Vanilla => paths.vanilla_worlds_dir().join(p),
                    GameEdition::TModLoader => paths.tml_worlds_dir().join(p),
                }
            };
            if normalize_world_path_key(&resolved) == target_key {
                return Some(s.profile_name.clone());
            }
        }
    }

    for ext in external_servers {
        let Some(ext_id) = ext.profile_id.as_deref() else {
            continue;
        };
        if ext_id == target.id {
            continue;
        }
        if let Some(other) = servers.iter().find(|p| p.id == ext_id) {
            if let Some(other_key) =
                resolve_profile_world_path(paths, other).map(|p| normalize_world_path_key(&p))
            {
                if other_key == target_key {
                    return Some(other.name.clone());
                }
            }
        }
    }

    None
}

fn generate_unique_profile_world_stem(
    paths: &LauncherPaths,
    worlds_dir: &Path,
    existing_servers: &[ServerProfile],
) -> String {
    let used_keys: HashSet<String> = existing_servers
        .iter()
        .filter_map(|p| resolve_profile_world_path(paths, p))
        .map(|p| normalize_world_path_key(&p))
        .collect();

    let first_candidate = worlds_dir.join("NewWorld.wld");
    if !used_keys.contains(&normalize_world_path_key(&first_candidate)) && !first_candidate.exists()
    {
        return "NewWorld".to_string();
    }

    let mut idx = 2usize;
    loop {
        let stem = format!("ServerWorld_{idx}");
        let cand = worlds_dir.join(format!("{stem}.wld"));
        if !used_keys.contains(&normalize_world_path_key(&cand)) && !cand.exists() {
            return stem;
        }
        idx += 1;
    }
}

fn is_existing_valid_modpack(paths: &LauncherPaths, pack_name: &str) -> bool {
    let trimmed = pack_name.trim();
    if trimmed.is_empty() || !is_safe_single_dir_component(trimmed) {
        return false;
    }
    let modpacks_dir = paths.tml_modpacks_dir();
    let pack_dir = modpacks_dir.join(trimmed);
    pack_dir.join("Mods").is_dir() || modpacks_dir.join(format!("{trimmed}.json")).is_file()
}

fn apply_saved_join_server_to_config(
    paths: &LauncherPaths,
    config: &mut LauncherConfig,
    quick_join_port_input: &mut String,
    idx: usize,
) -> Option<(bool, Option<String>)> {
    let srv = config.saved_join_servers.get(idx).cloned()?;
    config.quick_join_host = srv.host;
    config.quick_join_port = srv.port;
    *quick_join_port_input = srv.port.to_string();
    config.quick_join_password = srv.password;
    let edition_changed = config.active_edition != srv.edition;
    config.active_edition = srv.edition;

    let mut missing_pack = None;
    let pack_trimmed = srv.modpack.trim();
    if pack_trimmed.is_empty() {
        config.active_modpack.clear();
    } else if srv.edition == GameEdition::TModLoader {
        if is_existing_valid_modpack(paths, pack_trimmed) {
            config.active_modpack = pack_trimmed.to_string();
        } else {
            missing_pack = Some(pack_trimmed.to_string());
        }
    }

    Some((edition_changed, missing_pack))
}

#[cfg(test)]
mod tests {
    use super::{
        apply_saved_join_server_to_config, check_install_edition_ready_flags,
        check_restore_world_backup_flags, collect_banlist_ips_from_entries,
        find_conflicting_world_profile_name, format_close_confirm_message,
        format_player_ip_with_resolver, generate_unique_profile_world_stem,
        is_safe_single_dir_component, merge_manual_ban_entry, next_available_server_port,
        remove_ban_entry_from_disk_list, resolve_active_server_port_value,
        resolve_player_ban_target, resolve_profile_world_path, resolve_workshop_install_order,
        sanitize_server_console_arg, should_block_close_transition, split_host_port_input,
        validate_and_normalize_manual_ban_ip, PlayerBanResolution,
    };
    use std::collections::{HashMap, HashSet};
    use std::net::IpAddr;
    use std::path::PathBuf;
    use tl_core::{
        GameEdition, LauncherConfig, LauncherPaths, SavedJoinServer, ServerConfigData,
        ServerProfile,
    };
    use tl_runtime::{BanEntry, DetectedExternalServer, ServerLifecycleState, ServerRuntimeSnapshot};

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

    #[test]
    fn collect_banlist_ips_ignores_loopback_and_normalizes_ipv6_mapped() {
        let entries = vec![
            BanEntry {
                label: "Local".to_string(),
                ip: "127.0.0.1".to_string(),
            },
            BanEntry {
                label: "Local6".to_string(),
                ip: "[::1]:7777".to_string(),
            },
            BanEntry {
                label: "Host".to_string(),
                ip: "localhost".to_string(),
            },
            BanEntry {
                label: "Wildcard".to_string(),
                ip: "0.0.0.0".to_string(),
            },
            BanEntry {
                label: "Mapped".to_string(),
                ip: "::ffff:203.0.113.9".to_string(),
            },
            BanEntry {
                label: "Dup".to_string(),
                ip: "203.0.113.9".to_string(),
            },
            BanEntry {
                label: "V6".to_string(),
                ip: "[2405:4802::1]:12345".to_string(),
            },
        ];
        let ips = collect_banlist_ips_from_entries(&entries);
        assert_eq!(
            ips,
            vec![
                "203.0.113.9".parse::<IpAddr>().unwrap(),
                "2405:4802::1".parse::<IpAddr>().unwrap(),
            ]
        );
    }

    #[test]
    fn format_player_ip_resolves_bridged_loopback_and_preserves_direct() {
        let bridged = format_player_ip_with_resolver("127.0.0.1:60972", |port| {
            assert_eq!(port, 60972);
            Some("::ffff:198.51.100.4".parse().unwrap())
        });
        assert_eq!(bridged, "198.51.100.4");

        let unbridged = format_player_ip_with_resolver("127.0.0.1:50000", |_| None);
        assert_eq!(unbridged, "127.0.0.1:50000");

        let direct = format_player_ip_with_resolver(" 192.168.1.50:7777 ", |_| None);
        assert_eq!(direct, "192.168.1.50:7777");
    }

    #[test]
    fn resolve_player_ban_target_protects_localhost_and_unmasks_bridge() {
        assert_eq!(
            resolve_player_ban_target(Some("127.0.0.1:60972"), |port| {
                assert_eq!(port, 60972);
                Some("::ffff:203.0.113.5".parse().unwrap())
            }),
            PlayerBanResolution::BridgedRealIp("203.0.113.5".parse().unwrap())
        );
        assert_eq!(
            resolve_player_ban_target(Some("127.0.0.1:60972"), |_| None),
            PlayerBanResolution::RefuseLoopback
        );
        assert_eq!(
            resolve_player_ban_target(Some("[::1]:54321"), |_| {
                Some("127.0.0.1".parse().unwrap())
            }),
            PlayerBanResolution::RefuseLoopback
        );
        assert_eq!(
            resolve_player_ban_target(Some("192.168.1.88:5555"), |_| None),
            PlayerBanResolution::DirectIp("192.168.1.88".parse().unwrap())
        );
        assert_eq!(
            resolve_player_ban_target(None, |_| None),
            PlayerBanResolution::UnknownIp
        );
    }

    #[test]
    fn resolve_profile_world_path_handles_relative_and_fallback_worldname() {
        let paths = LauncherPaths::resolve(None, None);
        let mut profile = ServerProfile {
            id: "srv-test".to_string(),
            name: "Test".to_string(),
            edition: GameEdition::TModLoader,
            auto_restart: false,
            extra_args: String::new(),
            config: ServerConfigData {
                world: "Custom.wld".to_string(),
                worldpath: String::new(),
                worldname: "Fallback".to_string(),
                ..ServerConfigData::default()
            },
        };
        assert_eq!(
            resolve_profile_world_path(&paths, &profile),
            Some(paths.tml_worlds_dir().join("Custom.wld"))
        );

        profile.config.world.clear();
        profile.edition = GameEdition::Vanilla;
        assert_eq!(
            resolve_profile_world_path(&paths, &profile),
            Some(paths.vanilla_worlds_dir().join("Fallback.wld"))
        );

        let abs = PathBuf::from(r"D:\Worlds\Main.wld");
        profile.config.world = abs.to_string_lossy().into_owned();
        assert_eq!(resolve_profile_world_path(&paths, &profile), Some(abs));
    }

    #[test]
    fn is_safe_single_dir_component_rejects_traversal_and_special_paths() {
        assert!(is_safe_single_dir_component("default-tml"));
        assert!(is_safe_single_dir_component("srv-1a2b3c"));
        assert!(!is_safe_single_dir_component(""));
        assert!(!is_safe_single_dir_component("."));
        assert!(!is_safe_single_dir_component(".."));
        assert!(!is_safe_single_dir_component("../escape"));
        assert!(!is_safe_single_dir_component("a/b"));
        assert!(!is_safe_single_dir_component(r"a\b"));
        assert!(!is_safe_single_dir_component("C:srv"));
        assert!(!is_safe_single_dir_component(" srv "));
    }

    #[test]
    fn sanitize_server_console_arg_strips_newlines() {
        assert_eq!(
            sanitize_server_console_arg("  Alice\r\nexit "),
            "Aliceexit"
        );
        assert_eq!(sanitize_server_console_arg("Player Two"), "Player Two");
    }

    fn make_test_profile(id: &str, name: &str, world: &str) -> ServerProfile {
        ServerProfile {
            id: id.to_string(),
            name: name.to_string(),
            edition: GameEdition::TModLoader,
            auto_restart: false,
            extra_args: String::new(),
            config: ServerConfigData {
                world: world.to_string(),
                worldpath: String::new(),
                worldname: "NewWorld".to_string(),
                ..ServerConfigData::default()
            },
        }
    }

    #[test]
    fn saved_join_server_preserves_modpack_across_serde() {
        let entry = SavedJoinServer {
            name: "Calamity Co-op".to_string(),
            host: "203.0.113.10".to_string(),
            port: 7777,
            password: "secret".to_string(),
            edition: GameEdition::TModLoader,
            modpack: "CalamityPack".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let roundtrip: SavedJoinServer = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.modpack, "CalamityPack");

        let legacy_json =
            r#"{"name":"Legacy","host":"127.0.0.1","port":7777,"password":"","edition":"TModLoader"}"#;
        let legacy: SavedJoinServer = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(legacy.modpack, "");
    }

    #[test]
    fn load_saved_join_server_to_form_restores_fields_and_modpack() {
        let root = std::env::temp_dir().join(format!(
            "tl_app_state_saved_join_{}_{}",
            std::process::id(),
            super::uuid_short()
        ));
        let mut paths = LauncherPaths::resolve(None, None);
        paths.base_dir = root.clone();
        paths.tmodloader_dir = root.join("tModLoader");
        paths.data_dir = root.join("data");
        paths.tml_saves_dir = paths.data_dir.join("Saves").join("tModLoader");
        paths.vanilla_saves_dir = paths.data_dir.join("Saves").join("Vanilla");
        paths.servers_dir = paths.data_dir.join("Servers");
        std::fs::create_dir_all(paths.tml_modpacks_dir().join("CalamityPack").join("Mods"))
            .unwrap();

        let mut config = LauncherConfig {
            active_edition: GameEdition::Vanilla,
            active_modpack: "OldPack".to_string(),
            saved_join_servers: vec![
                SavedJoinServer {
                    name: "Friends TML".to_string(),
                    host: "play.example.org".to_string(),
                    port: 7788,
                    password: "pw".to_string(),
                    edition: GameEdition::TModLoader,
                    modpack: "CalamityPack".to_string(),
                },
                SavedJoinServer {
                    name: "Missing Pack".to_string(),
                    host: "192.168.1.2".to_string(),
                    port: 7799,
                    password: String::new(),
                    edition: GameEdition::TModLoader,
                    modpack: "DeletedPack".to_string(),
                },
            ],
            ..LauncherConfig::default()
        };

        let mut port_input = "7777".to_string();
        let (ed_changed, missing) =
            apply_saved_join_server_to_config(&paths, &mut config, &mut port_input, 0).unwrap();
        assert!(ed_changed);
        assert!(missing.is_none());
        assert_eq!(config.quick_join_host, "play.example.org");
        assert_eq!(config.quick_join_port, 7788);
        assert_eq!(port_input, "7788");
        assert_eq!(config.quick_join_password, "pw");
        assert_eq!(config.active_edition, GameEdition::TModLoader);
        assert_eq!(config.active_modpack, "CalamityPack");

        let (ed_changed_2, missing_2) =
            apply_saved_join_server_to_config(&paths, &mut config, &mut port_input, 1).unwrap();
        assert!(!ed_changed_2);
        assert_eq!(missing_2.as_deref(), Some("DeletedPack"));
        assert_eq!(config.active_modpack, "CalamityPack");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn active_server_port_prioritizes_external_when_showing_external() {
        assert_eq!(
            resolve_active_server_port_value(false, None, true, Some(7780), Some(7777)),
            7780
        );
        assert_eq!(
            resolve_active_server_port_value(false, None, false, Some(7780), Some(7777)),
            7777
        );
        assert_eq!(
            resolve_active_server_port_value(true, Some(7790), true, Some(7780), Some(7777)),
            7790
        );
    }

    #[test]
    fn find_conflicting_world_profile_name_detects_active_supervised_and_external() {
        let paths = LauncherPaths::resolve(None, None);
        let shared_abs = paths
            .tml_worlds_dir()
            .join("SharedWorld.wld")
            .to_string_lossy()
            .into_owned();
        let p1 = make_test_profile("srv-1", "Main Server", &shared_abs);
        let p2 = make_test_profile("srv-2", "Second Server", "SharedWorld.wld");
        let p3 = make_test_profile("srv-3", "Third Server", "OtherWorld.wld");
        let servers = vec![p1.clone(), p2.clone(), p3.clone()];

        let active_sup = HashSet::from(["srv-1"]);
        assert_eq!(
            find_conflicting_world_profile_name(
                &paths,
                &p2,
                &servers,
                &active_sup,
                &HashMap::new(),
                &[],
            ),
            Some("Main Server".to_string())
        );
        assert_eq!(
            find_conflicting_world_profile_name(
                &paths,
                &p3,
                &servers,
                &active_sup,
                &HashMap::new(),
                &[],
            ),
            None
        );

        let ext = DetectedExternalServer {
            pid: 9999,
            edition: GameEdition::TModLoader,
            port: Some(7778),
            profile_id: Some("srv-2".to_string()),
            memory_mb: 100,
            cpu_percent: 1.0,
        };
        assert_eq!(
            find_conflicting_world_profile_name(
                &paths,
                &p1,
                &servers,
                &HashSet::new(),
                &HashMap::new(),
                &[ext],
            ),
            Some("Second Server".to_string())
        );
    }

    #[test]
    fn generate_unique_profile_world_stem_avoids_existing_profiles_and_disk_files() {
        let root = std::env::temp_dir().join(format!(
            "tl_app_state_world_stem_{}_{}",
            std::process::id(),
            super::uuid_short()
        ));
        let mut paths = LauncherPaths::resolve(None, None);
        paths.base_dir = root.clone();
        paths.data_dir = root.join("data");
        paths.vanilla_saves_dir = paths.data_dir.join("Saves").join("Vanilla");
        paths.tml_saves_dir = paths.data_dir.join("Saves").join("tModLoader");
        paths.servers_dir = paths.data_dir.join("Servers");
        paths.tmodloader_dir = root.join("tModLoader");
        let worlds_dir = paths.tml_worlds_dir();
        std::fs::create_dir_all(&worlds_dir).unwrap();

        assert_eq!(
            generate_unique_profile_world_stem(&paths, &worlds_dir, &[]),
            "NewWorld"
        );

        let existing = vec![make_test_profile("srv-1", "P1", "NewWorld.wld")];
        assert_eq!(
            generate_unique_profile_world_stem(&paths, &worlds_dir, &existing),
            "ServerWorld_2"
        );

        std::fs::write(worlds_dir.join("ServerWorld_2.wld"), b"").unwrap();
        assert_eq!(
            generate_unique_profile_world_stem(&paths, &worlds_dir, &existing),
            "ServerWorld_3"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn should_block_close_transition_and_messages_cover_all_hosted_resources() {
        let mut modal_open = false;
        assert_eq!(
            should_block_close_transition(false, false, &mut modal_open),
            (false, false)
        );
        assert!(!modal_open);

        assert_eq!(
            should_block_close_transition(false, true, &mut modal_open),
            (true, true)
        );
        assert!(modal_open);

        assert_eq!(
            should_block_close_transition(false, true, &mut modal_open),
            (true, false)
        );
        assert!(modal_open);

        assert_eq!(
            should_block_close_transition(true, true, &mut modal_open),
            (false, false)
        );

        let msg_server = format_close_confirm_message(true, false, false, false);
        assert!(msg_server.contains("Server"));
        let msg_all = format_close_confirm_message(true, true, true, true);
        assert!(msg_all.contains("Server"));
        assert!(msg_all.contains("UPnP"));
        assert!(msg_all.contains("IPv6"));
        let msg_upnp = format_close_confirm_message(false, true, true, false);
        assert!(msg_upnp.contains("UPnP"));
        let msg_direct = format_close_confirm_message(false, true, false, false);
        assert!(msg_direct.contains("Port"));
        let msg_bridge = format_close_confirm_message(false, false, false, true);
        assert!(msg_bridge.contains("IPv6"));
    }

    #[test]
    fn apply_saved_join_server_clears_modpack_when_entry_is_blank() {
        let paths = LauncherPaths::resolve(None, None);
        let mut config = LauncherConfig {
            active_edition: GameEdition::TModLoader,
            active_modpack: "OldPack".to_string(),
            saved_join_servers: vec![
                SavedJoinServer {
                    name: "Blank TML".to_string(),
                    host: "play.example.org".to_string(),
                    port: 7777,
                    password: String::new(),
                    edition: GameEdition::TModLoader,
                    modpack: "   ".to_string(),
                },
                SavedJoinServer {
                    name: "Blank Vanilla".to_string(),
                    host: "198.51.100.7".to_string(),
                    port: 7780,
                    password: String::new(),
                    edition: GameEdition::Vanilla,
                    modpack: String::new(),
                },
            ],
            ..LauncherConfig::default()
        };

        let mut port_input = String::new();
        let (ed_changed_1, missing_1) =
            apply_saved_join_server_to_config(&paths, &mut config, &mut port_input, 0).unwrap();
        assert!(!ed_changed_1);
        assert!(missing_1.is_none());
        assert!(config.active_modpack.is_empty());

        config.active_modpack = "OldPack".to_string();
        let (ed_changed_2, missing_2) =
            apply_saved_join_server_to_config(&paths, &mut config, &mut port_input, 1).unwrap();
        assert!(ed_changed_2);
        assert!(missing_2.is_none());
        assert!(config.active_modpack.is_empty());
        assert_eq!(config.active_edition, GameEdition::Vanilla);
    }

    #[test]
    fn find_conflicting_world_profile_name_checks_active_effective_world_snapshot() {
        let paths = LauncherPaths::resolve(None, None);
        let p1 = make_test_profile("srv-1", "Main Server", "WorldA.wld");
        let p2 = make_test_profile("srv-2", "Second Server", "WorldB.wld");
        let p3 = make_test_profile("srv-3", "Third Server", "WorldA.wld");
        let servers = vec![p1.clone(), p2.clone(), p3.clone()];

        let active_sup = HashSet::from(["srv-1"]);
        let mut snaps = HashMap::new();
        snaps.insert(
            "srv-1".to_string(),
            ServerRuntimeSnapshot {
                profile_id: "srv-1".to_string(),
                profile_name: "Main Server".to_string(),
                edition: GameEdition::TModLoader,
                port: 7777,
                effective_world: paths
                    .tml_worlds_dir()
                    .join("WorldB.wld")
                    .to_string_lossy()
                    .into_owned(),
                effective_password: String::new(),
                effective_modpack: String::new(),
                state: ServerLifecycleState::Running,
                status_detail: "Running".to_string(),
                pid: Some(1234),
                memory_mb: 256,
                cpu_percent: 2.0,
                players: Vec::new(),
            },
        );

        assert_eq!(
            find_conflicting_world_profile_name(&paths, &p2, &servers, &active_sup, &snaps, &[]),
            Some("Main Server".to_string())
        );
        assert_eq!(
            find_conflicting_world_profile_name(&paths, &p3, &servers, &active_sup, &snaps, &[]),
            None
        );
    }

    #[test]
    fn next_available_server_port_picks_lowest_unused_from_7777() {
        assert_eq!(next_available_server_port(&[]), 7777);

        let servers_with_gap = vec![
            make_test_profile("srv-1", "S1", "W1.wld"),
            ServerProfile {
                config: ServerConfigData {
                    port: 7779,
                    ..ServerConfigData::default()
                },
                ..make_test_profile("srv-2", "S2", "W2.wld")
            },
        ];
        assert_eq!(next_available_server_port(&servers_with_gap), 7778);
    }

    #[test]
    fn resolve_workshop_install_order_handles_transitive_diamonds_cycles_and_installed_deps() {
        let mut adj = HashMap::new();
        adj.insert("100".to_string(), vec!["200".to_string(), "300".to_string()]);
        adj.insert(
            "200".to_string(),
            vec!["400".to_string(), "500".to_string(), "100".to_string()],
        );
        adj.insert("300".to_string(), vec!["400".to_string()]);
        adj.insert("400".to_string(), Vec::new());

        let installed = HashSet::from(["500".to_string(), "100".to_string()]);
        let order = resolve_workshop_install_order("100", &adj, &installed);

        assert!(!order.contains(&"500".to_string()));
        assert_eq!(order.last().map(String::as_str), Some("100"));
        assert_eq!(order.iter().filter(|id| id.as_str() == "400").count(), 1);
        let pos_400 = order.iter().position(|id| id == "400").unwrap();
        let pos_200 = order.iter().position(|id| id == "200").unwrap();
        let pos_300 = order.iter().position(|id| id == "300").unwrap();
        let pos_100 = order.iter().position(|id| id == "100").unwrap();
        assert!(pos_400 < pos_200 && pos_400 < pos_300);
        assert!(pos_200 < pos_100 && pos_300 < pos_100);
    }

    #[test]
    fn install_and_restore_gates_enforce_cross_edition_and_unknown_format_rules() {
        assert!(check_install_edition_ready_flags(
            true,
            false,
            None,
            false,
            false,
            Some(GameEdition::TModLoader),
        )
        .is_err());
        assert!(check_install_edition_ready_flags(
            false,
            true,
            None,
            false,
            false,
            Some(GameEdition::Vanilla),
        )
        .is_err());
        assert!(check_install_edition_ready_flags(
            false,
            false,
            Some(GameEdition::TModLoader),
            false,
            false,
            Some(GameEdition::Vanilla),
        )
        .is_err());
        assert!(check_install_edition_ready_flags(
            false,
            false,
            None,
            false,
            true,
            Some(GameEdition::Vanilla),
        )
        .is_err());
        assert!(check_install_edition_ready_flags(
            false,
            false,
            Some(GameEdition::Vanilla),
            true,
            false,
            Some(GameEdition::TModLoader),
        )
        .is_ok());
        assert!(check_install_edition_ready_flags(
            false,
            false,
            Some(GameEdition::Vanilla),
            false,
            false,
            None,
        )
        .is_err());
        assert!(check_install_edition_ready_flags(
            false,
            false,
            Some(GameEdition::TModLoader),
            false,
            true,
            None,
        )
        .is_ok());

        assert!(check_restore_world_backup_flags(
            true,
            GameEdition::TModLoader,
            None,
            false,
            Some(GameEdition::TModLoader),
        )
        .is_err());
        assert!(check_restore_world_backup_flags(
            false,
            GameEdition::TModLoader,
            Some(GameEdition::TModLoader),
            false,
            Some(GameEdition::TModLoader),
        )
        .is_err());
        assert!(check_restore_world_backup_flags(
            false,
            GameEdition::TModLoader,
            None,
            true,
            Some(GameEdition::TModLoader),
        )
        .is_err());
        assert!(check_restore_world_backup_flags(
            false,
            GameEdition::TModLoader,
            None,
            false,
            None,
        )
        .is_err());
        assert!(check_restore_world_backup_flags(
            false,
            GameEdition::TModLoader,
            None,
            false,
            Some(GameEdition::Vanilla),
        )
        .is_err());
        assert!(check_restore_world_backup_flags(
            false,
            GameEdition::TModLoader,
            None,
            false,
            Some(GameEdition::TModLoader),
        )
        .is_ok());
    }

    #[test]
    fn manual_ban_validation_dedup_and_disk_merge_preserve_concurrent_entries() {
        assert!(validate_and_normalize_manual_ban_ip("").is_err());
        assert!(validate_and_normalize_manual_ban_ip("127.0.0.1").is_err());
        assert!(validate_and_normalize_manual_ban_ip("localhost:7777").is_err());
        assert!(validate_and_normalize_manual_ban_ip("[::1]").is_err());
        assert!(validate_and_normalize_manual_ban_ip("0.0.0.0").is_err());
        assert!(validate_and_normalize_manual_ban_ip("203.0.113.10:7777").is_err());

        let v4 = validate_and_normalize_manual_ban_ip("::ffff:203.0.113.10").unwrap();
        assert_eq!(v4, "203.0.113.10".parse::<IpAddr>().unwrap());
        let v6 = validate_and_normalize_manual_ban_ip("[2405:4802::1]").unwrap();
        assert_eq!(v6, "2405:4802::1".parse::<IpAddr>().unwrap());

        let mut disk = vec![BanEntry {
            label: "ConcurrentBan".to_string(),
            ip: "198.51.100.2".to_string(),
        }];
        assert!(merge_manual_ban_entry(&mut disk, "Manual".to_string(), v4));
        assert!(!merge_manual_ban_entry(&mut disk, "Duplicate".to_string(), v4));
        assert_eq!(disk.len(), 2);

        let target = BanEntry {
            label: "Manual".to_string(),
            ip: "::ffff:203.0.113.10".to_string(),
        };
        assert!(remove_ban_entry_from_disk_list(&mut disk, &target));
        assert_eq!(disk.len(), 1);
        assert_eq!(disk[0].label, "ConcurrentBan");
    }
}
