use crate::game_runner::{prepend_native_lib_path, resolve_dotnet_executable, split_shell_args};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tl_core::{
    is_xna_framework_installed, GameEdition, LauncherError, LauncherPaths, LauncherResult,
    ServerProfile,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_CONSOLE_LINES: usize = 2000;

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
    pub state: ServerLifecycleState,
    pub status_detail: String,
    pub pid: Option<u32>,
    pub memory_mb: u64,
    pub cpu_percent: f32,
    pub players: Vec<ConnectedPlayer>,
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
    child_pid: Arc<Mutex<Option<u32>>>,
}

impl ServerSupervisor {
    pub fn new(paths: &LauncherPaths, profile: &ServerProfile) -> Self {
        let log_dir = paths.servers_dir.join(&profile.id);
        let _ = fs::create_dir_all(&log_dir);
        let log_file_path = log_dir.join("server.log");
        Self {
            shared: Arc::new(RwLock::new(ServerSharedState {
                snapshot: ServerRuntimeSnapshot {
                    profile_id: profile.id.clone(),
                    profile_name: profile.name.clone(),
                    edition: profile.edition,
                    port: profile.config.port,
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
            child_pid: Arc::new(Mutex::new(None)),
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

    pub fn send_command(&self, cmd: &str) -> LauncherResult<()> {
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

        push_log_line(&self.shared, false, format!("> {trimmed}"));
        sender
            .send(trimmed.to_string())
            .map_err(|_| LauncherError::Process("Kênh stdin của server đã đóng".to_string()))
    }

    pub fn request_graceful_stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        {
            let mut guard = self.shared.write().unwrap_or_else(|e| e.into_inner());
            if matches!(
                guard.snapshot.state,
                ServerLifecycleState::Running | ServerLifecycleState::Starting
            ) {
                guard.snapshot.state = ServerLifecycleState::Stopping;
                guard.snapshot.status_detail = "Đang lưu thế giới và dừng server...".to_string();
            }
        }
        let _ = self.send_command("save");
        let _ = self.send_command("exit");
    }

    pub fn force_kill(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        let pid_opt = *self.child_pid.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pid) = pid_opt {
            if let Ok(mut sys) = System::new() {
                let spid = Pid::from_u32(pid);
                sys.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&[spid]),
                    true,
                    ProcessRefreshKind::nothing(),
                );
                if let Some(p) = sys.process(spid) {
                    p.kill();
                }
            }
        }
    }
}

pub fn start_server(
    rt: &tokio::runtime::Handle,
    paths: LauncherPaths,
    profile: ServerProfile,
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

    let mut effective_cfg = profile.config.clone();
    if effective_cfg.worldpath.trim().is_empty() {
        effective_cfg.worldpath = match profile.edition {
            GameEdition::Vanilla => paths.vanilla_worlds_dir(),
            GameEdition::TModLoader => paths.tml_worlds_dir(),
        }
        .to_string_lossy()
        .into_owned();
    }
    if effective_cfg.world.trim().is_empty() && !effective_cfg.worldname.trim().is_empty() {
        effective_cfg.world = Path::new(&effective_cfg.worldpath)
            .join(format!("{}.wld", effective_cfg.worldname.trim()))
            .to_string_lossy()
            .into_owned();
    }
    if effective_cfg.banlist.trim().is_empty() {
        effective_cfg.banlist = server_dir.join("banlist.txt").to_string_lossy().into_owned();
    }
    if let Some(ban_parent) = Path::new(&effective_cfg.banlist).parent() {
        let _ = fs::create_dir_all(ban_parent);
    }
    if !Path::new(&effective_cfg.banlist).exists() {
        let _ = fs::write(&effective_cfg.banlist, "");
    }

    let config_file = server_dir.join("serverconfig.txt");
    fs::write(&config_file, effective_cfg.to_config_text(profile.edition))?;

    let supervisor = ServerSupervisor::new(&paths, &profile);
    {
        let mut guard = supervisor.shared.write().unwrap_or_else(|e| e.into_inner());
        guard.snapshot.state = ServerLifecycleState::Starting;
        guard.snapshot.status_detail = "Đang khởi chạy tiến trình server...".to_string();
        guard.snapshot.port = effective_cfg.port;
    }

    let sup_clone = supervisor.clone();
    rt.spawn(async move {
        run_server_supervisor(sup_clone, paths, profile, config_file, repaint_ctx).await;
    });

    Ok(supervisor)
}

async fn run_server_supervisor(
    sup: ServerSupervisor,
    paths: LauncherPaths,
    profile: ServerProfile,
    config_file: PathBuf,
    repaint: egui_repaint::RepaintHandle,
) {
    loop {
        sup.stop_requested.store(false, Ordering::SeqCst);
        match spawn_server_process(&paths, &profile, &config_file) {
            Ok(mut child) => {
                let pid = child.id();
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
                let exit_desc = match exit_res {
                    Ok(st) => format!("Mã thoát: {:?}", st.code()),
                    Err(e) => format!("Lỗi chờ tiến trình: {e}"),
                };
                push_log_line(
                    &sup.shared,
                    !user_stopped,
                    format!("[Launcher] Tiến trình server đã kết thúc ({exit_desc})."),
                );

                if user_stopped || !profile.auto_restart {
                    let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
                    guard.snapshot.pid = None;
                    guard.snapshot.players.clear();
                    if user_stopped {
                        guard.snapshot.state = ServerLifecycleState::Stopped;
                        guard.snapshot.status_detail = "Đã dừng".to_string();
                    } else {
                        guard.snapshot.state = ServerLifecycleState::Crashed(exit_desc.clone());
                        guard.snapshot.status_detail = format!("Dừng bất thường ({exit_desc})");
                    }
                    repaint.wake();
                    break;
                }

                {
                    let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
                    guard.snapshot.pid = None;
                    guard.snapshot.players.clear();
                    guard.snapshot.state = ServerLifecycleState::Starting;
                    guard.snapshot.status_detail =
                        "Server tự khởi động lại sau 3 giây...".to_string();
                }
                repaint.wake();
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
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

fn spawn_server_process(
    paths: &LauncherPaths,
    profile: &ServerProfile,
    config_file: &Path,
) -> LauncherResult<Child> {
    let mut cmd = match profile.edition {
        GameEdition::Vanilla => {
            let mut c = Command::new(paths.terraria_server_exe());
            c.current_dir(&paths.terraria_dir);
            c.arg("-savedirectory").arg(&paths.vanilla_saves_dir);
            c
        }
        GameEdition::TModLoader => {
            let dotnet = resolve_dotnet_executable(paths);
            let mut c = Command::new(&dotnet);
            c.current_dir(&paths.tmodloader_dir);
            c.env("DOTNET_ROLL_FORWARD", "Disable");
            let mut std_cmd = std::process::Command::new("dummy");
            prepend_native_lib_path(&mut std_cmd, &paths.tmodloader_dir);
            for (k, v) in std_cmd.get_envs() {
                if let Some(val) = v {
                    c.env(k, val);
                }
            }
            c.arg(paths.tmodloader_dll())
                .arg("-server")
                .arg("-nosteam")
                .arg("-tmlsavedirectory")
                .arg(&paths.tml_saves_dir);
            c
        }
    };

    cmd.arg("-config").arg(config_file);
    if !profile.config.upnp {
        cmd.arg("-noupnp");
    }
    for extra in split_shell_args(&profile.extra_args) {
        cmd.arg(extra);
    }

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    cmd.spawn()
        .map_err(|e| LauncherError::Process(format!("Không thể tạo tiến trình server: {e}")))
}

async fn stdin_writer_task(mut stdin: ChildStdin, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(line) = rx.recv().await {
        if stdin.write_all(line.as_bytes()).await.is_err() {
            break;
        }
        if stdin.write_all(b"\r\n").await.is_err() {
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

fn handle_server_output_line(sup: &ServerSupervisor, is_error: bool, raw_line: &str) {
    let clean = raw_line.trim_end_matches(['\r', '\n']);
    if clean.is_empty() {
        return;
    }
    push_log_line(&sup.shared, is_error, clean.to_string());

    let mut guard = sup.shared.write().unwrap_or_else(|e| e.into_inner());
    let now = Local::now().format("%H:%M:%S").to_string();

    if clean.contains("Listening on port ") || clean.contains("Server started") {
        guard.snapshot.state = ServerLifecycleState::Running;
        guard.snapshot.status_detail = clean.to_string();
    } else if clean.starts_with("Loading world data:")
        || clean.starts_with("Settling liquids")
        || clean.starts_with("Resetting game objects")
        || clean.starts_with("Finding Mods")
        || clean.starts_with("Constructing Mods")
        || clean.starts_with("Adding Content:")
    {
        guard.snapshot.status_detail = clean.to_string();
    } else if clean.starts_with("Choose World:") {
        guard.snapshot.status_detail =
            "Đang chờ chọn World (hãy kiểm tra world / autocreate)".to_string();
    } else if let Some(name) = clean.strip_suffix(" has joined.") {
        let trimmed = name.trim().to_string();
        if !trimmed.is_empty() && !guard.snapshot.players.iter().any(|p| p.name == trimmed) {
            guard.snapshot.players.push(ConnectedPlayer {
                name: trimmed,
                ip: None,
                joined_at: now,
            });
        }
    } else if let Some(name) = clean.strip_suffix(" has left.") {
        let trimmed = name.trim();
        guard.snapshot.players.retain(|p| p.name != trimmed);
    } else if clean == "No players connected." {
        guard.snapshot.players.clear();
    } else if clean.starts_with(':') {
        let body = clean.trim_start_matches(':').trim();
        if let Some((name_part, rest)) = body.rsplit_once(" (") {
            if let Some(ip_part) = rest.strip_suffix(')') {
                let pname = name_part.trim();
                let ip_str = ip_part.trim().to_string();
                if let Some(existing) = guard
                    .snapshot
                    .players
                    .iter_mut()
                    .find(|p| p.name == pname)
                {
                    existing.ip = Some(ip_str);
                } else if !pname.is_empty() {
                    guard.snapshot.players.push(ConnectedPlayer {
                        name: pname.to_string(),
                        ip: Some(ip_str),
                        joined_at: now,
                    });
                }
            }
        }
    }
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
