use crate::game_runner::{
    apply_dotnet_tokio_env, apply_dotnet_tuning_tokio_env, map_dotnet_spawn_error,
    native_lib_path_env, resolve_dotnet_launch, split_shell_args, DotnetLaunchSpec,
    RuntimeTuning,
};
#[cfg(windows)]
use crate::game_runner::windows_priority_flag;
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf, Prefix};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tl_core::{
    is_xna_framework_installed, prepare_folder_modpack_for_server, GameEdition, LauncherError,
    LauncherPaths, LauncherResult, ServerConfigData, ServerProfile,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Notify};
#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, HANDLE,
};
#[cfg(windows)]
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
#[cfg(windows)]
use windows::Win32::System::Threading::CreateMutexW;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_CONSOLE_LINES: usize = 2000;
const LOG_TAIL_MAX_BYTES: u64 = 128 * 1024;
const GRACEFUL_STOP_RUNNING_SECS: u64 = 12;
const GRACEFUL_STOP_STARTING_SECS: u64 = 2;

pub(crate) fn encode_server_stdin_line(line: &str) -> Vec<u8> {
    let full = format!("{line}\r\n");
    #[cfg(windows)]
    {
        // Both Vanilla TerrariaServer.exe and tModLoader.dll set Console.InputEncoding = Encoding.Unicode (UTF-16LE) on Windows.
        let utf16: Vec<u16> = full.encode_utf16().collect();
        let mut bytes = Vec::with_capacity(utf16.len() * 2);
        for unit in utf16 {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }
    #[cfg(not(windows))]
    {
        full.into_bytes()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerLifecycleState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Crashed(String),
}

impl ServerLifecycleState {
    pub fn label(&self) -> &str {
        match self {
            Self::Stopped => "Đã dừng",
            Self::Starting => "Đang khởi động...",
            Self::Running => "Đang chạy",
            Self::Stopping => "Đang lưu & tắt...",
            Self::Crashed(_) => "Dừng đột ngột",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleLogLine {
    pub timestamp: String,
    pub is_error: bool,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedPlayer {
    pub name: String,
    pub ip: Option<String>,
    pub joined_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BanEntry {
    pub label: String,
    pub ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerRuntimeSnapshot {
    pub profile_id: String,
    pub profile_name: String,
    pub edition: GameEdition,
    pub port: u16,
    pub effective_world: String,
    pub effective_password: String,
    pub effective_modpack: String,
    pub state: ServerLifecycleState,
    pub status_detail: String,
    pub pid: Option<u32>,
    pub memory_mb: u64,
    pub cpu_percent: f32,
    pub players: Vec<ConnectedPlayer>,
}

/// Dedicated Terraria or tModLoader server process discovered at the OS level.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedExternalServer {
    pub pid: u32,
    pub edition: GameEdition,
    pub port: Option<u16>,
    pub profile_id: Option<String>,
    pub memory_mb: u64,
    pub cpu_percent: f32,
}

#[derive(Debug, Clone, Default)]
pub struct ExternalServerLogSummary {
    pub lines: Vec<ConsoleLogLine>,
    pub status_detail: String,
    pub players: Vec<ConnectedPlayer>,
    pub is_listening: bool,
}

#[cfg(windows)]
struct KillOnCloseJob(isize);

#[cfg(windows)]
impl KillOnCloseJob {
    fn new() -> LauncherResult<Self> {
        let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|e| {
            LauncherError::Process(format!("Không thể tạo Windows Job Object: {e}"))
        })?;
        let job = Self(handle.0 as isize);

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &raw const info as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(|e| {
            LauncherError::Process(format!(
                "Không thể cấu hình JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: {e}"
            ))
        })?;

        Ok(job)
    }

    fn assign_child(&self, child: &Child) {
        let Some(raw) = child.raw_handle() else {
            return;
        };
        let job_handle = HANDLE(self.0 as *mut _);
        let process_handle = HANDLE(raw);
        if let Err(e) = unsafe { AssignProcessToJobObject(job_handle, process_handle) } {
            tracing::warn!("Could not attach server child to kill-on-close job object: {e}");
        }
    }
}

#[cfg(windows)]
impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        let handle = HANDLE(self.0 as *mut _);
        if !handle.is_invalid() {
            let _ = unsafe { CloseHandle(handle) };
        }
    }
}

fn resolve_effective_join_metadata(profile: &ServerProfile) -> (u16, String, String) {
    let mut effective_port = profile.config.port;
    let mut effective_password = profile.config.password.clone();
    let mut effective_modpack = if profile.edition == GameEdition::TModLoader {
        profile.config.modpack.trim().to_string()
    } else {
        String::new()
    };

    let args = split_shell_args(&profile.extra_args);
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].trim();
        if arg.eq_ignore_ascii_case("-port") || arg.eq_ignore_ascii_case("-p") {
            if let Some(next) = args.get(i + 1) {
                let cleaned = next.trim().trim_matches('"');
                if !cleaned.starts_with('-') {
                    if let Ok(parsed) = cleaned.parse::<u16>() {
                        if parsed > 0 {
                            effective_port = parsed;
                        }
                    }
                    i += 2;
                    continue;
                }
            }
        } else if arg.eq_ignore_ascii_case("-pass") || arg.eq_ignore_ascii_case("-password") {
            if let Some(next) = args.get(i + 1) {
                if !next.starts_with('-') {
                    effective_password = next.clone();
                    i += 2;
                    continue;
                }
            }
        } else if arg.eq_ignore_ascii_case("-modpack") {
            if let Some(next) = args.get(i + 1) {
                let cleaned = next.trim();
                if !cleaned.starts_with('-') {
                    if profile.edition == GameEdition::TModLoader {
                        effective_modpack = cleaned.to_string();
                    }
                    i += 2;
                    continue;
                }
            }
        } else if let Some((flag, value)) = arg.split_once('=') {
            let flag = flag.trim();
            if flag.eq_ignore_ascii_case("-port") || flag.eq_ignore_ascii_case("-p") {
                if let Ok(parsed) = value.trim().trim_matches('"').parse::<u16>() {
                    if parsed > 0 {
                        effective_port = parsed;
                    }
                }
            } else if flag.eq_ignore_ascii_case("-pass") || flag.eq_ignore_ascii_case("-password") {
                effective_password = value.trim_matches('"').to_string();
            } else if flag.eq_ignore_ascii_case("-modpack")
                && profile.edition == GameEdition::TModLoader
            {
                effective_modpack = value.trim().trim_matches('"').to_string();
            }
        }
        i += 1;
    }

    (effective_port, effective_password, effective_modpack)
}

#[derive(Debug)]
#[cfg(windows)]
pub struct SingleInstanceGuard(windows::Win32::Foundation::HANDLE);

#[derive(Debug)]
#[cfg(not(windows))]
pub struct SingleInstanceGuard(());

#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(windows)]
fn acquire_named_single_instance_guard(
    mutex_name: PCWSTR,
) -> Result<Option<SingleInstanceGuard>, windows::core::Error> {
    match unsafe { CreateMutexW(None, false, mutex_name) } {
        Ok(handle) => {
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                let _ = unsafe { CloseHandle(handle) };
                Ok(None)
            } else {
                Ok(Some(SingleInstanceGuard(handle)))
            }
        }
        Err(err) => {
            if err.code() == ERROR_ACCESS_DENIED.to_hresult() {
                Ok(None)
            } else {
                Err(err)
            }
        }
    }
}

pub fn focus_existing_launcher_window() {
    #[cfg(windows)]
    {
        if let Ok(hwnd) =
            unsafe { FindWindowW(PCWSTR::null(), windows::core::w!("Terraria Launcher")) }
        {
            if !hwnd.is_invalid() {
                let _ = unsafe { ShowWindow(hwnd, SW_RESTORE) };
                let _ = unsafe { SetForegroundWindow(hwnd) };
            }
        }
    }
}

pub fn acquire_single_instance_guard() -> Option<SingleInstanceGuard> {
    #[cfg(windows)]
    {
        match acquire_named_single_instance_guard(windows::core::w!(
            "Local\\TerrariaLauncher_SingleInstance_v1"
        )) {
            Ok(Some(guard)) => Some(guard),
            Ok(None) => {
                focus_existing_launcher_window();
                None
            }
            Err(err) => {
                tracing::warn!("Could not initialize single-instance mutex: {err}");
                Some(SingleInstanceGuard(HANDLE::default()))
            }
        }
    }
    #[cfg(not(windows))]
    {
        Some(SingleInstanceGuard(()))
    }
}

struct ServerSharedState {
    snapshot: ServerRuntimeSnapshot,
    logs: VecDeque<ConsoleLogLine>,
    log_file_path: PathBuf,
}

#[derive(Clone)]
pub struct ServerSupervisor {
    shared: Arc<RwLock<ServerSharedState>>,
    stdin_tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    stop_requested: Arc<AtomicBool>,
    stop_notify: Arc<Notify>,
    child_pid: Arc<Mutex<Option<u32>>>,
    run_generation: Arc<AtomicU64>,
    exit_notify: Arc<Notify>,
    async_ctx: Arc<Mutex<Option<(tokio::runtime::Handle, egui_repaint::RepaintHandle)>>>,
    #[cfg(windows)]
    _job_object: Arc<Option<KillOnCloseJob>>,
}

impl ServerSupervisor {
    pub fn new(paths: &LauncherPaths, profile: &ServerProfile) -> Self {
        let log_dir = paths.servers_dir.join(&profile.id);
        let _ = fs::create_dir_all(&log_dir);
        let log_file_path = log_dir.join("server.log");
        let (effective_port, effective_password, effective_modpack) =
            resolve_effective_join_metadata(profile);
        #[cfg(windows)]
        let job_object = match KillOnCloseJob::new() {
            Ok(job) => Arc::new(Some(job)),
            Err(e) => {
                tracing::warn!("Could not initialize kill-on-close job object: {e}");
                Arc::new(None)
            }
        };
        Self {
            shared: Arc::new(RwLock::new(ServerSharedState {
                snapshot: ServerRuntimeSnapshot {
                    profile_id: profile.id.clone(),
                    profile_name: profile.name.clone(),
                    edition: profile.edition,
                    port: effective_port,
                    effective_world: profile.config.world.clone(),
                    effective_password,
                    effective_modpack,
                    state: ServerLifecycleState::Stopped,
                    status_detail: "Sẵn sàng".to_string(),
                    pid: None,
                    memory_mb: 0,
                    cpu_percent: 0.0,
                    players: Vec::new(),
                },
                logs: VecDeque::with_capacity(MAX_CONSOLE_LINES),
                log_file_path,
            })),
            stdin_tx: Arc::new(Mutex::new(None)),
            stop_requested: Arc::new(AtomicBool::new(false)),
            stop_notify: Arc::new(Notify::new()),
            child_pid: Arc::new(Mutex::new(None)),
            run_generation: Arc::new(AtomicU64::new(0)),
            exit_notify: Arc::new(Notify::new()),
            async_ctx: Arc::new(Mutex::new(None)),
            #[cfg(windows)]
            _job_object: job_object,
        }
    }

    pub fn snapshot(&self, sys: &mut System) -> ServerRuntimeSnapshot {
        let pid_opt = *self.child_pid.lock().unwrap_or_else(|e| e.into_inner());
        let mut guard = self.shared.write().unwrap_or_else(|e| e.into_inner());
        if let Some(pid_u32) = pid_opt {
            let pid = Pid::from_u32(pid_u32);
            sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                true,
                ProcessRefreshKind::nothing().with_memory().with_cpu(),
            );
            if let Some(proc_info) = sys.process(pid) {
                guard.snapshot.memory_mb = proc_info.memory() / (1024 * 1024);
                guard.snapshot.cpu_percent = proc_info.cpu_usage();
            }
        } else {
            guard.snapshot.memory_mb = 0;
            guard.snapshot.cpu_percent = 0.0;
        }
        guard.snapshot.clone()
    }

    pub fn logs(&self) -> Vec<ConsoleLogLine> {
        self.shared
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .logs
            .iter()
            .cloned()
            .collect()
    }

    pub fn clear_logs(&self) {
        self.shared
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .logs
            .clear();
    }

    pub fn is_active(&self) -> bool {
        let state = &self
            .shared
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .state;
        matches!(
            state,
            ServerLifecycleState::Starting
                | ServerLifecycleState::Running
                | ServerLifecycleState::Stopping
        )
    }

    pub fn has_stdin(&self) -> bool {
        self.stdin_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    fn request_ui_repaint(&self) {
        if let Some((_, repaint)) = self
            .async_ctx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            repaint.wake();
        }
    }

    fn spawn_stop_watchdog(&self, prev_state: ServerLifecycleState) {
        let Some((rt, repaint)) = self
            .async_ctx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        let sup = self.clone();
        let expected_gen = self.run_generation.load(Ordering::SeqCst);
        let grace = if matches!(prev_state, ServerLifecycleState::Starting) {
            Duration::from_secs(GRACEFUL_STOP_STARTING_SECS)
        } else {
            Duration::from_secs(GRACEFUL_STOP_RUNNING_SECS)
        };
        rt.spawn(async move {
            tokio::select! {
                _ = sup.exit_notify.notified() => {}
                _ = tokio::time::sleep(grace) => {
                    if sup.run_generation.load(Ordering::SeqCst) == expected_gen
                        && sup.is_active()
                    {
                        push_log_line(
                            &sup.shared,
                            true,
                            "[Launcher] Quá thời gian chờ lưu thế giới, đang buộc dừng tiến trình server.".to_string(),
                        );
                        sup.force_kill();
                        repaint.wake();
                    }
                }
            }
        });
    }

    fn dispatch_command(&self, cmd: &str, interactive: bool) -> LauncherResult<()> {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let sender = self
            .stdin_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| LauncherError::Process("Server chưa chạy".to_string()))?;

        let mut watchdog_prev_state = None;
        if interactive {
            if let Some((verb, rest)) = trimmed.split_once(char::is_whitespace) {
                if verb.eq_ignore_ascii_case("ban") {
                    let target = rest.trim().trim_matches('"').trim();
                    if !target.is_empty() {
                        let is_loopback_player = is_loopback_ban_target(target)
                            || self
                                .shared
                                .read()
                                .unwrap_or_else(|e| e.into_inner())
                                .snapshot
                                .players
                                .iter()
                                .any(|p| {
                                    p.name.eq_ignore_ascii_case(target)
                                        && p.ip.as_deref().is_some_and(|ip| {
                                            ip.starts_with("127.0.0.1")
                                                || ip.starts_with("::1")
                                                || ip.starts_with("[::1]")
                                                || is_loopback_ban_target(ip)
                                        })
                                });
                        if is_loopback_player {
                            return Err(LauncherError::InvalidInput(
                                "Người chơi đang qua cầu nối cục bộ — hãy dùng nút Chặn IP trong mục Người chơi thay vì gõ 'ban' để tránh tự khóa 127.0.0.1.".to_string(),
                            ));
                        }
                    }
                }
            }

            if trimmed.eq_ignore_ascii_case("exit")
                || trimmed.eq_ignore_ascii_case("exit-nosave")
            {
                self.stop_requested.store(true, Ordering::SeqCst);
                self.stop_notify.notify_waiters();
                {
                    let mut guard = self.shared.write().unwrap_or_else(|e| e.into_inner());
                    let prev = guard.snapshot.state.clone();
                    if matches!(
                        prev,
                        ServerLifecycleState::Running | ServerLifecycleState::Starting
                    ) {
                        watchdog_prev_state = Some(prev);
                    }
                    guard.snapshot.state = ServerLifecycleState::Stopping;
                    guard.snapshot.status_detail = if trimmed.eq_ignore_ascii_case("exit") {
                        "Đang lưu thế giới và dừng server...".to_string()
                    } else {
                        "Đang dừng server...".to_string()
                    };
                }
                self.request_ui_repaint();
            }
            push_log_line(&self.shared, false, format!("> {trimmed}"));
        }

        sender
            .send(trimmed.to_string())
            .map_err(|_| LauncherError::Process("Kênh stdin của server đã đóng".to_string()))?;

        if let Some(prev_state) = watchdog_prev_state {
            self.spawn_stop_watchdog(prev_state);
        }
        Ok(())
    }

    pub fn send_silent_command(&self, cmd: &str) -> LauncherResult<()> {
        self.dispatch_command(cmd, false)
    }

    pub fn send_command(&self, cmd: &str) -> LauncherResult<()> {
        self.dispatch_command(cmd, true)
    }

    pub fn is_stopping(&self) -> bool {
        let state = &self
            .shared
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .state;
        matches!(state, ServerLifecycleState::Stopping)
    }

    pub fn request_graceful_stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        self.stop_notify.notify_waiters();
        let prev_state = {
            let mut guard = self.shared.write().unwrap_or_else(|e| e.into_inner());
            let prev = guard.snapshot.state.clone();
            if matches!(
                prev,
                ServerLifecycleState::Running | ServerLifecycleState::Starting
            ) {
                guard.snapshot.state = ServerLifecycleState::Stopping;
                guard.snapshot.status_detail =
                    "Đang lưu thế giới và dừng server...".to_string();
            }
            prev
        };

        if matches!(
            prev_state,
            ServerLifecycleState::Stopped
                | ServerLifecycleState::Crashed(_)
                | ServerLifecycleState::Stopping
        ) {
            return;
        }

        self.request_ui_repaint();

        if self.send_command("exit").is_err() {
            self.force_kill();
            return;
        }

        self.spawn_stop_watchdog(prev_state);
    }

    pub fn force_kill(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        self.stop_notify.notify_waiters();
        *self.stdin_tx.lock().unwrap_or_else(|e| e.into_inner()) = None;
        {
            let mut guard = self.shared.write().unwrap_or_else(|e| e.into_inner());
            if matches!(
                guard.snapshot.state,
                ServerLifecycleState::Starting
                    | ServerLifecycleState::Running
                    | ServerLifecycleState::Stopping
            ) {
                guard.snapshot.state = ServerLifecycleState::Stopping;
                guard.snapshot.status_detail = "Đang buộc dừng tiến trình server...".to_string();
            }
        }
        let pid_opt = *self.child_pid.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pid) = pid_opt {
            push_log_line(
                &self.shared,
                false,
                format!("[Launcher] Đang buộc dừng tiến trình PID {pid}."),
            );
            if let Ok(mut sys) = System::new() {
                let _ = kill_tracked_child_pid(&mut sys, pid);
            }
        }
        self.request_ui_repaint();
    }

    pub fn wait_for_exit_timeout(&self, max_wait: Duration) -> bool {
        let deadline = Instant::now() + max_wait;
        while self.is_active() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            std::thread::sleep(remaining.min(Duration::from_millis(100)));
        }
        true
    }
}

