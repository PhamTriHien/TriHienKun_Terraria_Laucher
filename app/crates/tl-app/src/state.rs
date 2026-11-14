use crate::theme::ACCENT_EMERALD;
use egui::{ColorImage, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use sysinfo::System;
use tl_core::{
    activate_modpack, backup_world, create_modpack, delete_modpack, detect_environment,
    list_modpacks, list_world_backups, parse_world_file, restore_world_backup, EnvironmentStatus,
    GameEdition, LauncherConfig, LauncherPaths, ModPackInfo, SavedJoinServer, ServerConfigData,
    ServerProfile, TmodMetadata, WorldBackupInfo, WorldMetadata,
};
use tl_runtime::{
    browse_workshop_mods, check_firewall_rule_exists, close_upnp_port, delete_installed_mod,
    delete_staged_workshop_mod, download_and_install_tmod_url, download_and_install_xna,
    download_workshop_mod_via_steamcmd, ensure_firewall_rule, fetch_github_latest_tmod,
    fetch_tmodloader_releases, fetch_workshop_details, fetch_workshop_image_bytes,
    import_local_mod_files, install_dotnet8_portable, install_steamcmd_via_winget,
    install_terraria_from_local_zip, install_terraria_from_source, install_tmodloader_release,
    install_xna_framework_elevated, launch_game, load_banlist, load_workshop_manifest,
    open_upnp_port, parse_workshop_id, probe_network_snapshot, probe_tcp_endpoint,
    record_workshop_install, remove_firewall_rule, save_banlist, scan_installed_mods,
    scan_staged_workshop_mods, set_mod_enabled, start_bore_tunnel, start_server, ActiveGameProcess,
    BanEntry, BoreTunnelHandle, GameLaunchOptions, InstallProgress, InstallTarget,
    InstalledModEntry, NetworkSnapshot, ProgressCallback, RemoteTmodAsset, RepaintHandle,
    RunningGameSnapshot, ServerLifecycleState, ServerRuntimeSnapshot, ServerSupervisor,
    StagedWorkshopMod, TmodLoaderReleaseInfo, TunnelStatus, UpnpLeaseInfo, WorkshopItemDetails,
    WorkshopManifestRecord, WorkshopQueryResult, WorkshopSort,
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavTab {
    Play,
    Server,
    Mods,
    Host,
    Settings,
}

impl NavTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Play => "Chơi Game",
            Self::Server => "Server Game",
            Self::Mods => "Cài Mod",
            Self::Host => "Host Online",
            Self::Settings => "Cài đặt",
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
    UpnpOpened(Result<UpnpLeaseInfo, String>),
    UpnpClosed(Result<(), String>),
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
    InstallProgress(InstallProgress),
    InstallFinished {
        target: InstallTarget,
        result: Result<String, String>,
    },
}

pub struct AppState {
    pub rt: tokio::runtime::Runtime,
    pub repaint: RepaintHandle,
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    pub event_rx: mpsc::UnboundedReceiver<AppEvent>,

    pub paths: LauncherPaths,
    pub config: LauncherConfig,
    pub env: EnvironmentStatus,
    pub sys: Option<System>,

    pub active_tab: NavTab,
    pub mods_tab: ModsSubTab,
    pub notification: Option<AppNotification>,
    pub last_poll_at: Instant,

    pub active_game: Option<ActiveGameProcess>,
    pub running_game: Option<RunningGameSnapshot>,

    pub server_supervisor: Option<ServerSupervisor>,
    pub server_snapshot: Option<ServerRuntimeSnapshot>,
    pub console_input: String,
    pub console_autoscroll: bool,
    pub banlist_entries: Vec<BanEntry>,
    pub manual_ban_name: String,
    pub manual_ban_ip: String,

    pub bore_handle: Option<BoreTunnelHandle>,
    pub upnp_lease: Option<UpnpLeaseInfo>,
    pub network_snapshot: NetworkSnapshot,
    pub network_loading: bool,
    pub port_probe_host: String,
    pub port_probe_port: String,
    pub port_probe_status: Option<(bool, String)>,
    pub port_probe_busy: bool,

    pub installed_mods: Vec<InstalledModEntry>,
    pub mod_icons: HashMap<String, TextureHandle>,
    pub modpacks: Vec<ModPackInfo>,
    pub new_modpack_name: String,
    pub mod_search: String,

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

    pub tml_releases: Vec<TmodLoaderReleaseInfo>,
    pub selected_tml_tag: String,
    pub tml_releases_busy: bool,
    pub install_busy: bool,
    pub install_progress: Option<InstallProgress>,
    pub install_last_error: Option<String>,

    pub worlds: Vec<WorldMetadata>,
    pub world_backups: Vec<WorldBackupInfo>,

    pub new_profile_name: String,
    pub new_profile_edition: GameEdition,
    pub new_join_name: String,
    pub new_join_host: String,
    pub new_join_port: String,
    pub new_join_password: String,
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
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let repaint_ctx = ctx.clone();
        let repaint = RepaintHandle::new(Arc::new(move || {
            repaint_ctx.request_repaint();
        }));

        let default_port = config
            .active_server()
            .map(|s| s.config.port)
            .unwrap_or(7777);
        let last_saved_proxy = config.proxy_url.trim().to_string();

        let mut state = Self {
            rt,
            repaint,
            event_tx,
            event_rx,
            paths,
            config,
            env,
            sys: System::new().ok(),
            active_tab: NavTab::Play,
            mods_tab: ModsSubTab::Installed,
            notification: None,
            last_poll_at: Instant::now(),
            active_game: None,
            running_game: None,
            server_supervisor: None,
            server_snapshot: None,
            console_input: String::new(),
            console_autoscroll: true,
            banlist_entries: Vec::new(),
            manual_ban_name: String::new(),
            manual_ban_ip: String::new(),
            bore_handle: None,
            upnp_lease: None,
            network_snapshot: NetworkSnapshot::default(),
            network_loading: false,
            port_probe_host: String::new(),
            port_probe_port: default_port.to_string(),
            port_probe_status: None,
            port_probe_busy: false,
            installed_mods: Vec::new(),
            mod_icons: HashMap::new(),
            modpacks: Vec::new(),
            new_modpack_name: String::new(),
            mod_search: String::new(),
            github_repo_input: "JavidPack/RecipeBrowser".to_string(),
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
            tml_releases: Vec::new(),
            selected_tml_tag: String::new(),
            tml_releases_busy: false,
            install_busy: false,
            install_progress: None,
            install_last_error: None,
            worlds: Vec::new(),
            world_backups: Vec::new(),
            new_profile_name: String::new(),
            new_profile_edition: GameEdition::TModLoader,
            new_join_name: String::new(),
            new_join_host: String::new(),
            new_join_port: "7777".to_string(),
            new_join_password: String::new(),
        };

        state.refresh_worlds_and_backups();
        state.refresh_mods_and_packs();
        state.refresh_banlist();
        state.spawn_network_probe();
        state
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
        self.refresh_worlds_and_backups();
        self.refresh_mods_and_packs();
        self.refresh_banlist();
    }

    pub fn refresh_worlds_and_backups(&mut self) {
        let worlds_dir = match self.config.active_edition {
            GameEdition::Vanilla => self.paths.vanilla_worlds_dir(),
            GameEdition::TModLoader => self.paths.tml_worlds_dir(),
        };
        let mut parsed = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&worlds_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("wld"))
                    .unwrap_or(false)
                {
                    if let Ok(meta) = parse_world_file(&path) {
                        parsed.push(meta);
                    }
                }
            }
        }
        parsed.sort_by_key(|a| a.world_name.to_lowercase());
        self.worlds = parsed;
        self.world_backups = list_world_backups(&self.paths.backups_dir).unwrap_or_default();
    }

    pub fn refresh_mods_and_packs(&mut self) {
        match scan_installed_mods(&self.paths) {
            Ok(list) => self.installed_mods = list,
            Err(e) => self.notify_err(format!("Không quét được thư mục Mods: {e}")),
        }
        self.modpacks = list_modpacks(&self.paths.tml_modpacks_dir()).unwrap_or_default();
        self.workshop_records = load_workshop_manifest(&self.paths);
        self.staged_workshop_mods = scan_staged_workshop_mods(&self.paths).unwrap_or_default();
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
                        if let Some(pub_ip) = snap.public_ip {
                            self.port_probe_host = pub_ip.to_string();
                        }
                    }
                    self.network_snapshot = snap;
                }
                AppEvent::UpnpOpened(res) => match res {
                    Ok(lease) => {
                        let warn = if lease.is_cgnat {
                            " (Lưu ý: Router báo IP WAN thuộc dải CGNAT — hãy bật Tunnel cho bạn bè kết nối)"
                        } else {
                            ""
                        };
                        self.notify_ok(format!("Đã mở UPnP cổng {}{warn}", lease.port));
                        self.upnp_lease = Some(lease);
                    }
                    Err(e) => {
                        self.notify_err(e);
                    }
                },
                AppEvent::UpnpClosed(res) => {
                    self.upnp_lease = None;
                    match res {
                        Ok(()) => self.notify_ok("Đã đóng cổng UPnP trên router."),
                        Err(e) => self.notify_err(e),
                    }
                }
                AppEvent::FirewallResult(res) => {
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
                        Ok(ms) => (true, format!("{endpoint} phản hồi ({ms} ms)")),
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
                    if self.workshop_downloading_id.is_none() {
                        self.workshop_busy = false;
                    }
                    match res {
                        Ok(details) => self.workshop_details = Some(details),
                        Err(e) => self.notify_err(e),
                    }
                }
                AppEvent::WorkshopLog(line) => {
                    if self.workshop_logs.len() >= 250 {
                        self.workshop_logs.remove(0);
                    }
                    self.workshop_logs.push(line);
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
                    self.refresh_environment();
                    match res {
                        Ok(msg) => self.notify_ok(msg),
                        Err(e) => self.notify_err(e),
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
                AppEvent::InstallProgress(prog) => {
                    self.install_progress = Some(prog);
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
                    let msg = format!(
                        "{} đã đóng (mã thoát {:?}).",
                        proc_handle.edition.label(),
                        exit.code()
                    );
                    self.active_game = None;
                    self.running_game = None;
                    self.notify_ok(msg);
                    self.refresh_worlds_and_backups();
                } else {
                    self.running_game = Some(proc_handle.snapshot());
                }
            }

            if let Some(sup) = &self.server_supervisor {
                let prev_player_count = self
                    .server_snapshot
                    .as_ref()
                    .map(|s| s.players.len())
                    .unwrap_or(0);
                let prev_state = self
                    .server_snapshot
                    .as_ref()
                    .map(|s| s.state.clone());
                let snap = match self.sys.as_mut() {
                    Some(sys) => sup.snapshot(sys),
                    None => {
                        if let Ok(mut fallback) = System::new() {
                            sup.snapshot(&mut fallback)
                        } else {
                            return;
                        }
                    }
                };
                let curr_player_count = snap.players.len();
                let just_stopped = matches!(
                    snap.state,
                    ServerLifecycleState::Stopped | ServerLifecycleState::Crashed(_)
                ) && !matches!(
                    prev_state,
                    Some(ServerLifecycleState::Stopped | ServerLifecycleState::Crashed(_))
                );
                if curr_player_count != prev_player_count || just_stopped {
                    self.refresh_banlist();
                    if just_stopped {
                        self.refresh_worlds_and_backups();
                    }
                }
                self.server_snapshot = Some(snap);
            }
        }

        if self.active_game.is_some()
            || self
                .server_supervisor
                .as_ref()
                .map(|s| s.is_active())
                .unwrap_or(false)
            || self.bore_handle.is_some()
            || self.install_busy
            || self.tml_releases_busy
        {
            ctx.request_repaint_after(Duration::from_millis(800));
        }
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
        self.server_snapshot
            .as_ref()
            .map(|s| s.port)
            .or_else(|| self.config.active_server().map(|p| p.config.port))
            .unwrap_or(7777)
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

    pub fn launch_selected_game(&mut self, join_server: Option<(String, u16, String)>) {
        if self.active_game.is_some() {
            self.notify_err("Game đang chạy. Hãy đóng phiên hiện tại trước khi mở mới.");
            return;
        }
        self.save_config();

        let (join_host, join_port, join_password) = join_server.unwrap_or_else(|| {
            (
                self.config.quick_join_host.trim().to_string(),
                self.config.quick_join_port,
                self.config.quick_join_password.clone(),
            )
        });

        let opts = GameLaunchOptions {
            edition: self.config.active_edition,
            modpack: self.config.active_modpack.clone(),
            join_host,
            join_port,
            join_password,
            extra_args: String::new(),
        };

        match launch_game(&self.paths, &opts) {
            Ok(mut proc_handle) => {
                self.running_game = Some(proc_handle.snapshot());
                self.active_game = Some(proc_handle);
                self.notify_ok(format!("Đã khởi chạy {}.", self.config.active_edition.label()));
            }
            Err(e) => self.notify_err(e.to_string()),
        }
    }

    pub fn kill_running_game(&mut self) {
        if let Some(mut proc_handle) = self.active_game.take() {
            let _ = proc_handle.kill();
            self.running_game = None;
            self.notify_ok("Đã đóng tiến trình game.");
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
            if self.active_game.is_some() {
                self.notify_err("Vui lòng đóng game trước khi cài đặt/cập nhật.");
                return false;
            }
            let server_active = self
                .server_supervisor
                .as_ref()
                .is_some_and(|s| s.is_active());
            if server_active {
                let running_edition = self
                    .server_snapshot
                    .as_ref()
                    .map(|s| s.edition)
                    .or_else(|| self.config.active_server().map(|p| p.edition));
                if running_edition == Some(edition) {
                    self.notify_err("Vui lòng đóng game trước khi cài đặt/cập nhật.");
                    return false;
                }
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

    pub fn install_terraria_url(&mut self) {
        if !self.can_start_install(Some(GameEdition::Vanilla)) {
            return;
        }
        self.save_config();
        let source = self.config.terraria_install_source.trim().to_string();
        if source.is_empty() {
            let msg = "Vui lòng nhập URL .zip hoặc GitHub owner/repo[@tag] trước khi tải Terraria.";
            self.install_last_error = Some(msg.to_string());
            self.notify_err(msg);
            return;
        }
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
        if self
            .server_supervisor
            .as_ref()
            .map(|s| s.is_active())
            .unwrap_or(false)
        {
            self.notify_err("Server đang chạy.");
            return;
        }
        self.save_config();
        let Some(profile) = self.config.active_server().cloned() else {
            self.notify_err("Chưa có cấu hình server nào.");
            return;
        };

        if self.config.auto_firewall {
            let port = profile.config.port;
            if !check_firewall_rule_exists(port) {
                self.ensure_firewall(port);
            }
        }
        if self.config.auto_upnp && profile.config.upnp && self.upnp_lease.is_none() {
            self.open_upnp(profile.config.port);
        }

        let handle = self.rt.handle().clone();
        match start_server(&handle, self.paths.clone(), profile, self.repaint.clone()) {
            Ok(sup) => {
                self.server_snapshot = self
                    .sys
                    .as_mut()
                    .map(|sys| sup.snapshot(sys));
                self.server_supervisor = Some(sup);
                self.notify_ok("Đang khởi chạy server...");
            }
            Err(e) => self.notify_err(e.to_string()),
        }
    }

    pub fn stop_selected_server(&mut self) {
        if let Some(sup) = &self.server_supervisor {
            sup.request_graceful_stop();
            self.notify_ok("Đã gửi lệnh save & exit cho server.");
        }
    }

    pub fn force_kill_server(&mut self) {
        if let Some(sup) = &self.server_supervisor {
            sup.force_kill();
            self.notify_ok("Đã buộc dừng tiến trình server.");
        }
    }

    pub fn send_server_command(&mut self, cmd: &str) {
        if let Some(sup) = &self.server_supervisor {
            if let Err(e) = sup.send_command(cmd) {
                self.notify_err(e.to_string());
            }
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
            return;
        }
        self.network_loading = true;
        let port = self.active_server_port();
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let snap = probe_network_snapshot(port).await;
            let _ = tx.send(AppEvent::NetworkProbed(snap));
            wake.wake();
        });
    }

    pub fn ensure_firewall(&mut self, port: u16) {
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn_blocking(move || {
            let res = ensure_firewall_rule(port).map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::FirewallResult(res));
            wake.wake();
        });
    }

    pub fn remove_firewall(&mut self, port: u16) {
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn_blocking(move || {
            let res = remove_firewall_rule(port).map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::FirewallResult(res));
            wake.wake();
        });
    }

    pub fn open_upnp(&mut self, port: u16) {
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let res = open_upnp_port(port).await.map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::UpnpOpened(res));
            wake.wake();
        });
    }

    pub fn close_upnp(&mut self) {
        let Some(lease) = self.upnp_lease.clone() else {
            return;
        };
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let res = close_upnp_port(&lease).await.map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::UpnpClosed(res));
            wake.wake();
        });
    }

    pub fn toggle_bore_tunnel(&mut self) {
        if let Some(handle) = self.bore_handle.take() {
            handle.stop();
            self.notify_ok("Đang đóng Bore Tunnel...");
            return;
        }
        let port = self.active_server_port();
        let bore_server = self.config.bore_server.clone();
        let secret = non_empty_opt(&self.config.bore_secret).map(ToString::to_string);
        let handle = start_bore_tunnel(
            self.rt.handle(),
            port,
            bore_server,
            secret,
            self.repaint.clone(),
        );
        self.bore_handle = Some(handle);
        self.notify_ok("Đang kết nối Tunnel công khai...");
    }

    pub fn active_tunnel_endpoint(&self) -> Option<String> {
        self.bore_handle
            .as_ref()
            .and_then(|h| h.current_status().endpoint())
    }

    pub fn probe_port(&mut self) {
        if self.port_probe_busy {
            return;
        }
        let host = self.port_probe_host.trim().to_string();
        let Ok(port) = self.port_probe_port.trim().parse::<u16>() else {
            self.notify_err("Cổng kiểm tra không hợp lệ.");
            return;
        };
        let endpoint = format!("{host}:{port}");
        self.port_probe_busy = true;
        self.port_probe_status = None;
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let result = probe_tcp_endpoint(&host, port).await;
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
        let enabled: Vec<String> = self
            .installed_mods
            .iter()
            .filter(|m| m.enabled)
            .map(|m| m.meta.internal_name.clone())
            .collect();
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
                self.new_modpack_name.clear();
                self.config.active_modpack = pack.name.clone();
                self.save_config();
                self.refresh_mods_and_packs();
                self.notify_ok(format!("Đã tạo ModPack '{}'.", pack.name));
            }
            Err(e) => self.notify_err(e.to_string()),
        }
    }

    pub fn activate_selected_modpack(&mut self, pack: &ModPackInfo) {
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

    pub fn download_mod_url(&mut self, url: &str, suggested_name: Option<String>) {
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
            let res = download_and_install_tmod_url(
                &url_owned,
                suggested_name.as_deref(),
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

    pub fn lookup_workshop(&mut self) {
        let Some(id) = parse_workshop_id(&self.workshop_input) else {
            self.notify_err("Nhập Workshop ID hoặc link Steam Workshop hợp lệ.");
            return;
        };
        self.inspect_workshop_id(&id);
    }

    pub fn inspect_workshop_id(&mut self, id: &str) {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.workshop_busy && self.workshop_downloading_id.is_none() {
            return;
        }

        self.workshop_details = self
            .workshop_results
            .iter()
            .find(|item| item.published_file_id == trimmed)
            .cloned();

        if self.workshop_downloading_id.is_none() {
            self.workshop_busy = true;
        }
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

    pub fn install_staged_workshop_mod(&mut self, staged: &StagedWorkshopMod) {
        let sources = [staged.meta.file_path.clone()];
        match import_local_mod_files(&sources, &self.paths, true) {
            Ok(imported) => {
                if imported.is_empty() {
                    self.notify_err("Không tìm thấy file .tmod hợp lệ trong thư mục staging.");
                    return;
                }
                let details = self.resolve_workshop_details_for_staged(staged);
                if let Err(e) = record_workshop_install(
                    &self.paths,
                    details.as_ref(),
                    &staged.workshop_id,
                    &imported,
                ) {
                    self.notify_err(format!("Đã cài mod nhưng lỗi lưu thông tin Workshop: {e}"));
                } else {
                    self.notify_ok(format!(
                        "Đã cài và bật {} từ bộ nhớ tạm Workshop.",
                        staged.meta.display_name
                    ));
                }
                self.refresh_mods_and_packs();
            }
            Err(e) => self.notify_err(format!("Lỗi cài mod từ staging: {e}")),
        }
    }

    pub fn remove_staged_workshop_mod(&mut self, workshop_id: &str) {
        match delete_staged_workshop_mod(&self.paths, workshop_id) {
            Ok(()) => {
                self.refresh_mods_and_packs();
                self.notify_ok(format!("Đã xóa dữ liệu staging của ID {workshop_id}."));
            }
            Err(e) => self.notify_err(format!("Không xóa được staging: {e}")),
        }
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

    fn resolve_workshop_details_for_staged(
        &self,
        staged: &StagedWorkshopMod,
    ) -> Option<WorkshopItemDetails> {
        self.workshop_details
            .as_ref()
            .filter(|d| d.published_file_id == staged.workshop_id)
            .cloned()
            .or_else(|| {
                self.workshop_results
                    .iter()
                    .find(|d| d.published_file_id == staged.workshop_id)
                    .cloned()
            })
            .or_else(|| {
                let record = self
                    .workshop_records
                    .iter()
                    .rev()
                    .find(|r| r.published_file_id == staged.workshop_id);
                let title = staged
                    .workshop_title
                    .clone()
                    .or_else(|| record.map(|r| r.workshop_title.clone()))
                    .filter(|s| !s.is_empty());
                let author = staged
                    .author
                    .clone()
                    .or_else(|| record.map(|r| r.author.clone()))
                    .filter(|s| !s.is_empty());
                let preview_url = staged
                    .preview_url
                    .clone()
                    .or_else(|| record.map(|r| r.preview_url.clone()))
                    .filter(|s| !s.is_empty());
                let time_updated = staged
                    .time_updated
                    .or_else(|| record.map(|r| r.time_updated));

                if title.is_some() || author.is_some() || preview_url.is_some() || time_updated.is_some() {
                    Some(WorkshopItemDetails {
                        published_file_id: staged.workshop_id.clone(),
                        title: title.unwrap_or_else(|| staged.meta.display_name.clone()),
                        author: author.unwrap_or_else(|| staged.meta.author.clone()),
                        description: staged.meta.description.clone(),
                        preview_url: preview_url.unwrap_or_default(),
                        file_size: staged.meta.file_size,
                        time_updated: time_updated.unwrap_or(0),
                        subscriptions: 0,
                        favorited: 0,
                        views: 0,
                        tags: Vec::new(),
                        child_ids: Vec::new(),
                        consumer_app_id: 1281930,
                    })
                } else {
                    None
                }
            })
    }

    pub fn install_steamcmd(&mut self) {
        if self.steamcmd_busy {
            return;
        }
        self.steamcmd_busy = true;
        let tx = self.event_tx.clone();
        let wake = self.repaint.clone();
        self.rt.spawn(async move {
            let res = install_steamcmd_via_winget()
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
        self.config.active_server_id = id;
        self.new_profile_name.clear();
        self.save_config();
        self.refresh_banlist();
        self.notify_ok("Đã thêm cấu hình server mới.");
    }

    pub fn delete_active_server_profile(&mut self) {
        if self.config.servers.len() <= 1 {
            self.notify_err("Cần giữ ít nhất 1 cấu hình server.");
            return;
        }
        let id = self.config.active_server_id.clone();
        self.config.servers.retain(|s| s.id != id);
        self.config.active_server_id = self.config.servers[0].id.clone();
        self.save_config();
        self.refresh_banlist();
        self.notify_ok("Đã xóa cấu hình server.");
    }

    pub fn add_saved_join_server(&mut self) {
        let host = self.new_join_host.trim().to_string();
        if host.is_empty() {
            self.notify_err("Nhập địa chỉ IP hoặc tên miền server.");
            return;
        }
        let port = self.new_join_port.trim().parse::<u16>().unwrap_or(7777);
        let name = if self.new_join_name.trim().is_empty() {
            format!("{host}:{port}")
        } else {
            self.new_join_name.trim().to_string()
        };
        self.config.saved_join_servers.push(SavedJoinServer {
            name,
            host,
            port,
            password: self.new_join_password.clone(),
            edition: self.config.active_edition,
        });
        self.new_join_name.clear();
        self.new_join_host.clear();
        self.new_join_port = "7777".to_string();
        self.new_join_password.clear();
        self.save_config();
        self.notify_ok("Đã lưu server vào danh sách tham gia nhanh.");
    }

    pub fn shutdown_all(&mut self) {
        self.save_config();
        if let Some(tunnel) = self.bore_handle.take() {
            tunnel.stop();
        }
        if let Some(lease) = self.upnp_lease.take() {
            let _ = self.rt.block_on(close_upnp_port(&lease));
        }
        if let Some(sup) = &self.server_supervisor {
            if sup.is_active() {
                sup.request_graceful_stop();
                std::thread::sleep(Duration::from_millis(600));
            }
        }
    }

    pub fn badge_color_for_server(&self) -> egui::Color32 {
        match self.server_snapshot.as_ref().map(|s| &s.state) {
            Some(ServerLifecycleState::Running) => ACCENT_EMERALD,
            Some(ServerLifecycleState::Starting) => crate::theme::ACCENT_CYAN,
            Some(ServerLifecycleState::Stopping) => crate::theme::ACCENT_AMBER,
            Some(ServerLifecycleState::Crashed(_)) => crate::theme::ACCENT_RED,
            _ => crate::theme::TEXT_MUTED,
        }
    }

    pub fn server_status_short(&self) -> &str {
        self.server_snapshot
            .as_ref()
            .map(|s| s.state.label())
            .unwrap_or("Chưa bật")
    }

    pub fn active_tunnel_status(&self) -> TunnelStatus {
        self.bore_handle
            .as_ref()
            .map(|h| h.current_status())
            .unwrap_or(TunnelStatus::Idle)
    }
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

fn uuid_short() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{:x}", now & 0xFFFF_FFFF)
}