pub fn resolve_tmodloader_server_modpack_config(
    paths: &LauncherPaths,
    edition: GameEdition,
    effective_cfg: &mut ServerConfigData,
) -> LauncherResult<()> {
    if edition != GameEdition::TModLoader {
        return Ok(());
    }
    let pack_name = effective_cfg.modpack.trim().to_string();
    if pack_name.is_empty() {
        return Ok(());
    }

    let prepared = prepare_folder_modpack_for_server(
        &paths.tml_modpacks_dir(),
        &paths.tml_mods_dir(),
        &pack_name,
    )?;
    let legacy_json_exists = !pack_name.contains(['/', '\\', ':'])
        && pack_name != "."
        && pack_name != ".."
        && paths
            .tml_modpacks_dir()
            .join(format!("{pack_name}.json"))
            .is_file();

    match prepared {
        Some(pack_mods_dir) => {
            if effective_cfg.modpath.trim().is_empty() {
                effective_cfg.modpath = pack_mods_dir.to_string_lossy().into_owned();
            }
            effective_cfg.modpack.clear();
        }
        None => {
            if legacy_json_exists {
                effective_cfg.modpack = pack_name;
            } else {
                effective_cfg.modpack.clear();
            }
        }
    }

    Ok(())
}

fn prepare_effective_server_config(
    paths: &LauncherPaths,
    profile: &ServerProfile,
    server_dir: &Path,
    tuning: RuntimeTuning,
) -> LauncherResult<ServerConfigData> {
    let mut effective_cfg = profile.config.clone();
    if tuning.disable_builtin_upnp {
        effective_cfg.upnp = false;
    }
    if effective_cfg.worldpath.trim().is_empty() {
        effective_cfg.worldpath = match profile.edition {
            GameEdition::Vanilla => paths.vanilla_worlds_dir(),
            GameEdition::TModLoader => paths.tml_worlds_dir(),
        }
        .to_string_lossy()
        .into_owned();
    }
    effective_cfg.world = effective_cfg.world.trim().to_string();
    if effective_cfg.world.is_empty() && !effective_cfg.worldname.trim().is_empty() {
        effective_cfg.world = Path::new(&effective_cfg.worldpath)
            .join(format!("{}.wld", effective_cfg.worldname.trim()))
            .to_string_lossy()
            .into_owned();
    }
    if effective_cfg.world.trim().is_empty()
        || (!Path::new(&effective_cfg.world).is_file() && effective_cfg.autocreate.is_none())
    {
        return Err(LauncherError::InvalidInput(
            "File thế giới chưa tồn tại và tính năng Tự tạo World đang tắt. Hãy chọn thế giới có sẵn hoặc bật Tự tạo World.".to_string(),
        ));
    }
    if effective_cfg.language.trim().is_empty() {
        effective_cfg.language = "en-US".to_string();
    }

    let ban_trimmed = effective_cfg.banlist.trim();
    if ban_trimmed.is_empty() || Path::new(ban_trimmed).is_relative() {
        let file_name = Path::new(ban_trimmed)
            .file_name()
            .filter(|_| !ban_trimmed.is_empty())
            .unwrap_or_else(|| OsStr::new("banlist.txt"));
        effective_cfg.banlist = server_dir.join(file_name).to_string_lossy().into_owned();
    } else {
        effective_cfg.banlist = ban_trimmed.to_string();
    }
    if let Some(ban_parent) = Path::new(&effective_cfg.banlist).parent() {
        let _ = fs::create_dir_all(ban_parent);
    }
    if !Path::new(&effective_cfg.banlist).exists() {
        let _ = fs::write(&effective_cfg.banlist, "");
    }
    resolve_tmodloader_server_modpack_config(paths, profile.edition, &mut effective_cfg)?;
    Ok(effective_cfg)
}

pub fn start_server(
    rt: &tokio::runtime::Handle,
    paths: LauncherPaths,
    profile: ServerProfile,
    tuning: RuntimeTuning,
    repaint_ctx: egui_repaint::RepaintHandle,
) -> LauncherResult<ServerSupervisor> {
    paths.ensure_dirs()?;

    if profile.edition == GameEdition::Vanilla {
        if !paths.terraria_server_exe().is_file() {
            return Err(LauncherError::NotFound(format!(
                "Không tìm thấy TerrariaServer.exe tại {}",
                paths.terraria_server_exe().display()
            )));
        }
        if !is_xna_framework_installed() {
            return Err(LauncherError::NotFound(
                "Thiếu Microsoft XNA Framework 4.0 cho TerrariaServer.exe. Hãy cài XNA 4.0 trước."
                    .to_string(),
            ));
        }
    } else if !paths.tmodloader_dll().is_file() {
        return Err(LauncherError::NotFound(format!(
            "Không tìm thấy tModLoader.dll tại {}",
            paths.tmodloader_dll().display()
        )));
    }

    let server_dir = paths.servers_dir.join(&profile.id);
    fs::create_dir_all(&server_dir)?;

    let effective_cfg = prepare_effective_server_config(&paths, &profile, &server_dir, tuning)?;

    let config_file = server_dir.join("serverconfig.txt");
    fs::write(&config_file, effective_cfg.to_config_text(profile.edition))?;

    let supervisor = ServerSupervisor::new(&paths, &profile);
    *supervisor
        .async_ctx
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some((rt.clone(), repaint_ctx.clone()));
    let (effective_port, effective_password, effective_modpack) =
        resolve_effective_join_metadata(&profile);
    {
        let mut guard = supervisor.shared.write().unwrap_or_else(|e| e.into_inner());
        guard.snapshot.state = ServerLifecycleState::Starting;
        guard.snapshot.status_detail = "Đang khởi chạy tiến trình server...".to_string();
        guard.snapshot.port = effective_port;
        guard.snapshot.effective_world = effective_cfg.world.clone();
        guard.snapshot.effective_password = effective_password;
        guard.snapshot.effective_modpack = effective_modpack;
    }

    let sup_clone = supervisor.clone();
    rt.spawn(async move {
        run_server_supervisor(
            sup_clone,
            paths,
            profile,
            config_file,
            tuning,
            repaint_ctx,
        )
        .await;
    });

    Ok(supervisor)
}

fn is_clean_server_exit(
    user_stopped: bool,
    prev_state: &ServerLifecycleState,
    exit_success: bool,
) -> bool {
    let was_running_or_stopping = matches!(
        prev_state,
        ServerLifecycleState::Running | ServerLifecycleState::Stopping
    );
    user_stopped || (was_running_or_stopping && exit_success)
}

fn mark_stopped_by_request(sup: &ServerSupervisor, repaint: &egui_repaint::RepaintHandle) {
    {
        let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
        guard.snapshot.pid = None;
        guard.snapshot.players.clear();
        guard.snapshot.state = ServerLifecycleState::Stopped;
        guard.snapshot.status_detail = "Đã dừng theo yêu cầu".to_string();
    }
    sup.exit_notify.notify_waiters();
    repaint.wake();
}

async fn run_server_supervisor(
    sup: ServerSupervisor,
    paths: LauncherPaths,
    profile: ServerProfile,
    config_file: PathBuf,
    tuning: RuntimeTuning,
    repaint: egui_repaint::RepaintHandle,
) {
    loop {
        if sup.stop_requested.load(Ordering::SeqCst) {
            mark_stopped_by_request(&sup, &repaint);
            break;
        }
        match spawn_server_process(&paths, &profile, &config_file, tuning) {
            Ok(mut child) => {
                #[cfg(windows)]
                if let Some(job) = sup._job_object.as_ref() {
                    job.assign_child(&child);
                }
                if sup.stop_requested.load(Ordering::SeqCst) {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    mark_stopped_by_request(&sup, &repaint);
                    break;
                }
                let pid = child.id();
                sup.run_generation.fetch_add(1, Ordering::SeqCst);
                *sup.child_pid.lock().unwrap_or_else(|e| e.into_inner()) = pid;
                {
                    let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
                    guard.snapshot.pid = pid;
                    guard.snapshot.state = ServerLifecycleState::Starting;
                    guard.snapshot.players.clear();
                }
                repaint.wake();

                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let stdin = child.stdin.take();

                let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<String>();
                *sup.stdin_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(cmd_tx);

                if let Some(stdin) = stdin {
                    tokio::spawn(stdin_writer_task(stdin, cmd_rx));
                }
                let out_handle = stdout.map(|out| {
                    let s = sup.clone();
                    let r = repaint.clone();
                    tokio::spawn(async move {
                        let mut lines = BufReader::new(out).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            handle_server_output_line(&s, false, &line);
                            r.wake();
                        }
                    })
                });
                let err_handle = stderr.map(|err| {
                    let s = sup.clone();
                    let r = repaint.clone();
                    tokio::spawn(async move {
                        let mut lines = BufReader::new(err).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            handle_server_output_line(&s, true, &line);
                            r.wake();
                        }
                    })
                });

                let exit_res = child.wait().await;
                if let Some(h) = out_handle {
                    let _ = h.await;
                }
                if let Some(h) = err_handle {
                    let _ = h.await;
                }
                *sup.stdin_tx.lock().unwrap_or_else(|e| e.into_inner()) = None;
                *sup.child_pid.lock().unwrap_or_else(|e| e.into_inner()) = None;

                let user_stopped = sup.stop_requested.load(Ordering::SeqCst);
                let clean_shutdown = {
                    let guard = sup.shared.read().unwrap_or_else(|e| e.into_inner());
                    is_clean_server_exit(
                        user_stopped,
                        &guard.snapshot.state,
                        exit_res.as_ref().is_ok_and(|st| st.success()),
                    )
                };
                let exit_desc = match &exit_res {
                    Ok(st) => format!("Mã thoát: {:?}", st.code()),
                    Err(e) => format!("Lỗi chờ tiến trình: {e}"),
                };
                push_log_line(
                    &sup.shared,
                    !clean_shutdown,
                    format!("[Launcher] Tiến trình server đã kết thúc ({exit_desc})."),
                );

                if clean_shutdown || !profile.auto_restart {
                    {
                        let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
                        guard.snapshot.pid = None;
                        guard.snapshot.players.clear();
                        if clean_shutdown {
                            guard.snapshot.state = ServerLifecycleState::Stopped;
                            guard.snapshot.status_detail = "Đã dừng".to_string();
                        } else {
                            guard.snapshot.state = ServerLifecycleState::Crashed(exit_desc.clone());
                            guard.snapshot.status_detail = format!("Dừng bất thường ({exit_desc})");
                        }
                    }
                    sup.exit_notify.notify_waiters();
                    repaint.wake();
                    break;
                }

                if !wait_for_auto_restart_or_stop(&sup, &repaint).await {
                    break;
                }
            }
            Err(e) => {
                push_log_line(&sup.shared, true, format!("[Launcher] Lỗi khởi chạy: {e}"));
                let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
                guard.snapshot.state = ServerLifecycleState::Crashed(e.to_string());
                guard.snapshot.status_detail = e.to_string();
                repaint.wake();
                break;
            }
        }
    }
}

async fn wait_for_auto_restart_or_stop(
    sup: &ServerSupervisor,
    repaint: &egui_repaint::RepaintHandle,
) -> bool {
    {
        let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
        guard.snapshot.pid = None;
        guard.snapshot.players.clear();
        guard.snapshot.state = ServerLifecycleState::Starting;
        guard.snapshot.status_detail = "Server tự khởi động lại sau 3 giây...".to_string();
    }
    repaint.wake();

    let stop_wait = sup.stop_notify.notified();
    tokio::pin!(stop_wait);
    stop_wait.as_mut().enable();
    if !sup.stop_requested.load(Ordering::SeqCst) {
        tokio::select! {
            _ = &mut stop_wait => {}
            _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {}
        }
    }

    if sup.stop_requested.load(Ordering::SeqCst) {
        {
            let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
            guard.snapshot.pid = None;
            guard.snapshot.state = ServerLifecycleState::Stopped;
            guard.snapshot.players.clear();
            guard.snapshot.status_detail = "Đã dừng theo yêu cầu".to_string();
        }
        sup.exit_notify.notify_waiters();
        repaint.wake();
        return false;
    }

    true
}

fn spawn_server_process(
    paths: &LauncherPaths,
    profile: &ServerProfile,
    config_file: &Path,
    tuning: RuntimeTuning,
) -> LauncherResult<Child> {
    let mut dotnet_spec: Option<DotnetLaunchSpec> = None;
    let mut cmd = match profile.edition {
        GameEdition::Vanilla => {
            let mut c = Command::new(paths.terraria_server_exe());
            c.current_dir(&paths.terraria_dir);
            c.arg("-savedirectory").arg(&paths.vanilla_saves_dir);
            c
        }
        GameEdition::TModLoader => {
            let spec = resolve_dotnet_launch(paths);
            let mut c = Command::new(&spec.executable);
            c.current_dir(&paths.tmodloader_dir);
            apply_dotnet_tokio_env(&spec, &mut c);
            apply_dotnet_tuning_tokio_env(tuning, &mut c);
            if let Some(path_env) = native_lib_path_env(&paths.tmodloader_dir) {
                c.env("PATH", path_env);
            }
            c.arg(paths.tmodloader_dll())
                .arg("-server")
                .arg("-nosteam")
                .arg("-tmlsavedirectory")
                .arg(&paths.tml_saves_dir);
            dotnet_spec = Some(spec);
            c
        }
    };

    cmd.arg("-config").arg(config_file);
    if !profile.config.upnp || tuning.disable_builtin_upnp {
        cmd.arg("-noupnp");
    }
    for extra in split_shell_args(&profile.extra_args) {
        cmd.arg(extra);
    }

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW | windows_priority_flag(tuning.process_priority));

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    cmd.spawn().map_err(|e| match &dotnet_spec {
        Some(spec) => map_dotnet_spawn_error(e, spec, "Không thể tạo tiến trình server"),
        None => LauncherError::Process(format!("Không thể tạo tiến trình server: {e}")),
    })
}

async fn stdin_writer_task(mut stdin: ChildStdin, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(line) = rx.recv().await {
        let encoded = encode_server_stdin_line(&line);
        if stdin.write_all(&encoded).await.is_err() {
            break;
        }
        let _ = stdin.flush().await;
    }
}

fn push_log_line(shared: &Arc<RwLock<ServerSharedState>>, is_error: bool, text: String) {
    let timestamp = Local::now().format("%H:%M:%S").to_string();
    let mut guard = shared.write().unwrap_or_else(|e| e.into_inner());
    if guard.logs.len() >= MAX_CONSOLE_LINES {
        guard.logs.pop_front();
    }
    let log_path = guard.log_file_path.clone();
    guard.logs.push_back(ConsoleLogLine {
        timestamp: timestamp.clone(),
        is_error,
        text: text.clone(),
    });
    drop(guard);

    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_path) {
        let _ = writeln!(f, "[{timestamp}] {text}");
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LogEventOutcome {
    became_running: bool,
    player_joined: bool,
    listening_port: Option<u16>,
}

fn parse_listening_port(normalized: &str) -> Option<u16> {
    let (_, rest) = normalized.split_once("Listening on port ")?;
    let trimmed = rest.trim_start();
    let digit_len = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digit_len == 0 {
        return None;
    }
    let trailing = &trimmed[digit_len..];
    if trailing.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return None;
    }
    trimmed[..digit_len].parse::<u16>().ok().filter(|&p| p > 0)
}

fn strip_prompt_prefix(text: &str) -> &str {
    let mut s = text.trim();
    while let Some(rest) = s.strip_prefix(':') {
        if rest.is_empty() {
            return "";
        }
        if rest.starts_with(char::is_whitespace) {
            s = rest.trim_start();
        } else {
            break;
        }
    }
    s
}

fn parse_playing_player_line(normalized: &str) -> Option<(&str, &str)> {
    let cleaned = strip_prompt_prefix(normalized);
    let (name_part, rest) = cleaned.rsplit_once(" (")?;
    let ip_raw = rest.strip_suffix(')')?;
    let pname = name_part.trim();
    let ip_part = ip_raw.trim();
    if pname.is_empty()
        || pname.starts_with('<')
        || pname.starts_with('[')
        || pname.starts_with('>')
    {
        return None;
    }
    if ip_part.parse::<SocketAddr>().is_ok() || ip_part.parse::<IpAddr>().is_ok() {
        Some((pname, ip_part))
    } else {
        None
    }
}

fn handle_server_output_line(sup: &ServerSupervisor, is_error: bool, raw_line: &str) {
    let clean = raw_line.trim_end_matches(['\r', '\n']);
    if clean.is_empty() {
        return;
    }
    push_log_line(&sup.shared, is_error, clean.to_string());

    let (outcome, is_stopping) = {
        let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
        let now = Local::now().format("%H:%M:%S").to_string();
        let snap = &mut guard.snapshot;
        let is_stopping = sup.stop_requested.load(Ordering::SeqCst)
            || snap.state == ServerLifecycleState::Stopping;
        let outcome =
            apply_log_event_to_state(clean, &now, &mut snap.status_detail, &mut snap.players);
        if let Some(parsed_port) = outcome.listening_port {
            snap.port = parsed_port;
        }
        if outcome.became_running && !is_stopping {
            snap.state = ServerLifecycleState::Running;
        }
        (outcome, is_stopping)
    };

    if outcome.player_joined && !is_stopping {
        let _ = sup.send_silent_command("playing");
    }
}

fn apply_log_event_to_state(
    text: &str,
    timestamp: &str,
    status_detail: &mut String,
    players: &mut Vec<ConnectedPlayer>,
) -> LogEventOutcome {
    let mut outcome = LogEventOutcome::default();
    let normalized = strip_prompt_prefix(text);
    if normalized.is_empty() {
        return outcome;
    }
    if normalized.starts_with('<') || normalized.starts_with('[') {
        return outcome;
    }

    if normalized.starts_with("Listening on port ") || normalized.starts_with("Server started") {
        *status_detail = normalized.to_string();
        outcome.became_running = true;
        outcome.listening_port = parse_listening_port(normalized);
        return outcome;
    }
    if normalized.starts_with("Saving before exit") || normalized.starts_with("Saving world data") {
        *status_detail = "Đang lưu thế giới trước khi tắt...".to_string();
        return outcome;
    }
    if normalized.starts_with("Loading world data:")
        || normalized.starts_with("Settling liquids")
        || normalized.starts_with("Resetting game objects")
        || normalized.starts_with("Finding Mods")
        || normalized.starts_with("Constructing Mods")
        || normalized.starts_with("Adding Content:")
    {
        *status_detail = normalized.to_string();
    } else if normalized.starts_with("Choose World:") {
        *status_detail =
            "Đang chờ chọn World (nhập số thứ tự ở Console hoặc bật tự tạo)".to_string();
    } else if let Some(name) = normalized.strip_suffix(" has joined.") {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            if !players.iter().any(|p| p.name == trimmed) {
                players.push(ConnectedPlayer {
                    name: trimmed.to_string(),
                    ip: None,
                    joined_at: timestamp.to_string(),
                });
            }
            outcome.player_joined = true;
        }
    } else if let Some(name) = normalized.strip_suffix(" has left.") {
        let trimmed = name.trim();
        players.retain(|p| p.name != trimmed);
    } else if normalized == "No players connected." {
        players.clear();
    } else if let Some((pname, ip_part)) = parse_playing_player_line(normalized) {
        let ip_str = ip_part.to_string();
        if let Some(existing) = players.iter_mut().find(|p| p.name == pname) {
            existing.ip = Some(ip_str);
        } else {
            players.push(ConnectedPlayer {
                name: pname.to_string(),
                ip: Some(ip_str),
                joined_at: timestamp.to_string(),
            });
        }
    }
    outcome
}

fn parse_server_log_line(raw: &str) -> Option<ConsoleLogLine> {
    let clean = raw.trim_end_matches(['\r', '\n']);
    if clean.is_empty() {
        return None;
    }
    let bytes = clean.as_bytes();
    let (timestamp, text) = if bytes.len() >= 11
        && bytes[0] == b'['
        && bytes[1].is_ascii_digit()
        && bytes[2].is_ascii_digit()
        && bytes[3] == b':'
        && bytes[4].is_ascii_digit()
        && bytes[5].is_ascii_digit()
        && bytes[6] == b':'
        && bytes[7].is_ascii_digit()
        && bytes[8].is_ascii_digit()
        && bytes[9] == b']'
        && bytes[10] == b' '
    {
        (clean[1..9].to_string(), clean[11..].to_string())
    } else {
        (String::new(), clean.to_string())
    };
    if text.is_empty() {
        return None;
    }
    let is_error = text.starts_with("[Launcher] Lỗi")
        || text.contains("Error:")
        || text.contains("Exception:");
    Some(ConsoleLogLine {
        timestamp,
        is_error,
        text,
    })
}

pub fn summarize_server_log_lines(lines: &[ConsoleLogLine]) -> ExternalServerLogSummary {
    let mut summary = ExternalServerLogSummary {
        lines: lines.to_vec(),
        ..Default::default()
    };

    for line in lines {
        let text = line.text.trim_end_matches(['\r', '\n']);
        if text.starts_with("[Launcher] Tiến trình server đã kết thúc")
            || text.starts_with("Terraria Server ")
            || text.starts_with("Running engine preload...")
        {
            summary.players.clear();
            summary.is_listening = false;
            summary.status_detail.clear();
            continue;
        }

        let outcome = apply_log_event_to_state(
            text,
            &line.timestamp,
            &mut summary.status_detail,
            &mut summary.players,
        );
        if outcome.became_running {
            summary.is_listening = true;
        }
    }

    summary
}

pub fn read_server_log_summary(
    paths: &LauncherPaths,
    profile_id: &str,
    max_lines: usize,
) -> ExternalServerLogSummary {
    let log_path = paths.servers_dir.join(profile_id).join("server.log");
    let Ok(mut file) = File::open(log_path) else {
        return ExternalServerLogSummary::default();
    };
    let Ok(meta) = file.metadata() else {
        return ExternalServerLogSummary::default();
    };
    let len = meta.len();
    if len == 0 {
        return ExternalServerLogSummary::default();
    }

    let seek_pos = len.saturating_sub(LOG_TAIL_MAX_BYTES);
    if seek_pos > 0 && file.seek(SeekFrom::Start(seek_pos)).is_err() {
        return ExternalServerLogSummary::default();
    }

    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return ExternalServerLogSummary::default();
    }

    let slice = if seek_pos > 0 {
        match buf.iter().position(|&b| b == b'\n') {
            Some(idx) => &buf[idx + 1..],
            None => return ExternalServerLogSummary::default(),
        }
    } else {
        &buf[..]
    };

    let text = String::from_utf8_lossy(slice);
    let limit = max_lines.max(1);
    let mut tail = VecDeque::with_capacity(limit.min(MAX_CONSOLE_LINES));
    for raw_line in text.lines() {
        if let Some(parsed) = parse_server_log_line(raw_line) {
            if tail.len() >= limit {
                let _ = tail.pop_front();
            }
            tail.push_back(parsed);
        }
    }

    let lines: Vec<ConsoleLogLine> = tail.into_iter().collect();
    summarize_server_log_lines(&lines)
}

/// Scans running OS processes for Terraria and tModLoader dedicated server instances.
pub fn detect_existing_server_processes(
    sys: &mut System,
    paths: &LauncherPaths,
) -> Vec<DetectedExternalServer> {
    let current_pid = std::process::id();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_memory()
            .with_cpu(),
    );

    let mut detected = Vec::new();
    for (&pid, proc_info) in sys.processes() {
        let pid_u32 = pid.as_u32();
        if pid_u32 == current_pid {
            continue;
        }

        let Some(parsed) = parse_process_as_server(proc_info) else {
            continue;
        };

        let port = parsed.port.or_else(|| {
            parsed
                .config_path
                .as_deref()
                .and_then(|cfg_path| fs::read_to_string(cfg_path).ok())
                .and_then(|text| parse_port_from_serverconfig(&text))
        });
        let profile_id = parsed
            .config_path
            .as_deref()
            .and_then(|cfg_path| extract_profile_id_from_config_path(cfg_path, paths));

        detected.push(DetectedExternalServer {
            pid: pid_u32,
            edition: parsed.edition,
            port,
            profile_id,
            memory_mb: proc_info.memory() / (1024 * 1024),
            cpu_percent: proc_info.cpu_usage(),
        });
    }

    detected.sort_by_key(|server| server.pid);
    detected
}

/// Terminates a running Terraria/tModLoader server process by PID after verifying its identity.
///
/// Refreshes command-line metadata to defend against OS PID reuse and returns `true` only if the
/// target process still matches a known server command line and termination succeeds.
pub fn terminate_process_by_pid(sys: &mut System, pid: u32) -> bool {
    if pid == 0 || pid == std::process::id() {
        return false;
    }
    let spid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[spid]),
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::OnlyIfNotSet),
    );
    let Some(proc_info) = sys.process(spid) else {
        return false;
    };
    if parse_process_as_server(proc_info).is_none() {
        return false;
    }
    proc_info.kill()
}

fn kill_tracked_child_pid(sys: &mut System, pid: u32) -> bool {
    if pid == 0 || pid == std::process::id() {
        return false;
    }
    let spid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[spid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(spid).map(|p| p.kill()).unwrap_or(false)
}

fn parse_process_as_server(proc_info: &sysinfo::Process) -> Option<ParsedServerCmdline> {
    let exe_or_name = proc_info
        .exe()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| proc_info.name().to_string_lossy().into_owned());
    let args: Vec<String> = proc_info
        .cmd()
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    parse_server_cmdline(&exe_or_name, &args)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedServerCmdline {
    edition: GameEdition,
    port: Option<u16>,
    config_path: Option<PathBuf>,
}

fn file_name_from_arg(raw: &str) -> &str {
    let trimmed = raw.trim().trim_matches('"');
    trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed)
}

fn is_shell_binary(raw: &str) -> bool {
    let name = file_name_from_arg(raw);
    [
        "cmd",
        "cmd.exe",
        "powershell",
        "powershell.exe",
        "pwsh",
        "pwsh.exe",
        "bash",
        "sh",
        "zsh",
        "conhost",
        "conhost.exe",
    ]
    .iter()
    .any(|shell| name.eq_ignore_ascii_case(shell))
}

fn is_vanilla_server_binary(raw: &str) -> bool {
    let name = file_name_from_arg(raw);
    name.eq_ignore_ascii_case("terrariaserver.exe") || name.eq_ignore_ascii_case("terrariaserver")
}

fn is_tmodloader_dll_arg(raw: &str) -> bool {
    file_name_from_arg(raw).eq_ignore_ascii_case("tmodloader.dll")
}

fn extract_cli_options(args: &[String]) -> (Option<u16>, Option<PathBuf>) {
    let mut port: Option<u16> = None;
    let mut config_path: Option<PathBuf> = None;
    let mut i = 0;

    while i < args.len() {
        let arg = args[i].trim();
        if arg.eq_ignore_ascii_case("-config") {
            if let Some(next) = args.get(i + 1) {
                let cleaned = next.trim().trim_matches('"');
                if !cleaned.is_empty() && !cleaned.starts_with('-') {
                    config_path = Some(PathBuf::from(cleaned));
                    i += 2;
                    continue;
                }
            }
        } else if arg.eq_ignore_ascii_case("-port") {
            if let Some(next) = args.get(i + 1) {
                let cleaned = next.trim().trim_matches('"');
                if !cleaned.starts_with('-') {
                    if let Ok(parsed) = cleaned.parse::<u16>() {
                        if parsed > 0 {
                            port = Some(parsed);
                        }
                    }
                    i += 2;
                    continue;
                }
            }
        } else if let Some((flag, value)) = arg.split_once('=') {
            let flag = flag.trim();
            let cleaned = value.trim().trim_matches('"');
            if flag.eq_ignore_ascii_case("-config") {
                if !cleaned.is_empty() {
                    config_path = Some(PathBuf::from(cleaned));
                }
            } else if flag.eq_ignore_ascii_case("-port") {
                if let Ok(parsed) = cleaned.parse::<u16>() {
                    if parsed > 0 {
                        port = Some(parsed);
                    }
                }
            }
        }
        i += 1;
    }

    (port, config_path)
}

fn parse_server_cmdline(exe_or_name: &str, args: &[String]) -> Option<ParsedServerCmdline> {
    if is_shell_binary(exe_or_name) || args.first().is_some_and(|a| is_shell_binary(a)) {
        return None;
    }

    let has_tml_dll = args.iter().any(|a| is_tmodloader_dll_arg(a));
    let has_server_flag = args
        .iter()
        .any(|a| a.trim().eq_ignore_ascii_case("-server"));

    let edition = if has_tml_dll && has_server_flag {
        GameEdition::TModLoader
    } else if is_vanilla_server_binary(exe_or_name)
        || args.first().is_some_and(|a| is_vanilla_server_binary(a))
    {
        GameEdition::Vanilla
    } else {
        return None;
    };

    let (port, config_path) = extract_cli_options(args);
    Some(ParsedServerCmdline {
        edition,
        port,
        config_path,
    })
}

fn parse_port_from_serverconfig(text: &str) -> Option<u16> {
    let mut port = None;
    for raw_line in text.lines() {
        let line = raw_line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            if key.trim().eq_ignore_ascii_case("port") {
                if let Ok(parsed) = val.trim().parse::<u16>() {
                    if parsed > 0 {
                        port = Some(parsed);
                    }
                }
            }
        }
    }
    port
}

fn clean_lexical_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    let _ = out.pop();
                } else if !out.has_root() {
                    out.push(comp.as_os_str());
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

fn normalize_path_for_compare(path: &Path) -> String {
    let canonical = fs::canonicalize(path);
    let target = canonical.as_deref().unwrap_or(path);
    let cleaned = clean_lexical_path(target);
    let mut parts = Vec::new();
    for comp in cleaned.components() {
        match comp {
            Component::Prefix(prefix) => {
                let s = match prefix.kind() {
                    Prefix::VerbatimDisk(d) | Prefix::Disk(d) => {
                        format!("{}:", (d as char).to_ascii_lowercase())
                    }
                    Prefix::VerbatimUNC(server, share) | Prefix::UNC(server, share) => {
                        format!(
                            "\\\\{}\\{}",
                            server.to_string_lossy().to_lowercase(),
                            share.to_string_lossy().to_lowercase()
                        )
                    }
                    Prefix::Verbatim(v) => v.to_string_lossy().to_lowercase(),
                    Prefix::DeviceNS(d) => {
                        format!("\\\\.\\{}", d.to_string_lossy().to_lowercase())
                    }
                };
                parts.push(s);
            }
            Component::RootDir => parts.push(String::new()),
            Component::CurDir => {}
            Component::ParentDir => parts.push("..".to_string()),
            Component::Normal(seg) => {
                let text = seg.to_string_lossy();
                if cfg!(windows) {
                    parts.push(text.to_lowercase());
                } else {
                    parts.push(text.into_owned());
                }
            }
        }
    }
    parts.join("/")
}

fn extract_profile_id_from_config_path(
    config_path: &Path,
    paths: &LauncherPaths,
) -> Option<String> {
    let cleaned = clean_lexical_path(config_path);
    let parent_dir = cleaned.parent()?;
    let servers_parent = parent_dir.parent()?;
    if normalize_path_for_compare(servers_parent) != normalize_path_for_compare(&paths.servers_dir) {
        return None;
    }
    let name = parent_dir.file_name()?.to_string_lossy().trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

pub fn normalize_ip_addr(ip: IpAddr) -> IpAddr {
    ip.to_canonical()
}

pub fn extract_ban_ip_key(raw: &str) -> Option<IpAddr> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(sock) = trimmed.parse::<SocketAddr>() {
        return Some(normalize_ip_addr(sock.ip()));
    }
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return Some(normalize_ip_addr(ip));
    }
    None
}

pub fn is_loopback_ban_target(raw: &str) -> bool {
    raw.trim().eq_ignore_ascii_case("localhost")
        || extract_ban_ip_key(raw).is_some_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

pub fn load_banlist(banlist_path: &Path) -> Vec<BanEntry> {
    let Ok(text) = fs::read_to_string(banlist_path) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    let mut last_comment = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix("//") {
            last_comment = comment.trim().to_string();
        } else {
            entries.push(BanEntry {
                label: if last_comment.is_empty() {
                    "Người chơi bị cấm".to_string()
                } else {
                    std::mem::take(&mut last_comment)
                },
                ip: line.to_string(),
            });
        }
    }
    entries
}

pub fn save_banlist(banlist_path: &Path, entries: &[BanEntry]) -> LauncherResult<()> {
    if let Some(parent) = banlist_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut content = String::new();
    for entry in entries {
        if !entry.label.trim().is_empty() {
            content.push_str(&format!("//{}\r\n", entry.label.trim()));
        }
        content.push_str(&format!("{}\r\n", entry.ip.trim()));
    }
    fs::write(banlist_path, content)?;
    Ok(())
}

pub mod egui_repaint {
    use std::sync::Arc;

    #[derive(Clone, Default)]
    pub struct RepaintHandle {
        cb: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    impl RepaintHandle {
        pub fn new(cb: Arc<dyn Fn() + Send + Sync>) -> Self {
            Self { cb: Some(cb) }
        }

        pub fn wake(&self) {
            if let Some(cb) = &self.cb {
                cb();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDirGuard(PathBuf);

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn make_temp_paths(prefix: &str) -> (TempDirGuard, LauncherPaths) {
        let base = std::env::temp_dir().join(format!("{prefix}_{}", uuid::Uuid::new_v4()));
        let terraria_dir = base.join("Terraria");
        let tmodloader_dir = base.join("tModLoader");
        let data_dir = base.join("data");
        let saves_dir = data_dir.join("Saves");
        let paths = LauncherPaths {
            base_dir: base.clone(),
            terraria_dir,
            tmodloader_dir,
            vanilla_saves_dir: saves_dir.join("Vanilla"),
            tml_saves_dir: saves_dir.join("tModLoader"),
            servers_dir: data_dir.join("Servers"),
            backups_dir: data_dir.join("Backups"),
            steamcmd_dir: data_dir.join("SteamCMD"),
            config_path: data_dir.join("config.json"),
            data_dir,
        };
        paths.ensure_dirs().unwrap();
        (TempDirGuard(base), paths)
    }

    #[test]
    fn detects_tmodloader_server_and_extracts_profile_and_port() {
        let (_guard, paths) = make_temp_paths("tl_srv_tml");
        let profile_dir = paths.servers_dir.join("my-tml-profile");
        fs::create_dir_all(&profile_dir).unwrap();
        let config_path = profile_dir.join("serverconfig.txt");
        fs::write(&config_path, "worldname=Test\r\nport=7779\r\n").unwrap();

        let args = vec![
            r"C:\TerrariaLauncher\tModLoader\dotnet\dotnet.exe".to_string(),
            r"C:\TerrariaLauncher\tModLoader\tModLoader.dll".to_string(),
            "-server".to_string(),
            "-nosteam".to_string(),
            "-config".to_string(),
            config_path.to_string_lossy().into_owned(),
        ];

        let parsed = parse_server_cmdline("dotnet.exe", &args).expect("should detect tml server");
        assert_eq!(parsed.edition, GameEdition::TModLoader);
        assert_eq!(parsed.port, None);
        assert_eq!(parsed.config_path.as_deref(), Some(config_path.as_path()));

        let resolved_port = parsed.port.or_else(|| {
            parsed
                .config_path
                .as_deref()
                .and_then(|p| fs::read_to_string(p).ok())
                .and_then(|text| parse_port_from_serverconfig(&text))
        });
        assert_eq!(resolved_port, Some(7779));
        assert_eq!(
            extract_profile_id_from_config_path(parsed.config_path.as_deref().unwrap(), &paths)
                .as_deref(),
            Some("my-tml-profile")
        );
    }

    #[test]
    fn rejects_tmodloader_client_without_server_flag() {
        let args = vec![
            r"C:\TerrariaLauncher\tModLoader\dotnet\dotnet.exe".to_string(),
            r"C:\TerrariaLauncher\tModLoader\tModLoader.dll".to_string(),
            "-tmlsavedirectory".to_string(),
            r"C:\TerrariaLauncher\data\Saves\tModLoader".to_string(),
        ];

        assert_eq!(parse_server_cmdline("dotnet.exe", &args), None);
    }

    #[test]
    fn rejects_non_server_and_shell_processes() {
        let vanilla_client = vec![r"C:\TerrariaLauncher\Terraria\Terraria.exe".to_string()];
        assert_eq!(parse_server_cmdline("Terraria.exe", &vanilla_client), None);

        let wrapped_shell = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "dotnet".to_string(),
            r"C:\TerrariaLauncher\tModLoader\tModLoader.dll".to_string(),
            "-server".to_string(),
        ];
        assert_eq!(parse_server_cmdline("cmd.exe", &wrapped_shell), None);
    }

    #[test]
    fn detects_vanilla_server_with_cli_port_override() {
        let (_guard, paths) = make_temp_paths("tl_srv_vanilla");
        let profile_dir = paths.servers_dir.join("vanilla-profile");
        fs::create_dir_all(&profile_dir).unwrap();
        let config_path = profile_dir.join("serverconfig.txt");
        fs::write(&config_path, "port=7777\r\n").unwrap();

        let args = vec![
            r"C:\TerrariaLauncher\Terraria\TerrariaServer.exe".to_string(),
            "-PORT".to_string(),
            "7778".to_string(),
            "-CONFIG".to_string(),
            config_path.to_string_lossy().into_owned(),
        ];

        let parsed =
            parse_server_cmdline("TerrariaServer.exe", &args).expect("should detect vanilla");
        assert_eq!(parsed.edition, GameEdition::Vanilla);
        assert_eq!(parsed.port, Some(7778));
        assert_eq!(
            extract_profile_id_from_config_path(parsed.config_path.as_deref().unwrap(), &paths)
                .as_deref(),
            Some("vanilla-profile")
        );

        let eq_args = vec!["-port=7781".to_string()];
        let eq_parsed =
            parse_server_cmdline("terrariaserver", &eq_args).expect("should detect -port=");
        assert_eq!(eq_parsed.edition, GameEdition::Vanilla);
        assert_eq!(eq_parsed.port, Some(7781));
    }

    #[test]
    fn parses_port_from_serverconfig_text_and_checks_profile_parent() {
        let text = "\u{feff}# server config\r\n# port=9999\r\nworldname=Example\r\nport = 7777\r\n";
        assert_eq!(parse_port_from_serverconfig(text), Some(7777));
        assert_eq!(parse_port_from_serverconfig("worldname=NoPort\r\n"), None);
        assert_eq!(parse_port_from_serverconfig("port=0\r\n"), None);

        let (_guard, paths) = make_temp_paths("tl_srv_profile_case");
        let mixed_case_cfg = PathBuf::from(
            paths
                .servers_dir
                .to_string_lossy()
                .replace('\\', "/")
                .to_uppercase(),
        )
        .join("MyProfile")
        .join("serverconfig.txt");
        assert_eq!(
            extract_profile_id_from_config_path(&mixed_case_cfg, &paths).as_deref(),
            Some("MyProfile")
        );

        let nested_cfg = paths
            .servers_dir
            .join("MyProfile")
            .join("nested")
            .join("serverconfig.txt");
        assert_eq!(extract_profile_id_from_config_path(&nested_cfg, &paths), None);
    }

    #[cfg(windows)]
    #[test]
    fn kill_on_close_job_creates_and_drops_cleanly() {
        let job = KillOnCloseJob::new().expect("should create kill-on-close job object");
        assert!(!HANDLE(job.0 as *mut _).is_invalid());
        drop(job);
    }

    #[test]
    fn summarize_server_log_lines_resets_session_and_tracks_players() {
        let lines = vec![
            ConsoleLogLine {
                timestamp: "10:00:00".to_string(),
                is_error: false,
                text: "Listening on port 7777".to_string(),
            },
            ConsoleLogLine {
                timestamp: "10:00:05".to_string(),
                is_error: false,
                text: "StalePlayer has joined.".to_string(),
            },
            ConsoleLogLine {
                timestamp: "10:01:00".to_string(),
                is_error: true,
                text: "[Launcher] Tiến trình server đã kết thúc (Mã thoát: Some(1)).".to_string(),
            },
            ConsoleLogLine {
                timestamp: "10:01:03".to_string(),
                is_error: false,
                text: "Finding Mods...".to_string(),
            },
            ConsoleLogLine {
                timestamp: "10:01:04".to_string(),
                is_error: false,
                text: "Running engine preload...".to_string(),
            },
            ConsoleLogLine {
                timestamp: "10:01:05".to_string(),
                is_error: false,
                text: "Terraria Server v1.4.4.9".to_string(),
            },
            ConsoleLogLine {
                timestamp: "10:01:06".to_string(),
                is_error: false,
                text: "Loading world data: 50%".to_string(),
            },
            ConsoleLogLine {
                timestamp: "10:01:10".to_string(),
                is_error: false,
                text: "Listening on port 7778".to_string(),
            },
            ConsoleLogLine {
                timestamp: "10:01:11".to_string(),
                is_error: false,
                text: "Server started".to_string(),
            },
            ConsoleLogLine {
                timestamp: "10:01:20".to_string(),
                is_error: false,
                text: "Alice has joined.".to_string(),
            },
            ConsoleLogLine {
                timestamp: "10:01:25".to_string(),
                is_error: false,
                text: "Bob has joined.".to_string(),
            },
            ConsoleLogLine {
                timestamp: "10:01:30".to_string(),
                is_error: false,
                text: "Alice has left.".to_string(),
            },
        ];

        let summary = summarize_server_log_lines(&lines);
        assert!(summary.is_listening);
        assert_eq!(summary.status_detail, "Server started");
        assert_eq!(summary.lines.len(), lines.len());
        assert_eq!(summary.players.len(), 1);
        assert_eq!(summary.players[0].name, "Bob");
        assert_eq!(summary.players[0].ip, None);
        assert_eq!(summary.players[0].joined_at, "10:01:25");

        let reset_only = summarize_server_log_lines(&lines[..3]);
        assert!(!reset_only.is_listening);
        assert!(reset_only.players.is_empty());
        assert!(reset_only.status_detail.is_empty());

        let loading_only = summarize_server_log_lines(&lines[..7]);
        assert!(!loading_only.is_listening);
        assert!(loading_only.players.is_empty());
        assert_eq!(loading_only.status_detail, "Loading world data: 50%");
    }

    #[test]
    fn read_server_log_summary_handles_large_tail_offset_and_line_parsing() {
        let (_guard, paths) = make_temp_paths("tl_srv_log_tail");
        let missing = read_server_log_summary(&paths, "missing-profile", 50);
        assert!(missing.lines.is_empty());
        assert!(!missing.is_listening);

        let profile_dir = paths.servers_dir.join("large-log-profile");
        fs::create_dir_all(&profile_dir).unwrap();
        let log_path = profile_dir.join("server.log");

        let mut content = String::new();
        content.push_str("[08:00:00] Listening on port 7777\r\n");
        content.push_str("[08:00:01] OldPlayer has joined.\r\n");
        while content.len() < 150 * 1024 {
            content.push_str("[08:00:02] Đang tải dữ liệu bản đồ UTF-8 Tiếng Việt...\r\n");
        }
        content.push_str("[08:05:00] Terraria Server v1.4.5.8\r\n");
        content.push_str("[08:05:01] Settling liquids 75%\r\n");
        content.push_str("[08:05:02] Listening on port 7779\r\n");
        content.push_str("[08:05:03] Charlie has joined.\r\n");
        content.push_str("[08:05:04] Error: minor warning test\r\n");
        content.push_str("[Launcher] Lỗi không có mốc thời gian\r\n");
        fs::write(&log_path, content.as_bytes()).unwrap();

        let summary = read_server_log_summary(&paths, "large-log-profile", 6);
        assert_eq!(summary.lines.len(), 6);
        assert!(summary.is_listening);
        assert_eq!(summary.status_detail, "Listening on port 7779");
        assert_eq!(summary.players.len(), 1);
        assert_eq!(summary.players[0].name, "Charlie");
        assert_eq!(summary.players[0].joined_at, "08:05:03");
        assert_eq!(summary.lines[0].timestamp, "08:05:00");
        assert!(!summary.lines[0].is_error);
        assert!(summary.lines[4].is_error);
        assert!(summary.lines[5].timestamp.is_empty());
        assert!(summary.lines[5].is_error);
        assert_eq!(summary.lines[5].text, "[Launcher] Lỗi không có mốc thời gian");

        let min_one = read_server_log_summary(&paths, "large-log-profile", 0);
        assert_eq!(min_one.lines.len(), 1);
    }

    #[test]
    fn encode_server_stdin_line_matches_platform_console_encoding() {
        let encoded = encode_server_stdin_line("exit");
        #[cfg(windows)]
        {
            assert_eq!(
                encoded,
                vec![b'e', 0, b'x', 0, b'i', 0, b't', 0, b'\r', 0, b'\n', 0]
            );
        }
        #[cfg(not(windows))]
        {
            assert_eq!(encoded, b"exit\r\n");
        }
    }

    #[test]
    fn stopping_guard_and_graceful_stop_escalation_work_cleanly() {
        let (_guard, paths) = make_temp_paths("tl_stop_guard");
        let profile = ServerProfile::default();
        let sup = ServerSupervisor::new(&paths, &profile);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let repaint = egui_repaint::RepaintHandle::default();
            *sup.async_ctx.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((tokio::runtime::Handle::current(), repaint));
            let (tx, mut rx) = mpsc::unbounded_channel::<String>();
            *sup.stdin_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
            {
                let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
                guard.snapshot.state = ServerLifecycleState::Running;
            }
            sup.request_graceful_stop();
            assert!(sup.is_stopping());
            assert_eq!(rx.try_recv().ok().as_deref(), Some("exit"));
            assert!(sup.has_stdin());

            sup.request_graceful_stop();
            assert!(sup.is_stopping());
            assert!(
                sup.has_stdin(),
                "repeat request_graceful_stop must stay in Stopping without force_kill"
            );
            assert!(rx.try_recv().is_err());

            handle_server_output_line(&sup, false, "Listening on port 7782");
            assert!(sup.is_stopping());
            let mut sys = System::new().unwrap();
            assert_eq!(sup.snapshot(&mut sys).port, 7782);

            handle_server_output_line(&sup, false, ": Saving before exit...");
            assert!(sup.is_stopping());

            sup.force_kill();
            assert!(!sup.has_stdin());
            assert_eq!(
                sup.snapshot(&mut sys).status_detail,
                "Đang buộc dừng tiến trình server..."
            );
        });
    }

    #[test]
    fn snapshot_tracks_effective_join_metadata_and_auto_restart_cancels_on_stop() {
        let (_guard, paths) = make_temp_paths("tl_join_meta_restart");
        let mut tml_profile = ServerProfile {
            edition: GameEdition::TModLoader,
            ..Default::default()
        };
        tml_profile.config.password = "secret-pass".to_string();
        tml_profile.config.modpack = "  CalamityPack  ".to_string();

        let sup_tml = ServerSupervisor::new(&paths, &tml_profile);
        let mut sys = System::new().unwrap();
        let snap_tml = sup_tml.snapshot(&mut sys);
        assert_eq!(snap_tml.effective_password, "secret-pass");
        assert_eq!(snap_tml.effective_modpack, "CalamityPack");

        let mut vanilla_profile = ServerProfile {
            edition: GameEdition::Vanilla,
            ..Default::default()
        };
        vanilla_profile.config.password = "vanilla-pass".to_string();
        vanilla_profile.config.modpack = "ShouldBeCleared".to_string();
        let sup_vanilla = ServerSupervisor::new(&paths, &vanilla_profile);
        let snap_vanilla = sup_vanilla.snapshot(&mut sys);
        assert_eq!(snap_vanilla.effective_password, "vanilla-pass");
        assert!(snap_vanilla.effective_modpack.is_empty());

        handle_server_output_line(&sup_vanilla, false, ": Listening on port 7791");
        assert_eq!(sup_vanilla.snapshot(&mut sys).port, 7791);
        handle_server_output_line(&sup_vanilla, false, "Listening on port 0");
        assert_eq!(sup_vanilla.snapshot(&mut sys).port, 7791);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let repaint = egui_repaint::RepaintHandle::default();
            let sup_wait = sup_tml.clone();
            let task = tokio::spawn(async move {
                wait_for_auto_restart_or_stop(&sup_wait, &repaint).await
            });
            tokio::time::sleep(Duration::from_millis(25)).await;
            sup_tml.request_graceful_stop();
            let should_restart = tokio::time::timeout(Duration::from_millis(500), task)
                .await
                .expect("auto-restart wait must abort promptly on stop")
                .unwrap();
            assert!(!should_restart);
            assert!(sup_tml.stop_requested.load(Ordering::SeqCst));
            let snap = sup_tml.snapshot(&mut sys);
            assert_eq!(snap.state, ServerLifecycleState::Stopped);
            assert_eq!(snap.pid, None);
            assert!(snap.players.is_empty());
            assert_eq!(snap.status_detail, "Đã dừng theo yêu cầu");
        });
    }

    #[cfg(windows)]
    #[test]
    fn named_single_instance_mutex_blocks_duplicate_until_dropped() {
        let wide_name: Vec<u16> = format!("Local\\TerrariaLauncher_Test_{}\0", uuid::Uuid::new_v4())
            .encode_utf16()
            .collect();
        let name_ptr = PCWSTR::from_raw(wide_name.as_ptr());

        let first = acquire_named_single_instance_guard(name_ptr)
            .expect("first mutex creation must succeed")
            .expect("first guard must be acquired");
        let second = acquire_named_single_instance_guard(name_ptr)
            .expect("second check must succeed");
        assert!(second.is_none(), "duplicate mutex acquisition must return None");

        drop(first);
        let third = acquire_named_single_instance_guard(name_ptr)
            .expect("re-acquisition after drop must succeed");
        assert!(third.is_some(), "mutex must be acquirable after guard drop");
    }

    #[cfg(windows)]
    #[test]
    fn unresponsive_child_in_starting_state_escalates_and_terminates_cleanly() {
        let (_guard, paths) = make_temp_paths("tl_stop_escalate");
        let profile = ServerProfile::default();
        let sup = ServerSupervisor::new(&paths, &profile);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let repaint = egui_repaint::RepaintHandle::default();
            *sup.async_ctx.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((tokio::runtime::Handle::current(), repaint));

            let mut cmd = Command::new("ping");
            cmd.args(["-n", "20", "127.0.0.1"]);
            cmd.creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let mut child = cmd.spawn().unwrap();
            let pid = child.id().expect("child pid");
            *sup.child_pid.lock().unwrap_or_else(|e| e.into_inner()) = Some(pid);
            sup.run_generation.fetch_add(1, Ordering::SeqCst);

            let (tx, rx) = mpsc::unbounded_channel::<String>();
            *sup.stdin_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
            let stdin = child.stdin.take().unwrap();
            tokio::spawn(stdin_writer_task(stdin, rx));

            {
                let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
                guard.snapshot.pid = Some(pid);
                guard.snapshot.state = ServerLifecycleState::Starting;
            }

            let wait_handle = {
                let s = sup.clone();
                tokio::spawn(async move {
                    let _ = child.wait().await;
                    *s.stdin_tx.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    *s.child_pid.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    {
                        let mut guard = s.shared.write().unwrap_or_else(|e| e.into_inner());
                        guard.snapshot.pid = None;
                        guard.snapshot.state = ServerLifecycleState::Stopped;
                    }
                    s.exit_notify.notify_waiters();
                })
            };

            sup.request_graceful_stop();
            assert!(sup.is_stopping());
            tokio::time::timeout(Duration::from_secs(5), wait_handle)
                .await
                .expect("watchdog should escalate and terminate within 5s")
                .unwrap();
            assert!(!sup.is_active());
        });
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires local TerrariaServer.exe installation"]
    fn live_vanilla_server_start_and_graceful_stop_roundtrip() {
        let terraria_dir = PathBuf::from(r"C:\TerrariaLauncher\Terraria");
        let sample_world = PathBuf::from(r"C:\TerrariaLauncher\data\Saves\Vanilla\Worlds\NewWorld.wld");
        if !terraria_dir.join("TerrariaServer.exe").is_file() || !sample_world.is_file() {
            return;
        }
        let (_guard, mut paths) = make_temp_paths("tl_live_srv_stop");
        paths.terraria_dir = terraria_dir;
        fs::create_dir_all(paths.vanilla_worlds_dir()).unwrap();
        let test_world = paths.vanilla_worlds_dir().join("StopSmokeWorld.wld");
        fs::copy(&sample_world, &test_world).unwrap();

        let mut profile = ServerProfile {
            id: "smoke-stop".to_string(),
            edition: GameEdition::Vanilla,
            auto_restart: false,
            ..Default::default()
        };
        profile.config.world = test_world.to_string_lossy().into_owned();
        profile.config.worldpath = paths.vanilla_worlds_dir().to_string_lossy().into_owned();
        profile.config.port = 7798;
        profile.config.upnp = false;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let sup = start_server(
            rt.handle(),
            paths,
            profile,
            RuntimeTuning::default(),
            egui_repaint::RepaintHandle::default(),
        )
        .unwrap();

        let mut sys = System::new().unwrap();
        let ready_deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < ready_deadline {
            if matches!(sup.snapshot(&mut sys).state, ServerLifecycleState::Running) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(sup.snapshot(&mut sys).state, ServerLifecycleState::Running);

        sup.request_graceful_stop();
        let exited = sup.wait_for_exit_timeout(Duration::from_secs(10));
        assert!(exited, "TerrariaServer.exe should save and exit within 10s");
        assert_eq!(sup.snapshot(&mut sys).state, ServerLifecycleState::Stopped);
        let logs = sup.logs();
        assert!(
            logs.iter().any(|l| l.text.contains("Saving before exit")),
            "expected Saving before exit log: {logs:?}"
        );
        assert!(
            logs.iter().any(|l| l.text.contains("Mã thoát: Some(0)")),
            "expected clean exit code 0: {logs:?}"
        );
    }

    fn write_test_tmod(path: &Path, internal_name: &str) {
        let push_str = |buf: &mut Vec<u8>, s: &str| {
            buf.push(s.len() as u8);
            buf.extend_from_slice(s.as_bytes());
        };
        let mut buf = Vec::new();
        buf.extend_from_slice(b"TMOD");
        push_str(&mut buf, "2026.9.3.1");
        buf.extend_from_slice(&[0x22u8; 20]);
        buf.extend_from_slice(&[0u8; 256]);
        buf.extend_from_slice(&64i32.to_le_bytes());
        push_str(&mut buf, internal_name);
        push_str(&mut buf, "1.0.0");
        buf.extend_from_slice(&0i32.to_le_bytes());
        fs::write(path, &buf).unwrap();
    }

    #[test]
    fn parses_colon_prefixed_player_events_playing_lines_and_choose_world() {
        assert_eq!(
            parse_playing_player_line("trihienkun (127.0.0.1:60972)"),
            Some(("trihienkun", "127.0.0.1:60972"))
        );
        assert_eq!(
            parse_playing_player_line(": trihienkun (127.0.0.1:60972)"),
            Some(("trihienkun", "127.0.0.1:60972"))
        );
        assert_eq!(
            parse_playing_player_line(": : alice ([::1]:7777)"),
            Some(("alice", "[::1]:7777"))
        );
        assert_eq!(
            parse_playing_player_line("bob (2405:4802::1)"),
            Some(("bob", "2405:4802::1"))
        );
        assert_eq!(
            parse_playing_player_line("<trihienkun> test (127.0.0.1:60972)"),
            None
        );
        assert_eq!(
            parse_playing_player_line("[Server] test (127.0.0.1:60972)"),
            None
        );
        assert_eq!(
            parse_playing_player_line("> cmd (127.0.0.1:60972)"),
            None
        );
        assert_eq!(
            parse_playing_player_line("trihienkun (not-an-ip)"),
            None
        );

        let lines = vec![
            ConsoleLogLine {
                timestamp: "12:00:00".to_string(),
                is_error: false,
                text: ": Choose World: ".to_string(),
            },
            ConsoleLogLine {
                timestamp: "12:00:05".to_string(),
                is_error: false,
                text: ": Listening on port 7777".to_string(),
            },
            ConsoleLogLine {
                timestamp: "12:00:10".to_string(),
                is_error: false,
                text: ": trihienkun has joined.".to_string(),
            },
            ConsoleLogLine {
                timestamp: "12:00:11".to_string(),
                is_error: false,
                text: "trihienkun (127.0.0.1:60972)".to_string(),
            },
            ConsoleLogLine {
                timestamp: "12:00:12".to_string(),
                is_error: false,
                text: "<trihienkun> test (10.0.0.5:9999)".to_string(),
            },
        ];
        let joined_summary = summarize_server_log_lines(&lines);
        assert!(joined_summary.is_listening);
        assert_eq!(joined_summary.status_detail, "Listening on port 7777");
        assert_eq!(joined_summary.players.len(), 1);
        assert_eq!(joined_summary.players[0].name, "trihienkun");
        assert_eq!(
            joined_summary.players[0].ip.as_deref(),
            Some("127.0.0.1:60972")
        );

        let mut choose_world_lines = lines[..1].to_vec();
        let choose_summary = summarize_server_log_lines(&choose_world_lines);
        assert!(!choose_summary.is_listening);
        assert_eq!(
            choose_summary.status_detail,
            "Đang chờ chọn World (nhập số thứ tự ở Console hoặc bật tự tạo)"
        );

        choose_world_lines = lines;
        choose_world_lines.push(ConsoleLogLine {
            timestamp: "12:00:20".to_string(),
            is_error: false,
            text: ": trihienkun has left.".to_string(),
        });
        let left_summary = summarize_server_log_lines(&choose_world_lines);
        assert!(left_summary.players.is_empty());
    }

    #[test]
    fn supervisor_silent_playing_and_exit_command_behavior() {
        let (_guard, paths) = make_temp_paths("tl_sup_cmd_test");
        let profile = ServerProfile::default();
        let sup = ServerSupervisor::new(&paths, &profile);
        assert!(!sup.has_stdin());

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        *sup.stdin_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        assert!(sup.has_stdin());

        {
            let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
            guard.snapshot.state = ServerLifecycleState::Running;
        }

        handle_server_output_line(&sup, false, ": trihienkun has joined.");
        assert_eq!(rx.try_recv().ok().as_deref(), Some("playing"));
        assert!(!sup.logs().iter().any(|l| l.text == "> playing"));

        sup.send_silent_command("version").unwrap();
        assert_eq!(rx.try_recv().ok().as_deref(), Some("version"));
        assert!(!sup.logs().iter().any(|l| l.text == "> version"));
        assert!(!sup.stop_requested.load(Ordering::SeqCst));

        handle_server_output_line(&sup, false, ": trihienkun (127.0.0.1:60972)");
        let mut sys = System::new().unwrap();
        let snap = sup.snapshot(&mut sys);
        assert_eq!(snap.players.len(), 1);
        assert_eq!(snap.players[0].name, "trihienkun");
        assert_eq!(snap.players[0].ip.as_deref(), Some("127.0.0.1:60972"));

        sup.send_command("exit").unwrap();
        assert_eq!(rx.try_recv().ok().as_deref(), Some("exit"));
        assert!(sup.stop_requested.load(Ordering::SeqCst));
        assert!(sup.is_stopping());
        assert_eq!(
            sup.snapshot(&mut sys).status_detail,
            "Đang lưu thế giới và dừng server..."
        );

        let sup_nosave = ServerSupervisor::new(&paths, &profile);
        let (tx2, mut rx2) = mpsc::unbounded_channel::<String>();
        *sup_nosave
            .stdin_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(tx2);
        sup_nosave.send_command("EXIT-NOSAVE").unwrap();
        assert_eq!(rx2.try_recv().ok().as_deref(), Some("EXIT-NOSAVE"));
        assert!(sup_nosave.stop_requested.load(Ordering::SeqCst));
        assert!(sup_nosave.is_stopping());
        assert_eq!(
            sup_nosave.snapshot(&mut sys).status_detail,
            "Đang dừng server..."
        );

        assert!(is_clean_server_exit(true, &ServerLifecycleState::Starting, false));
        assert!(!is_clean_server_exit(false, &ServerLifecycleState::Starting, true));
        assert!(is_clean_server_exit(false, &ServerLifecycleState::Running, true));
        assert!(!is_clean_server_exit(false, &ServerLifecycleState::Running, false));
        assert!(is_clean_server_exit(false, &ServerLifecycleState::Stopping, true));
    }

    #[test]
    fn resolve_tmodloader_server_modpack_config_handles_folder_and_legacy_packs() {
        let (_guard, paths) = make_temp_paths("tl_srv_modpack_cfg");
        let mods_dir = paths.tml_mods_dir();
        write_test_tmod(&mods_dir.join("CalamityMod.tmod"), "CalamityMod");
        write_test_tmod(&mods_dir.join("UnusedMod.tmod"), "UnusedMod");

        let pack_mods = paths.tml_modpacks_dir().join("FolderPack").join("Mods");
        fs::create_dir_all(&pack_mods).unwrap();
        tl_core::save_enabled_mods(
            &pack_mods.join("enabled.json"),
            &["CalamityMod".to_string()],
        )
        .unwrap();

        let mut cfg = ServerConfigData {
            modpack: "FolderPack".to_string(),
            ..ServerConfigData::default()
        };
        resolve_tmodloader_server_modpack_config(&paths, GameEdition::TModLoader, &mut cfg)
            .unwrap();
        assert_eq!(cfg.modpath, pack_mods.to_string_lossy());
        assert!(cfg.modpack.is_empty());
        assert!(pack_mods.join("CalamityMod.tmod").is_file());
        assert!(!pack_mods.join("UnusedMod.tmod").exists());

        let text = cfg.to_config_text(GameEdition::TModLoader);
        assert!(text.contains(&format!("modpath={}", pack_mods.display())));
        assert!(!text.contains("modpack="));

        fs::write(paths.tml_modpacks_dir().join("LegacyPack.json"), b"{}").unwrap();
        let mut legacy_cfg = ServerConfigData {
            modpack: " LegacyPack ".to_string(),
            ..ServerConfigData::default()
        };
        resolve_tmodloader_server_modpack_config(
            &paths,
            GameEdition::TModLoader,
            &mut legacy_cfg,
        )
        .unwrap();
        assert!(legacy_cfg.modpath.is_empty());
        assert_eq!(legacy_cfg.modpack, "LegacyPack");

        let mut missing_cfg = ServerConfigData {
            modpack: "NonExistentPack".to_string(),
            ..ServerConfigData::default()
        };
        resolve_tmodloader_server_modpack_config(
            &paths,
            GameEdition::TModLoader,
            &mut missing_cfg,
        )
        .unwrap();
        assert!(missing_cfg.modpack.is_empty());

        let mut traversal_cfg = ServerConfigData {
            modpack: "../escape".to_string(),
            ..ServerConfigData::default()
        };
        assert!(matches!(
            resolve_tmodloader_server_modpack_config(
                &paths,
                GameEdition::TModLoader,
                &mut traversal_cfg
            ),
            Err(LauncherError::InvalidInput(_))
        ));
    }

    #[test]
    fn ban_ip_normalization_and_loopback_guards_cover_ipv4_ipv6_and_localhost() {
        assert_eq!(
            extract_ban_ip_key("127.0.0.1:60972"),
            Some("127.0.0.1".parse().unwrap())
        );
        assert_eq!(
            extract_ban_ip_key("[::1]:7777"),
            Some("::1".parse().unwrap())
        );
        assert_eq!(
            extract_ban_ip_key("::ffff:127.0.0.1"),
            Some("127.0.0.1".parse().unwrap())
        );
        assert_eq!(
            extract_ban_ip_key("2405:4802::1"),
            Some("2405:4802::1".parse().unwrap())
        );
        assert_eq!(
            extract_ban_ip_key("192.168.1.5"),
            Some("192.168.1.5".parse().unwrap())
        );
        assert_eq!(extract_ban_ip_key("not-an-ip"), None);

        assert!(is_loopback_ban_target("127.0.0.1:60972"));
        assert!(is_loopback_ban_target("[::1]:7777"));
        assert!(is_loopback_ban_target("::ffff:127.0.0.1"));
        assert!(is_loopback_ban_target("localhost"));
        assert!(is_loopback_ban_target(" LocalHost "));
        assert!(is_loopback_ban_target("0.0.0.0"));
        assert!(is_loopback_ban_target("::"));

        assert!(!is_loopback_ban_target("2405:4802::1"));
        assert!(!is_loopback_ban_target("192.168.1.5"));
        assert!(!is_loopback_ban_target("192.168.1.5:7777"));
        assert!(!is_loopback_ban_target("invalid"));
    }

    #[test]
    fn colon_and_angle_bracket_player_names_and_chat_spoof_rejection_work() {
        assert_eq!(
            parse_playing_player_line(":D (127.0.0.1:60972)"),
            Some((":D", "127.0.0.1:60972"))
        );
        assert_eq!(
            parse_playing_player_line(": :D ([::1]:7777)"),
            Some((":D", "[::1]:7777"))
        );
        assert_eq!(
            parse_playing_player_line("A>B (192.168.1.10:55000)"),
            Some(("A>B", "192.168.1.10:55000"))
        );
        assert_eq!(
            parse_playing_player_line(": : A>B (2405:4802::1)"),
            Some(("A>B", "2405:4802::1"))
        );
        assert_eq!(
            parse_playing_player_line("> say hi (127.0.0.1:60972)"),
            None
        );

        let lines = vec![
            ConsoleLogLine {
                timestamp: "12:00:00".to_string(),
                is_error: false,
                text: "<Troll> Listening on port 9999".to_string(),
            },
            ConsoleLogLine {
                timestamp: "12:00:01".to_string(),
                is_error: false,
                text: "[Server] Server started".to_string(),
            },
            ConsoleLogLine {
                timestamp: "12:00:02".to_string(),
                is_error: false,
                text: "Some chat said Listening on port 8888".to_string(),
            },
            ConsoleLogLine {
                timestamp: "12:00:03".to_string(),
                is_error: false,
                text: "<Troll> FakePlayer has joined.".to_string(),
            },
            ConsoleLogLine {
                timestamp: "12:00:04".to_string(),
                is_error: false,
                text: ": :D has joined.".to_string(),
            },
            ConsoleLogLine {
                timestamp: "12:00:05".to_string(),
                is_error: false,
                text: ": A>B has joined.".to_string(),
            },
            ConsoleLogLine {
                timestamp: "12:00:06".to_string(),
                is_error: false,
                text: ": :D (127.0.0.1:61000)".to_string(),
            },
            ConsoleLogLine {
                timestamp: "12:00:07".to_string(),
                is_error: false,
                text: ": A>B (2405:4802::2)".to_string(),
            },
            ConsoleLogLine {
                timestamp: "12:00:08".to_string(),
                is_error: false,
                text: "<Troll> :D has left.".to_string(),
            },
        ];

        let summary = summarize_server_log_lines(&lines);
        assert!(!summary.is_listening);
        assert!(summary.status_detail.is_empty());
        assert_eq!(summary.players.len(), 2);
        assert_eq!(summary.players[0].name, ":D");
        assert_eq!(summary.players[0].ip.as_deref(), Some("127.0.0.1:61000"));
        assert_eq!(summary.players[1].name, "A>B");
        assert_eq!(summary.players[1].ip.as_deref(), Some("2405:4802::2"));
    }

    #[test]
    fn prepare_effective_server_config_normalizes_banlist_language_and_guards_missing_world() {
        let (_guard, paths) = make_temp_paths("tl_prepare_srv_cfg");
        let server_dir = paths.servers_dir.join("p1");
        fs::create_dir_all(&server_dir).unwrap();
        let tuning = RuntimeTuning::default();

        let missing_world = paths.vanilla_worlds_dir().join("DoesNotExist.wld");
        let mut profile = ServerProfile {
            id: "p1".to_string(),
            edition: GameEdition::Vanilla,
            ..Default::default()
        };
        profile.config.world = missing_world.to_string_lossy().into_owned();
        profile.config.autocreate = None;

        let err = prepare_effective_server_config(&paths, &profile, &server_dir, tuning)
            .expect_err("missing world without autocreate must error before spawning");
        assert!(matches!(err, LauncherError::InvalidInput(_)));

        let mut empty_world_profile = profile.clone();
        empty_world_profile.config.world.clear();
        empty_world_profile.config.worldname.clear();
        empty_world_profile.config.autocreate = Some(1);
        let err_empty =
            prepare_effective_server_config(&paths, &empty_world_profile, &server_dir, tuning)
                .expect_err("empty world path must error even with autocreate");
        assert!(matches!(err_empty, LauncherError::InvalidInput(_)));

        profile.config.autocreate = Some(2);
        profile.config.language = "   ".to_string();
        profile.config.banlist = "banlist.txt".to_string();
        let effective =
            prepare_effective_server_config(&paths, &profile, &server_dir, tuning).unwrap();
        assert_eq!(effective.language, "en-US");
        assert_eq!(
            Path::new(&effective.banlist),
            server_dir.join("banlist.txt").as_path()
        );
        assert!(Path::new(&effective.banlist).is_file());

        profile.config.banlist = "sub/custom_ban.txt".to_string();
        let effective_rel =
            prepare_effective_server_config(&paths, &profile, &server_dir, tuning).unwrap();
        assert_eq!(
            Path::new(&effective_rel.banlist),
            server_dir.join("custom_ban.txt").as_path()
        );

        let existing_world = paths.vanilla_worlds_dir().join("RealWorld.wld");
        fs::create_dir_all(existing_world.parent().unwrap()).unwrap();
        fs::write(&existing_world, b"WLD").unwrap();
        profile.config.world = existing_world.to_string_lossy().into_owned();
        profile.config.autocreate = None;
        profile.config.banlist.clear();
        let effective_existing =
            prepare_effective_server_config(&paths, &profile, &server_dir, tuning).unwrap();
        assert_eq!(effective_existing.world, existing_world.to_string_lossy());
        assert_eq!(
            Path::new(&effective_existing.banlist),
            server_dir.join("banlist.txt").as_path()
        );
    }

    #[test]
    fn resolve_effective_join_metadata_and_loopback_ban_guard_work() {
        let (_guard, paths) = make_temp_paths("tl_join_meta_ban_guard");
        let mut profile = ServerProfile {
            edition: GameEdition::TModLoader,
            extra_args: r#"-p 7799 -password="cli pass" -modpack "CLI Pack""#.to_string(),
            ..Default::default()
        };
        profile.config.port = 7777;
        profile.config.password = "cfgpass".to_string();
        profile.config.modpack = "CfgPack".to_string();
        profile.config.world = "C:/Worlds/Test.wld".to_string();

        let (port, pass, pack) = resolve_effective_join_metadata(&profile);
        assert_eq!(port, 7799);
        assert_eq!(pass, "cli pass");
        assert_eq!(pack, "CLI Pack");

        let sup = ServerSupervisor::new(&paths, &profile);
        let mut sys = System::new().unwrap();
        let snap = sup.snapshot(&mut sys);
        assert_eq!(snap.port, 7799);
        assert_eq!(snap.effective_world, "C:/Worlds/Test.wld");
        assert_eq!(snap.effective_password, "cli pass");
        assert_eq!(snap.effective_modpack, "CLI Pack");

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        *sup.stdin_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        {
            let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
            guard.snapshot.state = ServerLifecycleState::Running;
            guard.snapshot.players = vec![
                ConnectedPlayer {
                    name: "BridgedLocal".to_string(),
                    ip: Some("127.0.0.1:61234".to_string()),
                    joined_at: "12:00:00".to_string(),
                },
                ConnectedPlayer {
                    name: "RemoteV6".to_string(),
                    ip: Some("2405:4802::1".to_string()),
                    joined_at: "12:00:01".to_string(),
                },
            ];
        }

        let ban_err = sup
            .send_command("ban BridgedLocal")
            .expect_err("interactive ban for 127.0.0.1 player must be blocked");
        assert!(matches!(ban_err, LauncherError::InvalidInput(_)));
        assert!(rx.try_recv().is_err());

        sup.send_command("BAN RemoteV6").unwrap();
        assert_eq!(rx.try_recv().ok().as_deref(), Some("BAN RemoteV6"));
    }

    #[test]
    fn pre_cancelled_supervisor_and_terminate_pid_guard_behave_safely() {
        let (_guard, paths) = make_temp_paths("tl_precancel_pid_guard");
        let profile = ServerProfile::default();
        let sup = ServerSupervisor::new(&paths, &profile);
        sup.request_graceful_stop();
        assert!(sup.stop_requested.load(Ordering::SeqCst));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let sup_clone = sup.clone();
            let paths_clone = paths.clone();
            let profile_clone = profile.clone();
            let config_file = paths.servers_dir.join("never_used.txt");
            let handle = tokio::spawn(async move {
                run_server_supervisor(
                    sup_clone,
                    paths_clone,
                    profile_clone,
                    config_file,
                    RuntimeTuning::default(),
                    egui_repaint::RepaintHandle::default(),
                )
                .await;
            });
            handle.await.unwrap();
        });

        let mut sys = System::new().unwrap();
        let snap = sup.snapshot(&mut sys);
        assert_eq!(snap.state, ServerLifecycleState::Stopped);
        assert_eq!(snap.status_detail, "Đã dừng theo yêu cầu");
        assert!(!terminate_process_by_pid(&mut sys, 0));
        assert!(!terminate_process_by_pid(&mut sys, std::process::id()));
    }
}
