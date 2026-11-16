use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tl_core::{
    encode_terraria_safe_arg, is_xna_framework_installed, GameEdition, GraphicsBackend,
    LauncherError, LauncherPaths, LauncherResult, ProcessPriority,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows::Win32::System::Threading::{
    ABOVE_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW as WIN32_CREATE_NO_WINDOW, HIGH_PRIORITY_CLASS,
};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = WIN32_CREATE_NO_WINDOW.0;
const MAX_RECENT_OUTPUT_LINES: usize = 80;
const DOTNET_ROLL_FORWARD_ENV: &str = "DOTNET_ROLL_FORWARD";
const DOTNET_TIERED_PGO_ENV: &str = "DOTNET_TieredPGO";
const DOTNET_GC_SERVER_ENV: &str = "DOTNET_gcServer";
const FNA3D_FORCE_DRIVER_ENV: &str = "FNA3D_FORCE_DRIVER";
const LAST_LAUNCH_LOG_NAME: &str = "last-game-launch.log";

#[cfg(windows)]
pub(crate) fn windows_priority_flag(priority: ProcessPriority) -> u32 {
    match priority {
        ProcessPriority::Normal => 0,
        ProcessPriority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS.0,
        ProcessPriority::High => HIGH_PRIORITY_CLASS.0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeTuning {
    pub tiered_pgo: bool,
    pub server_gc: bool,
    pub process_priority: ProcessPriority,
    pub disable_builtin_upnp: bool,
}

impl Default for RuntimeTuning {
    fn default() -> Self {
        Self {
            tiered_pgo: true,
            server_gc: false,
            process_priority: ProcessPriority::Normal,
            disable_builtin_upnp: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DotnetLaunchSpec {
    pub executable: PathBuf,
    pub using_bundled: bool,
    pub roll_forward: Option<&'static str>,
}

#[derive(Debug, Clone)]
pub struct GameLaunchOptions {
    pub edition: GameEdition,
    pub modpack: String,
    pub join_host: String,
    pub join_port: u16,
    pub join_password: String,
    pub extra_args: String,
    pub graphics_backend: GraphicsBackend,
    pub dotnet_tiered_pgo: bool,
    pub dotnet_server_gc: bool,
    pub process_priority: ProcessPriority,
}

impl Default for GameLaunchOptions {
    fn default() -> Self {
        let tuning = RuntimeTuning::default();
        Self {
            edition: GameEdition::default(),
            modpack: String::new(),
            join_host: String::new(),
            join_port: 0,
            join_password: String::new(),
            extra_args: String::new(),
            graphics_backend: GraphicsBackend::Auto,
            dotnet_tiered_pgo: tuning.tiered_pgo,
            dotnet_server_gc: tuning.server_gc,
            process_priority: tuning.process_priority,
        }
    }
}

impl GameLaunchOptions {
    pub fn runtime_tuning(&self) -> RuntimeTuning {
        RuntimeTuning {
            tiered_pgo: self.dotnet_tiered_pgo,
            server_gc: self.dotnet_server_gc,
            process_priority: self.process_priority,
            disable_builtin_upnp: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningGameSnapshot {
    pub pid: u32,
    pub edition: GameEdition,
    pub memory_mb: u64,
    pub cpu_percent: f32,
}

pub struct ActiveGameProcess {
    pub edition: GameEdition,
    child: Child,
    sys: System,
    recent_output: Arc<Mutex<VecDeque<String>>>,
    log_path: PathBuf,
    collectors: Vec<JoinHandle<()>>,
}

impl ActiveGameProcess {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn poll(&mut self) -> Option<std::process::ExitStatus> {
        let status = self.child.try_wait().ok().flatten()?;
        self.drain_collectors_briefly();
        Some(status)
    }

    pub fn snapshot(&mut self) -> RunningGameSnapshot {
        let pid_u32 = self.child.id();
        let pid = Pid::from_u32(pid_u32);
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_memory().with_cpu(),
        );
        let (memory_mb, cpu_percent) = self
            .sys
            .process(pid)
            .map(|p| (p.memory() / (1024 * 1024), p.cpu_usage()))
            .unwrap_or((0, 0.0));
        RunningGameSnapshot {
            pid: pid_u32,
            edition: self.edition,
            memory_mb,
            cpu_percent,
        }
    }

    pub fn kill(&mut self) -> LauncherResult<()> {
        self.child.kill()?;
        let _ = self.child.wait();
        self.drain_collectors_briefly();
        Ok(())
    }

    pub fn recent_output(&self) -> Vec<String> {
        self.recent_output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    fn drain_collectors_briefly(&mut self) {
        if self.collectors.is_empty() {
            return;
        }
        let deadline = Instant::now() + Duration::from_millis(100);
        loop {
            let mut idx = 0;
            while idx < self.collectors.len() {
                if self.collectors[idx].is_finished() {
                    let handle = self.collectors.swap_remove(idx);
                    let _ = handle.join();
                } else {
                    idx += 1;
                }
            }
            if self.collectors.is_empty() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

pub fn launch_game(
    paths: &LauncherPaths,
    opts: &GameLaunchOptions,
) -> LauncherResult<ActiveGameProcess> {
    paths.ensure_dirs()?;

    match opts.edition {
        GameEdition::Vanilla => launch_vanilla(paths, opts),
        GameEdition::TModLoader => launch_tmodloader(paths, opts),
    }
}

fn launch_vanilla(
    paths: &LauncherPaths,
    opts: &GameLaunchOptions,
) -> LauncherResult<ActiveGameProcess> {
    let cmd = build_vanilla_command(paths, opts)?;

    spawn_tracked_game(
        GameEdition::Vanilla,
        cmd,
        paths,
        None,
        "Không thể chạy Terraria.exe",
    )
}

pub(crate) fn build_vanilla_command(
    paths: &LauncherPaths,
    opts: &GameLaunchOptions,
) -> LauncherResult<Command> {
    build_vanilla_command_with_xna(paths, opts, is_xna_framework_installed())
}

fn build_vanilla_command_with_xna(
    paths: &LauncherPaths,
    opts: &GameLaunchOptions,
    xna_installed: bool,
) -> LauncherResult<Command> {
    let exe = paths.terraria_exe();
    if !exe.is_file() {
        return Err(LauncherError::NotFound(format!(
            "Không tìm thấy Terraria.exe tại {}",
            exe.display()
        )));
    }
    if !xna_installed {
        return Err(LauncherError::NotFound(
            "Thiếu Microsoft XNA Framework 4.0. Hãy nhấn 'Cài đặt XNA 4.0' ở mục Kiểm tra môi trường.".to_string(),
        ));
    }

    let mut cmd = Command::new(&exe);
    cmd.current_dir(&paths.terraria_dir);
    cmd.arg("-savedirectory").arg(&paths.vanilla_saves_dir);
    append_join_args(&mut cmd, opts);
    append_extra_args(&mut cmd, &opts.extra_args);

    #[cfg(windows)]
    {
        let flag = windows_priority_flag(opts.process_priority);
        if flag != 0 {
            cmd.creation_flags(flag);
        }
    }

    Ok(cmd)
}

fn launch_tmodloader(
    paths: &LauncherPaths,
    opts: &GameLaunchOptions,
) -> LauncherResult<ActiveGameProcess> {
    let (cmd, dotnet_spec) = build_tmodloader_command(paths, opts)?;
    spawn_tracked_game(
        GameEdition::TModLoader,
        cmd,
        paths,
        Some(&dotnet_spec),
        "Không thể khởi chạy tModLoader",
    )
}

pub(crate) fn has_terraria_content(paths: &LauncherPaths) -> bool {
    let terraria_content = paths.terraria_dir.join("Content");
    let sibling_content = paths
        .tmodloader_dir
        .parent()
        .map(|p| p.join("Terraria").join("Content").is_dir())
        .unwrap_or(false);
    terraria_content.is_dir() || sibling_content
}

fn build_tmodloader_command(
    paths: &LauncherPaths,
    opts: &GameLaunchOptions,
) -> LauncherResult<(Command, DotnetLaunchSpec)> {
    let dll = paths.tmodloader_dll();
    if !dll.is_file() {
        return Err(LauncherError::NotFound(format!(
            "Không tìm thấy tModLoader.dll tại {}",
            dll.display()
        )));
    }

    if !has_terraria_content(paths) {
        let terraria_content = paths.terraria_dir.join("Content");
        return Err(LauncherError::NotFound(format!(
            "Không tìm thấy thư mục tài nguyên Terraria (Content) tại {}. Hãy cài đặt Terraria hoặc chọn đúng thư mục Terraria trong Cài đặt.",
            terraria_content.display()
        )));
    }

    let dotnet_spec = resolve_dotnet_launch(paths);
    let mut cmd = Command::new(&dotnet_spec.executable);
    cmd.current_dir(&paths.tmodloader_dir);
    apply_dotnet_std_env(&dotnet_spec, &mut cmd);
    apply_dotnet_tuning_std_env(opts.runtime_tuning(), &mut cmd);
    prepend_native_lib_path(&mut cmd, &paths.tmodloader_dir);

    cmd.arg(&dll)
        .arg("-nosteam")
        .arg("-tmlsavedirectory")
        .arg(&paths.tml_saves_dir);

    if let Some(driver) = opts.graphics_backend.fna_driver_name() {
        cmd.env(FNA3D_FORCE_DRIVER_ENV, driver);
    } else {
        cmd.env_remove(FNA3D_FORCE_DRIVER_ENV);
    }

    let modpack = opts.modpack.trim();
    if should_pass_legacy_modpack_arg(paths, modpack) {
        cmd.arg("-modpack").arg(modpack);
    }

    append_join_args(&mut cmd, opts);
    append_extra_args(&mut cmd, &opts.extra_args);

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW | windows_priority_flag(opts.process_priority));

    Ok((cmd, dotnet_spec))
}

fn should_pass_legacy_modpack_arg(paths: &LauncherPaths, modpack: &str) -> bool {
    let modpacks_dir = paths.tml_modpacks_dir();
    !modpack.is_empty()
        && !modpack.contains(['/', '\\'])
        && modpack != "."
        && modpack != ".."
        && !modpacks_dir
            .join(modpack)
            .join("Mods")
            .join("enabled.json")
            .is_file()
        && modpacks_dir.join(format!("{modpack}.json")).is_file()
}

fn spawn_tracked_game(
    edition: GameEdition,
    mut cmd: Command,
    paths: &LauncherPaths,
    dotnet_spec: Option<&DotnetLaunchSpec>,
    spawn_err_context: &str,
) -> LauncherResult<ActiveGameProcess> {
    let sys = System::new()
        .map_err(|e| LauncherError::Process(format!("Không thể khởi tạo sysinfo: {e}")))?;

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| match dotnet_spec {
        Some(spec) => map_dotnet_spawn_error(e, spec, spawn_err_context),
        None => LauncherError::Process(format!("{spawn_err_context}: {e}")),
    })?;

    let logs_dir = paths.data_dir.join("Logs");
    let _ = fs::create_dir_all(&logs_dir);
    let log_path = logs_dir.join(LAST_LAUNCH_LOG_NAME);
    let log_file = Arc::new(Mutex::new(
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path)
            .ok(),
    ));

    let recent_output = Arc::new(Mutex::new(VecDeque::with_capacity(MAX_RECENT_OUTPUT_LINES)));
    let mut collectors = Vec::with_capacity(2);

    if let Some(stdout) = child.stdout.take() {
        collectors.push(spawn_output_collector(
            stdout,
            "stdout",
            Arc::clone(&recent_output),
            Arc::clone(&log_file),
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        collectors.push(spawn_output_collector(
            stderr,
            "stderr",
            Arc::clone(&recent_output),
            Arc::clone(&log_file),
        ));
    }

    Ok(ActiveGameProcess {
        edition,
        child,
        sys,
        recent_output,
        log_path,
        collectors,
    })
}

fn spawn_output_collector<R: Read + Send + 'static>(
    stream: R,
    stream_label: &'static str,
    recent_output: Arc<Mutex<VecDeque<String>>>,
    log_file: Arc<Mutex<Option<File>>>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut raw_buf = Vec::new();
        loop {
            raw_buf.clear();
            match reader.read_until(b'\n', &mut raw_buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let decoded = String::from_utf8_lossy(&raw_buf);
                    let trimmed = decoded.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    {
                        let mut guard = recent_output.lock().unwrap_or_else(|e| e.into_inner());
                        if guard.len() >= MAX_RECENT_OUTPUT_LINES {
                            guard.pop_front();
                        }
                        guard.push_back(trimmed.to_string());
                    }
                    if let Ok(mut file_guard) = log_file.lock() {
                        if let Some(file) = file_guard.as_mut() {
                            let _ = writeln!(file, "[{stream_label}] {trimmed}");
                            let _ = file.flush();
                        }
                    }
                }
            }
        }
    })
}

pub fn resolve_dotnet_launch(paths: &LauncherPaths) -> DotnetLaunchSpec {
    resolve_dotnet_launch_with(
        paths,
        tl_core::detect_exact_bundled_dotnet_runtime,
        tl_core::query_local_dotnet8_runtime,
        tl_core::query_system_dotnet8_runtime,
    )
}

fn resolve_dotnet_launch_with<FExact, FLocal, FSys>(
    paths: &LauncherPaths,
    detect_exact: FExact,
    query_local: FLocal,
    query_system: FSys,
) -> DotnetLaunchSpec
where
    FExact: Fn(&LauncherPaths, &str) -> bool,
    FLocal: Fn(&LauncherPaths) -> Option<String>,
    FSys: Fn() -> Option<String>,
{
    let local = paths.tmodloader_dir.join("dotnet").join("dotnet.exe");
    let req = crate::game_installer::read_declared_tml_dotnet_version(paths);

    if let Some(ref req_ver) = req {
        if detect_exact(paths, req_ver) {
            return DotnetLaunchSpec {
                executable: local,
                using_bundled: true,
                roll_forward: Some("Disable"),
            };
        }
    }

    if local.is_file() {
        if let Some(local_ver) = query_local(paths) {
            if req
                .as_deref()
                .is_none_or(|r| tl_core::version_at_least_patch(&local_ver, r))
            {
                return DotnetLaunchSpec {
                    executable: local,
                    using_bundled: true,
                    roll_forward: Some("LatestPatch"),
                };
            }
        }
    }

    if !local.is_file() || req.is_some() {
        if let Some(sys_ver) = query_system() {
            if req
                .as_deref()
                .is_none_or(|r| tl_core::version_at_least_patch(&sys_ver, r))
            {
                return DotnetLaunchSpec {
                    executable: PathBuf::from("dotnet"),
                    using_bundled: false,
                    roll_forward: None,
                };
            }
        }
    }

    if local.is_file() {
        DotnetLaunchSpec {
            executable: local,
            using_bundled: true,
            roll_forward: Some("LatestPatch"),
        }
    } else {
        DotnetLaunchSpec {
            executable: PathBuf::from("dotnet"),
            using_bundled: false,
            roll_forward: None,
        }
    }
}

trait ProcessCommandEnv {
    fn set_env_var(&mut self, key: &str, value: &OsStr);
    fn remove_env_var(&mut self, key: &str);
}

impl ProcessCommandEnv for Command {
    fn set_env_var(&mut self, key: &str, value: &OsStr) {
        self.env(key, value);
    }

    fn remove_env_var(&mut self, key: &str) {
        self.env_remove(key);
    }
}

impl ProcessCommandEnv for tokio::process::Command {
    fn set_env_var(&mut self, key: &str, value: &OsStr) {
        self.env(key, value);
    }

    fn remove_env_var(&mut self, key: &str) {
        self.env_remove(key);
    }
}

fn apply_dotnet_env<C: ProcessCommandEnv>(spec: &DotnetLaunchSpec, cmd: &mut C) {
    if let Some(policy) = spec.roll_forward {
        cmd.set_env_var(DOTNET_ROLL_FORWARD_ENV, OsStr::new(policy));
    } else {
        cmd.remove_env_var(DOTNET_ROLL_FORWARD_ENV);
    }
}

pub fn apply_dotnet_std_env(spec: &DotnetLaunchSpec, cmd: &mut Command) {
    apply_dotnet_env(spec, cmd);
}

pub fn apply_dotnet_tokio_env(spec: &DotnetLaunchSpec, cmd: &mut tokio::process::Command) {
    apply_dotnet_env(spec, cmd);
}

fn apply_dotnet_tuning_env<C: ProcessCommandEnv>(tuning: RuntimeTuning, cmd: &mut C) {
    cmd.set_env_var(
        DOTNET_TIERED_PGO_ENV,
        OsStr::new(if tuning.tiered_pgo { "1" } else { "0" }),
    );
    cmd.set_env_var(
        DOTNET_GC_SERVER_ENV,
        OsStr::new(if tuning.server_gc { "1" } else { "0" }),
    );
}

pub(crate) fn apply_dotnet_tuning_std_env(tuning: RuntimeTuning, cmd: &mut Command) {
    apply_dotnet_tuning_env(tuning, cmd);
}

pub(crate) fn apply_dotnet_tuning_tokio_env(
    tuning: RuntimeTuning,
    cmd: &mut tokio::process::Command,
) {
    apply_dotnet_tuning_env(tuning, cmd);
}

pub(crate) fn map_dotnet_spawn_error(
    err: std::io::Error,
    spec: &DotnetLaunchSpec,
    context: &str,
) -> LauncherError {
    if err.kind() == std::io::ErrorKind::NotFound {
        if spec.using_bundled {
            return LauncherError::NotFound(format!(
                "Không tìm thấy .NET 8 Runtime portable tại {}. Hãy nhấn 'Cài đặt .NET 8 Portable' trong tab Chơi Game hoặc Cài đặt.",
                spec.executable.display()
            ));
        }
        return LauncherError::NotFound(
            "Không tìm thấy .NET 8 Runtime (dotnet) trên máy. Hãy nhấn 'Cài đặt .NET 8 Portable' trong tab Chơi Game hoặc Cài đặt.".to_string(),
        );
    }
    LauncherError::Process(format!("{context}: {err}"))
}

pub(crate) fn native_lib_path_env(tml_dir: &Path) -> Option<OsString> {
    let native_dir = tml_dir.join("Libraries").join("Native").join("Windows");
    if !native_dir.is_dir() {
        return None;
    }
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut combined = native_dir.into_os_string();
    if !existing.is_empty() {
        combined.push(";");
        combined.push(existing);
    }
    Some(combined)
}

pub fn prepend_native_lib_path(cmd: &mut Command, tml_dir: &Path) {
    if let Some(combined) = native_lib_path_env(tml_dir) {
        cmd.env("PATH", combined);
    }
}

fn append_join_args(cmd: &mut Command, opts: &GameLaunchOptions) {
    let host = opts.join_host.trim();
    if host.is_empty() {
        return;
    }
    let port = if opts.join_port == 0 { 7777 } else { opts.join_port };
    cmd.arg("-join").arg(host);
    cmd.arg("-port").arg(port.to_string());
    if !opts.join_password.is_empty() {
        cmd.arg("-password")
            .arg(encode_terraria_safe_arg(&opts.join_password));
    }
}

fn extract_exe_stem(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('"');
    let file_name = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
    let lower = file_name.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

fn is_rejected_client_stem(stem: &str) -> bool {
    if matches!(
        stem,
        "cmd" | "powershell" | "pwsh" | "conhost" | "wt" | "terrariaserver"
    ) {
        return true;
    }
    let compact: String = stem
        .chars()
        .filter(|c| !matches!(c, ' ' | '_' | '-'))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    compact.starts_with("trihienkunterrarialauncher")
        || compact.starts_with("trihienkunterrarialaucher")
        || compact.starts_with("terrarialauncher")
        || compact.starts_with("terrariallauncher")
}

fn parse_game_client_cmdline(
    process_name: &str,
    exe_path: Option<&Path>,
    args: &[String],
) -> Option<GameEdition> {
    let name_stem = extract_exe_stem(process_name);
    let exe_stem = exe_path.map(|p| extract_exe_stem(&p.to_string_lossy()));

    if is_rejected_client_stem(&name_stem)
        || exe_stem.as_deref().is_some_and(is_rejected_client_stem)
    {
        return None;
    }

    if args.iter().any(|arg| arg.eq_ignore_ascii_case("-server")) {
        return None;
    }

    let has_tml_dll_arg = args.iter().any(|arg| {
        let trimmed = arg.trim().trim_matches('"');
        trimmed
            .rsplit(['\\', '/'])
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("tmodloader.dll"))
    });
    if has_tml_dll_arg
        || name_stem == "tmodloader"
        || exe_stem.as_deref() == Some("tmodloader")
    {
        return Some(GameEdition::TModLoader);
    }

    let is_vanilla_stem = |s: &str| s == "terraria" || s.starts_with("terraria_v");
    if is_vanilla_stem(&name_stem) || exe_stem.as_deref().is_some_and(is_vanilla_stem) {
        return Some(GameEdition::Vanilla);
    }

    None
}

fn normalize_path_for_match(raw: &str) -> String {
    let replaced = raw.trim().trim_matches('"').replace('/', "\\");
    let stripped = replaced.strip_prefix(r"\\?\").unwrap_or(&replaced);
    stripped.trim_end_matches('\\').to_ascii_lowercase()
}

fn text_references_dir(text: &str, dir_norm: &str) -> bool {
    if dir_norm.is_empty() {
        return false;
    }
    let norm = normalize_path_for_match(text);
    let Some(pos) = norm.find(dir_norm) else {
        return false;
    };
    let after = &norm[pos + dir_norm.len()..];
    after.is_empty() || after.starts_with('\\')
}

fn process_references_launcher_paths(
    exe_path: Option<&Path>,
    args: &[String],
    known_dirs: &[String],
) -> bool {
    if let Some(exe) = exe_path {
        let exe_str = exe.to_string_lossy();
        if known_dirs.iter().any(|dir| text_references_dir(&exe_str, dir)) {
            return true;
        }
    }
    args.iter()
        .any(|arg| known_dirs.iter().any(|dir| text_references_dir(arg, dir)))
}

fn build_known_game_dirs(paths: &LauncherPaths) -> Vec<String> {
    [
        &paths.terraria_dir,
        &paths.tmodloader_dir,
        &paths.vanilla_saves_dir,
        &paths.tml_saves_dir,
    ]
    .into_iter()
    .map(|p| normalize_path_for_match(&p.to_string_lossy()))
    .filter(|s| !s.is_empty())
    .collect()
}

struct GameProcessCandidate {
    pid: u32,
    proc_name: String,
    exe_path: Option<PathBuf>,
    args: Vec<String>,
    memory_bytes: u64,
    cpu_usage: f32,
}

fn collect_detected_game_processes<I>(
    self_pid: u32,
    known_dirs: &[String],
    processes: I,
) -> Vec<RunningGameSnapshot>
where
    I: IntoIterator<Item = GameProcessCandidate>,
{
    let mut detected: Vec<((bool, u32), RunningGameSnapshot)> = Vec::new();

    for candidate in processes {
        if candidate.pid == self_pid {
            continue;
        }
        let Some(edition) = parse_game_client_cmdline(
            &candidate.proc_name,
            candidate.exe_path.as_deref(),
            &candidate.args,
        ) else {
            continue;
        };

        let preferred = process_references_launcher_paths(
            candidate.exe_path.as_deref(),
            &candidate.args,
            known_dirs,
        );
        let rank = (!preferred, candidate.pid);
        detected.push((
            rank,
            RunningGameSnapshot {
                pid: candidate.pid,
                edition,
                memory_mb: candidate.memory_bytes / (1024 * 1024),
                cpu_percent: candidate.cpu_usage,
            },
        ));
    }

    detected.sort_by_key(|(rank, _)| *rank);
    detected.into_iter().map(|(_, snapshot)| snapshot).collect()
}

pub fn detect_all_existing_game_processes(
    sys: &mut System,
    paths: &LauncherPaths,
) -> Vec<RunningGameSnapshot> {
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_memory()
            .with_cpu(),
    );

    let self_pid = std::process::id();
    let known_dirs = build_known_game_dirs(paths);
    let owned_entries: Vec<GameProcessCandidate> = sys
        .processes()
        .iter()
        .map(|(pid, process)| GameProcessCandidate {
            pid: pid.as_u32(),
            proc_name: process.name().to_string_lossy().into_owned(),
            exe_path: process.exe().map(Path::to_path_buf),
            args: process
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            memory_bytes: process.memory(),
            cpu_usage: process.cpu_usage(),
        })
        .collect();

    collect_detected_game_processes(self_pid, &known_dirs, owned_entries)
}

pub fn detect_existing_game_process(
    sys: &mut System,
    paths: &LauncherPaths,
) -> Option<RunningGameSnapshot> {
    detect_all_existing_game_processes(sys, paths)
        .into_iter()
        .next()
}

pub fn terminate_game_process_by_pid(sys: &mut System, pid: u32) -> bool {
    if pid == 0 || pid == std::process::id() {
        return false;
    }
    let spid = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[spid]),
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cmd(UpdateKind::OnlyIfNotSet),
    );
    let Some(process) = sys.process(spid) else {
        return false;
    };
    let proc_name = process.name().to_string_lossy();
    let exe_path = process.exe();
    let args: Vec<String> = process
        .cmd()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if parse_game_client_cmdline(&proc_name, exe_path, &args).is_none() {
        return false;
    }
    process.kill()
}

pub fn append_extra_args(cmd: &mut Command, raw: &str) {
    for arg in split_shell_args(raw) {
        cmd.arg(arg);
    }
}

pub fn split_shell_args(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in raw.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Launches `msiexec.exe` elevated via UAC to install Microsoft XNA Framework 4.0.
pub fn install_xna_framework_elevated(msi_path: &Path) -> LauncherResult<()> {
    if !msi_path.is_file() {
        return Err(LauncherError::NotFound(format!(
            "Không tìm thấy bộ cài XNA tại {}",
            msi_path.display()
        )));
    }
    let params = format!("/i \"{}\" /passive /norestart", msi_path.display());
    run_elevated_windows("msiexec.exe", &params)
}

#[cfg(windows)]
pub fn run_elevated_windows(file: &str, params: &str) -> LauncherResult<()> {
    use windows::core::{HRESULT, PCWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_CANCELLED, HANDLE, STILL_ACTIVE, WAIT_OBJECT_0,
    };
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};

    struct ProcessHandleGuard(HANDLE);
    impl Drop for ProcessHandleGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let op = to_wide("runas");
    let exe = to_wide(file);
    let args = to_wide(params);
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(op.as_ptr()),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(args.as_ptr()),
        nShow: if file.eq_ignore_ascii_case("netsh.exe") || file.eq_ignore_ascii_case("netsh") {
            SW_HIDE.0
        } else {
            SW_SHOWNORMAL.0
        },
        ..Default::default()
    };

    if let Err(err) = unsafe { ShellExecuteExW(&mut sei) } {
        let last_err = unsafe { GetLastError() };
        if last_err == ERROR_CANCELLED || err.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
            return Err(LauncherError::Process(
                "Người dùng đã hủy cấp quyền Administrator".to_string(),
            ));
        }
        return Err(LauncherError::Process(format!(
            "Không thể khởi chạy tác vụ với quyền Administrator: {err}"
        )));
    }

    if sei.hProcess.is_invalid() {
        return Ok(());
    }
    let _handle_guard = ProcessHandleGuard(sei.hProcess);

    let wait_res = unsafe { WaitForSingleObject(sei.hProcess, 60_000) };
    if wait_res != WAIT_OBJECT_0 {
        return Err(LauncherError::Process(format!(
            "Tác vụ quyền Administrator không hoàn tất trong 60 giây (mã chờ {})",
            wait_res.0
        )));
    }

    let mut exit_code = 0u32;
    unsafe { GetExitCodeProcess(sei.hProcess, &mut exit_code) }.map_err(|e| {
        LauncherError::Process(format!(
            "Không đọc được mã thoát của tác vụ quyền Administrator: {e}"
        ))
    })?;

    if exit_code == STILL_ACTIVE.0 as u32 {
        return Err(LauncherError::Process(
            "Tác vụ quyền Administrator vẫn đang chạy quá thời gian chờ".to_string(),
        ));
    }
    if !is_elevated_exit_success(exit_code) {
        return Err(LauncherError::Process(format!(
            "Tác vụ quyền Administrator thất bại (mã thoát {exit_code})"
        )));
    }

    Ok(())
}

#[cfg(any(windows, test))]
fn is_elevated_exit_success(exit_code: u32) -> bool {
    exit_code == 0 || exit_code == 3010
}

#[cfg(not(windows))]
pub fn run_elevated_windows(_file: &str, _params: &str) -> LauncherResult<()> {
    Err(LauncherError::Process("Chỉ hỗ trợ Windows".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    static ENV_LOCK: StdMutex<()> = StdMutex::new(());

    struct TempDirGuard(PathBuf);

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct EnvRestoreGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvRestoreGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, val);
            Self { key, previous }
        }
    }

    impl Drop for EnvRestoreGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
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
        fs::create_dir_all(&paths.terraria_dir).unwrap();
        fs::create_dir_all(&paths.tmodloader_dir).unwrap();
        (TempDirGuard(base), paths)
    }

    #[test]
    fn resolve_dotnet_launch_prefers_bundled_and_falls_back_to_system() {
        let (_guard, paths) = make_temp_paths("tl_runtime_dotnet_spec");

        let sys_spec = resolve_dotnet_launch(&paths);
        assert!(!sys_spec.using_bundled);
        assert_eq!(sys_spec.executable, PathBuf::from("dotnet"));

        let local_dotnet = paths.tmodloader_dir.join("dotnet").join("dotnet.exe");
        fs::create_dir_all(local_dotnet.parent().unwrap()).unwrap();
        fs::write(&local_dotnet, b"MZ").unwrap();

        let bundled_spec = resolve_dotnet_launch(&paths);
        assert!(bundled_spec.using_bundled);
        assert_eq!(bundled_spec.executable, local_dotnet);
    }

    #[test]
    fn apply_dotnet_std_env_clears_or_disables_roll_forward() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvRestoreGuard::set(DOTNET_ROLL_FORWARD_ENV, "Disable");

        let system_spec = DotnetLaunchSpec {
            executable: PathBuf::from("dotnet"),
            using_bundled: false,
            roll_forward: None,
        };
        let mut std_cmd = Command::new("dotnet");
        std_cmd.env(DOTNET_ROLL_FORWARD_ENV, "Disable");
        apply_dotnet_std_env(&system_spec, &mut std_cmd);
        let std_envs: HashMap<_, _> = std_cmd.get_envs().collect();
        assert_eq!(
            std_envs.get(OsStr::new(DOTNET_ROLL_FORWARD_ENV)),
            Some(&None)
        );

        let mut tokio_cmd = tokio::process::Command::new("dotnet");
        tokio_cmd.env(DOTNET_ROLL_FORWARD_ENV, "Disable");
        apply_dotnet_tokio_env(&system_spec, &mut tokio_cmd);
        let tokio_envs: HashMap<_, _> = tokio_cmd.as_std().get_envs().collect();
        assert_eq!(
            tokio_envs.get(OsStr::new(DOTNET_ROLL_FORWARD_ENV)),
            Some(&None)
        );

        #[cfg(windows)]
        {
            let mut probe = Command::new("cmd");
            probe.args([
                "/C",
                "if defined DOTNET_ROLL_FORWARD (exit 1) else (exit 0)",
            ]);
            apply_dotnet_std_env(&system_spec, &mut probe);
            let status = probe.status().unwrap();
            assert!(
                status.success(),
                "inherited DOTNET_ROLL_FORWARD must be cleared for child process"
            );
        }

        let bundled_spec = DotnetLaunchSpec {
            executable: PathBuf::from(r"C:\tModLoader\dotnet\dotnet.exe"),
            using_bundled: true,
            roll_forward: Some("Disable"),
        };
        let mut bundled_std = Command::new(&bundled_spec.executable);
        apply_dotnet_std_env(&bundled_spec, &mut bundled_std);
        let bundled_envs: HashMap<_, _> = bundled_std.get_envs().collect();
        assert_eq!(
            bundled_envs.get(OsStr::new(DOTNET_ROLL_FORWARD_ENV)),
            Some(&Some(OsStr::new("Disable")))
        );

        let mut bundled_tokio = tokio::process::Command::new(&bundled_spec.executable);
        apply_dotnet_tokio_env(&bundled_spec, &mut bundled_tokio);
        let bundled_tokio_envs: HashMap<_, _> = bundled_tokio.as_std().get_envs().collect();
        assert_eq!(
            bundled_tokio_envs.get(OsStr::new(DOTNET_ROLL_FORWARD_ENV)),
            Some(&Some(OsStr::new("Disable")))
        );

        let patch_spec = DotnetLaunchSpec {
            executable: PathBuf::from(r"C:\tModLoader\dotnet\dotnet.exe"),
            using_bundled: true,
            roll_forward: Some("LatestPatch"),
        };
        let mut patch_std = Command::new(&patch_spec.executable);
        apply_dotnet_std_env(&patch_spec, &mut patch_std);
        let patch_envs: HashMap<_, _> = patch_std.get_envs().collect();
        assert_eq!(
            patch_envs.get(OsStr::new(DOTNET_ROLL_FORWARD_ENV)),
            Some(&Some(OsStr::new("LatestPatch")))
        );
    }

    #[test]
    fn split_shell_args_and_append_join_args_handle_edge_cases() {
        assert!(split_shell_args("").is_empty());
        assert!(split_shell_args("   \t\r\n  ").is_empty());
        assert_eq!(
            split_shell_args(r#"-host 127.0.0.1 -password "my secret pass"   -extra"#),
            vec!["-host", "127.0.0.1", "-password", "my secret pass", "-extra"]
        );
        assert_eq!(
            split_shell_args(r#"-path "C:\Games\Terraria" -unclosed "tail arg"#),
            vec!["-path", r"C:\Games\Terraria", "-unclosed", "tail arg"]
        );

        let mut empty_host_cmd = Command::new("Terraria.exe");
        append_join_args(
            &mut empty_host_cmd,
            &GameLaunchOptions {
                join_host: "   ".to_string(),
                join_port: 7788,
                join_password: "secret".to_string(),
                ..Default::default()
            },
        );
        assert_eq!(empty_host_cmd.get_args().count(), 0);

        let mut default_port_cmd = Command::new("Terraria.exe");
        append_join_args(
            &mut default_port_cmd,
            &GameLaunchOptions {
                join_host: "  play.example.com  ".to_string(),
                join_port: 0,
                join_password: String::new(),
                ..Default::default()
            },
        );
        let args_default: Vec<_> = default_port_cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args_default, vec!["-join", "play.example.com", "-port", "7777"]);

        let mut full_join_cmd = Command::new("Terraria.exe");
        append_join_args(
            &mut full_join_cmd,
            &GameLaunchOptions {
                join_host: "10.0.0.5".to_string(),
                join_port: 7788,
                join_password: "100% pass+word".to_string(),
                ..Default::default()
            },
        );
        let args_full: Vec<_> = full_join_cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args_full,
            vec![
                "-join",
                "10.0.0.5",
                "-port",
                "7788",
                "-password",
                "100%25%20pass%2Bword"
            ]
        );
    }

    #[test]
    fn parse_game_client_cmdline_distinguishes_clients_from_servers_and_wrappers() {
        assert_eq!(
            parse_game_client_cmdline(
                "Terraria.exe",
                Some(Path::new(r"C:\TerrariaLauncher\Terraria\Terraria.exe")),
                &[
                    r"C:\TerrariaLauncher\Terraria\Terraria.exe".to_string(),
                    "-savedirectory".to_string(),
                    r"C:\TerrariaLauncher\data\Saves\Vanilla".to_string(),
                ],
            ),
            Some(GameEdition::Vanilla)
        );
        assert_eq!(
            parse_game_client_cmdline("Terraria_v1.4.5.8.exe", None, &[]),
            Some(GameEdition::Vanilla)
        );
        assert_eq!(
            parse_game_client_cmdline(
                "dotnet.exe",
                Some(Path::new(
                    r"C:\TerrariaLauncher\tModLoader\dotnet\dotnet.exe"
                )),
                &[
                    "dotnet.exe".to_string(),
                    r"C:\TerrariaLauncher\tModLoader\tModLoader.dll".to_string(),
                    "-nosteam".to_string(),
                ],
            ),
            Some(GameEdition::TModLoader)
        );
        assert_eq!(
            parse_game_client_cmdline(
                "tModLoader.exe",
                Some(Path::new(r"C:\Steam\steamapps\common\tModLoader\tModLoader.exe")),
                &[],
            ),
            Some(GameEdition::TModLoader)
        );
        assert_eq!(
            parse_game_client_cmdline(
                "TerrariaServer.exe",
                Some(Path::new(
                    r"C:\TerrariaLauncher\Terraria\TerrariaServer.exe"
                )),
                &["-config".to_string(), "serverconfig.txt".to_string()],
            ),
            None
        );
        assert_eq!(
            parse_game_client_cmdline(
                "dotnet.exe",
                Some(Path::new(
                    r"C:\TerrariaLauncher\tModLoader\dotnet\dotnet.exe"
                )),
                &[
                    "dotnet".to_string(),
                    r"C:\TerrariaLauncher\tModLoader\tModLoader.dll".to_string(),
                    "-server".to_string(),
                    "-config".to_string(),
                    "serverconfig.txt".to_string(),
                ],
            ),
            None
        );
        assert_eq!(
            parse_game_client_cmdline(
                "TerrariaLauncher.exe",
                Some(Path::new(r"C:\TerrariaLauncher\TerrariaLauncher.exe")),
                &[r"C:\TerrariaLauncher\Terraria\Terraria.exe".to_string()],
            ),
            None
        );
        for (exe_name, exe_path) in [
            (
                "TriHienKun Terraria Launcher.exe",
                r"C:\TerrariaLauncher\TriHienKun Terraria Launcher.exe",
            ),
            (
                "TriHienKun_Terraria_Launcher.exe",
                r"C:\TerrariaLauncher\TriHienKun_Terraria_Launcher.exe",
            ),
        ] {
            assert_eq!(
                parse_game_client_cmdline(
                    exe_name,
                    Some(Path::new(exe_path)),
                    &[
                        r"C:\TerrariaLauncher\Terraria\Terraria.exe".to_string(),
                        r"C:\TerrariaLauncher\tModLoader\tModLoader.dll".to_string(),
                    ],
                ),
                None
            );
        }
        assert_eq!(
            parse_game_client_cmdline(
                "powershell.exe",
                Some(Path::new(
                    r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
                )),
                &[
                    "powershell.exe".to_string(),
                    "-Command".to_string(),
                    r"C:\TerrariaLauncher\Terraria\Terraria.exe".to_string(),
                    r"C:\TerrariaLauncher\tModLoader\tModLoader.dll".to_string(),
                ],
            ),
            None
        );
    }

    #[test]
    fn build_tmodloader_command_handles_modpacks_and_content_checks() {
        let (_guard, paths) = make_temp_paths("tl_runtime_tml_cmd");

        let missing_dll = build_tmodloader_command(&paths, &GameLaunchOptions::default());
        assert!(matches!(missing_dll, Err(LauncherError::NotFound(_))));

        fs::write(paths.tmodloader_dll(), b"DLL").unwrap();
        let missing_content = build_tmodloader_command(&paths, &GameLaunchOptions::default());
        assert!(matches!(missing_content, Err(LauncherError::NotFound(_))));

        fs::create_dir_all(paths.terraria_dir.join("Content")).unwrap();

        let folder_pack_mods = paths.tml_modpacks_dir().join("FolderPack").join("Mods");
        fs::create_dir_all(&folder_pack_mods).unwrap();
        fs::write(folder_pack_mods.join("enabled.json"), b"[]").unwrap();

        let (folder_cmd, _) = build_tmodloader_command(
            &paths,
            &GameLaunchOptions {
                edition: GameEdition::TModLoader,
                modpack: "FolderPack".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let folder_args: Vec<_> = folder_cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !folder_args.iter().any(|a| a == "-modpack"),
            "folder-based modpacks must not pass -modpack"
        );

        fs::write(paths.tml_modpacks_dir().join("LegacyPack.json"), b"{}").unwrap();
        let (legacy_cmd, _) = build_tmodloader_command(
            &paths,
            &GameLaunchOptions {
                edition: GameEdition::TModLoader,
                modpack: " LegacyPack ".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let legacy_args: Vec<_> = legacy_cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let modpack_pos = legacy_args
            .iter()
            .position(|a| a == "-modpack")
            .expect("legacy .json modpack must pass -modpack");
        assert_eq!(legacy_args.get(modpack_pos + 1).map(String::as_str), Some("LegacyPack"));

        let (traversal_cmd, _) = build_tmodloader_command(
            &paths,
            &GameLaunchOptions {
                edition: GameEdition::TModLoader,
                modpack: "../LegacyPack".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!traversal_cmd.get_args().any(|a| a == "-modpack"));
    }

    #[test]
    fn spawn_tracked_game_captures_streams_caps_lines_and_truncates_log() {
        let (_guard, paths) = make_temp_paths("tl_runtime_pipe_capture");

        let mut cmd1 = {
            #[cfg(windows)]
            {
                let mut c = Command::new("cmd");
                c.args([
                    "/C",
                    "(for /L %i in (1,1,90) do @echo out-line-%i) & echo err-line-last 1>&2",
                ]);
                c
            }
            #[cfg(not(windows))]
            {
                let mut c = Command::new("sh");
                c.args([
                    "-c",
                    "i=1; while [ $i -le 90 ]; do echo out-line-$i; i=$((i+1)); done; echo err-line-last >&2",
                ]);
                c
            }
        };
        #[cfg(windows)]
        cmd1.creation_flags(CREATE_NO_WINDOW);

        let mut proc1 =
            spawn_tracked_game(GameEdition::TModLoader, cmd1, &paths, None, "test spawn").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while proc1.poll().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(proc1.poll().is_some());

        let lines1 = proc1.recent_output();
        assert_eq!(lines1.len(), MAX_RECENT_OUTPUT_LINES);
        assert!(lines1.iter().any(|l| l == "err-line-last"));
        assert!(lines1.iter().any(|l| l == "out-line-90"));

        let expected_log = paths.data_dir.join("Logs").join(LAST_LAUNCH_LOG_NAME);
        assert_eq!(proc1.log_path(), expected_log);
        let log_txt1 = fs::read_to_string(proc1.log_path()).unwrap();
        assert!(log_txt1.contains("out-line-90"));
        assert!(log_txt1.contains("err-line-last"));

        let mut cmd2 = {
            #[cfg(windows)]
            {
                let mut c = Command::new("cmd");
                c.args(["/C", "echo second-launch-only"]);
                c
            }
            #[cfg(not(windows))]
            {
                let mut c = Command::new("sh");
                c.args(["-c", "echo second-launch-only"]);
                c
            }
        };
        #[cfg(windows)]
        cmd2.creation_flags(CREATE_NO_WINDOW);

        let mut proc2 =
            spawn_tracked_game(GameEdition::Vanilla, cmd2, &paths, None, "test spawn 2").unwrap();
        let deadline2 = Instant::now() + Duration::from_secs(5);
        while proc2.poll().is_none() && Instant::now() < deadline2 {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(proc2.poll().is_some());

        let log_txt2 = fs::read_to_string(proc2.log_path()).unwrap();
        assert!(log_txt2.contains("second-launch-only"));
        assert!(!log_txt2.contains("out-line-90"));
    }

    #[test]
    fn map_dotnet_spawn_error_reports_actionable_not_found() {
        let spec = DotnetLaunchSpec {
            executable: PathBuf::from("dotnet"),
            using_bundled: false,
            roll_forward: None,
        };
        let err = map_dotnet_spawn_error(
            std::io::Error::from(std::io::ErrorKind::NotFound),
            &spec,
            "Không thể khởi chạy tModLoader",
        );
        match err {
            LauncherError::NotFound(msg) => {
                assert!(msg.contains(".NET 8 Runtime"));
                assert!(msg.contains("Cài đặt .NET 8 Portable"));
            }
            other => panic!("expected LauncherError::NotFound, got {other:?}"),
        }
    }

    #[test]
    fn build_tmodloader_command_applies_default_tuning_and_auto_graphics() {
        let (_guard, paths) = make_temp_paths("tl_runtime_tml_auto");
        fs::write(paths.tmodloader_dll(), b"DLL").unwrap();
        fs::create_dir_all(paths.terraria_dir.join("Content")).unwrap();

        let (cmd, _) = build_tmodloader_command(&paths, &GameLaunchOptions::default()).unwrap();
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!args.iter().any(|a| a.starts_with("/gldevice:")));

        let envs: HashMap<_, _> = cmd.get_envs().collect();
        assert_eq!(envs.get(OsStr::new(FNA3D_FORCE_DRIVER_ENV)), Some(&None));
        assert_eq!(
            envs.get(OsStr::new(DOTNET_TIERED_PGO_ENV)),
            Some(&Some(OsStr::new("1")))
        );
        assert_eq!(
            envs.get(OsStr::new(DOTNET_GC_SERVER_ENV)),
            Some(&Some(OsStr::new("0")))
        );
    }

    #[test]
    fn build_tmodloader_command_applies_vulkan_and_custom_dotnet_tuning() {
        let (_guard, paths) = make_temp_paths("tl_runtime_tml_vulkan");
        fs::write(paths.tmodloader_dll(), b"DLL").unwrap();
        fs::create_dir_all(paths.terraria_dir.join("Content")).unwrap();

        let opts = GameLaunchOptions {
            edition: GameEdition::TModLoader,
            graphics_backend: GraphicsBackend::Vulkan,
            dotnet_tiered_pgo: false,
            dotnet_server_gc: true,
            process_priority: ProcessPriority::High,
            ..Default::default()
        };
        let (cmd, _) = build_tmodloader_command(&paths, &opts).unwrap();
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!args.iter().any(|a| a.starts_with("/gldevice:")));

        let envs: HashMap<_, _> = cmd.get_envs().collect();
        assert_eq!(
            envs.get(OsStr::new(FNA3D_FORCE_DRIVER_ENV)),
            Some(&Some(OsStr::new("Vulkan")))
        );
        assert_eq!(
            envs.get(OsStr::new(DOTNET_TIERED_PGO_ENV)),
            Some(&Some(OsStr::new("0")))
        );
        assert_eq!(
            envs.get(OsStr::new(DOTNET_GC_SERVER_ENV)),
            Some(&Some(OsStr::new("1")))
        );

        let mut tokio_cmd = tokio::process::Command::new("dotnet");
        apply_dotnet_tuning_tokio_env(opts.runtime_tuning(), &mut tokio_cmd);
        let tokio_envs: HashMap<_, _> = tokio_cmd.as_std().get_envs().collect();
        assert_eq!(
            tokio_envs.get(OsStr::new(DOTNET_TIERED_PGO_ENV)),
            Some(&Some(OsStr::new("0")))
        );
        assert_eq!(
            tokio_envs.get(OsStr::new(DOTNET_GC_SERVER_ENV)),
            Some(&Some(OsStr::new("1")))
        );

        #[cfg(windows)]
        {
            assert_eq!(windows_priority_flag(ProcessPriority::Normal), 0);
            assert_eq!(
                windows_priority_flag(ProcessPriority::AboveNormal),
                ABOVE_NORMAL_PRIORITY_CLASS.0
            );
            assert_eq!(
                windows_priority_flag(ProcessPriority::High),
                HIGH_PRIORITY_CLASS.0
            );
        }
    }

    #[test]
    fn build_vanilla_command_validates_executable_xna_and_omits_dotnet_env() {
        let (_guard, paths) = make_temp_paths("tl_runtime_vanilla_cmd");
        let opts = GameLaunchOptions {
            edition: GameEdition::Vanilla,
            join_host: "127.0.0.1".to_string(),
            join_port: 7777,
            extra_args: "-noupnp".to_string(),
            graphics_backend: GraphicsBackend::Vulkan,
            dotnet_tiered_pgo: false,
            dotnet_server_gc: true,
            process_priority: ProcessPriority::AboveNormal,
            ..Default::default()
        };

        assert!(matches!(
            build_vanilla_command(&paths, &opts),
            Err(LauncherError::NotFound(_))
        ));

        fs::write(paths.terraria_exe(), b"MZ").unwrap();
        assert!(matches!(
            build_vanilla_command_with_xna(&paths, &opts, false),
            Err(LauncherError::NotFound(_))
        ));

        let cmd = build_vanilla_command_with_xna(&paths, &opts, true).unwrap();
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let expected_saves = paths.vanilla_saves_dir.to_string_lossy().into_owned();
        assert!(args
            .windows(2)
            .any(|w| w[0] == "-savedirectory" && w[1] == expected_saves));
        assert!(args.iter().any(|a| a == "-noupnp"));

        let envs: HashMap<_, _> = cmd.get_envs().collect();
        assert!(!envs.contains_key(OsStr::new(DOTNET_TIERED_PGO_ENV)));
        assert!(!envs.contains_key(OsStr::new(DOTNET_GC_SERVER_ENV)));
        assert!(!envs.contains_key(OsStr::new(DOTNET_ROLL_FORWARD_ENV)));
        assert!(!envs.contains_key(OsStr::new(FNA3D_FORCE_DRIVER_ENV)));

        match build_vanilla_command(&paths, &opts) {
            Ok(real_cmd) => {
                assert!(is_xna_framework_installed());
                let real_envs: HashMap<_, _> = real_cmd.get_envs().collect();
                assert!(!real_envs.contains_key(OsStr::new(DOTNET_TIERED_PGO_ENV)));
                assert!(!real_envs.contains_key(OsStr::new(DOTNET_GC_SERVER_ENV)));
                assert!(!real_envs.contains_key(OsStr::new(FNA3D_FORCE_DRIVER_ENV)));
            }
            Err(LauncherError::NotFound(_)) => assert!(!is_xna_framework_installed()),
            Err(other) => panic!("unexpected build_vanilla_command error: {other:?}"),
        }
    }

    #[test]
    fn resolve_dotnet_launch_handles_exact_patch_and_system_fallback() {
        let (_guard, paths) = make_temp_paths("tl_runtime_dotnet_roll_forward");
        let local_dotnet = paths.tmodloader_dir.join("dotnet").join("dotnet.exe");
        fs::create_dir_all(local_dotnet.parent().unwrap()).unwrap();
        fs::write(&local_dotnet, b"MZ").unwrap();
        fs::write(
            paths.tmodloader_dir.join("tModLoader.runtimeconfig.json"),
            r#"{"runtimeOptions":{"framework":{"name":"Microsoft.NETCore.App","version":"8.0.23"}}}"#,
        )
        .unwrap();

        let exact = resolve_dotnet_launch_with(
            &paths,
            |_, req| req == "8.0.23",
            |_| Some("8.0.23".to_string()),
            || Some("8.0.24".to_string()),
        );
        assert!(exact.using_bundled);
        assert_eq!(exact.executable, local_dotnet);
        assert_eq!(exact.roll_forward, Some("Disable"));

        let newer_local = resolve_dotnet_launch_with(
            &paths,
            |_, _| false,
            |_| Some("8.0.25".to_string()),
            || Some("8.0.24".to_string()),
        );
        assert!(newer_local.using_bundled);
        assert_eq!(newer_local.executable, local_dotnet);
        assert_eq!(newer_local.roll_forward, Some("LatestPatch"));

        let system_fallback = resolve_dotnet_launch_with(
            &paths,
            |_, _| false,
            |_| Some("8.0.14".to_string()),
            || Some("8.0.24".to_string()),
        );
        assert!(!system_fallback.using_bundled);
        assert_eq!(system_fallback.executable, PathBuf::from("dotnet"));
        assert_eq!(system_fallback.roll_forward, None);

        let local_when_no_sys = resolve_dotnet_launch_with(
            &paths,
            |_, _| false,
            |_| Some("8.0.14".to_string()),
            || Some("8.0.10".to_string()),
        );
        assert!(local_when_no_sys.using_bundled);
        assert_eq!(local_when_no_sys.roll_forward, Some("LatestPatch"));
    }

    #[test]
    fn build_tmodloader_command_requires_vanilla_or_sibling_content() {
        let (_guard, mut paths) = make_temp_paths("tl_runtime_tml_content_guard");
        fs::write(paths.tmodloader_dll(), b"DLL").unwrap();
        fs::create_dir_all(paths.tmodloader_dir.join("Content")).unwrap();

        let err = build_tmodloader_command(&paths, &GameLaunchOptions::default()).unwrap_err();
        assert!(matches!(err, LauncherError::NotFound(_)));

        let custom_terraria = paths.data_dir.join("CustomTerraria");
        fs::create_dir_all(&custom_terraria).unwrap();
        paths.terraria_dir = custom_terraria;
        fs::create_dir_all(paths.tmodloader_dir.parent().unwrap().join("Terraria").join("Content"))
            .unwrap();

        assert!(build_tmodloader_command(&paths, &GameLaunchOptions::default()).is_ok());
    }

    #[test]
    fn should_pass_legacy_modpack_arg_skips_when_folder_modpack_exists() {
        let (_guard, paths) = make_temp_paths("tl_runtime_modpack_collision");
        let modpacks_dir = paths.tml_modpacks_dir();
        let pack_mods = modpacks_dir.join("DualPack").join("Mods");
        fs::create_dir_all(&pack_mods).unwrap();
        fs::write(pack_mods.join("enabled.json"), b"[]").unwrap();
        fs::write(modpacks_dir.join("DualPack.json"), b"{}").unwrap();

        assert!(!should_pass_legacy_modpack_arg(&paths, "DualPack"));
    }

    #[test]
    fn elevated_exit_code_accepts_zero_and_3010_reboot_required() {
        assert!(is_elevated_exit_success(0));
        assert!(is_elevated_exit_success(3010));
        assert!(!is_elevated_exit_success(1603));
        assert!(!is_elevated_exit_success(1));
    }

    #[test]
    fn detect_all_existing_game_processes_sorts_preferred_then_pid_and_ignores_self() {
        let (_guard, paths) = make_temp_paths("tl_game_detect_multi");

        let known_dirs = build_known_game_dirs(&paths);
        let preferred_vanilla_exe = paths.terraria_dir.join("Terraria.exe");
        let preferred_tml_dll = paths.tmodloader_dir.join("tModLoader.dll");
        let external_vanilla_exe = paths.base_dir.join("OtherInstall").join("Terraria.exe");

        let ext_args = vec![external_vanilla_exe.to_string_lossy().into_owned()];
        let pref_vanilla_args = vec![preferred_vanilla_exe.to_string_lossy().into_owned()];
        let pref_tml_args = vec![
            "dotnet".to_string(),
            preferred_tml_dll.to_string_lossy().into_owned(),
        ];
        let server_args = vec![
            "dotnet".to_string(),
            preferred_tml_dll.to_string_lossy().into_owned(),
            "-server".to_string(),
        ];

        let self_pid = 4000u32;
        let candidates = vec![
            GameProcessCandidate {
                pid: 100,
                proc_name: "Terraria.exe".to_string(),
                exe_path: Some(external_vanilla_exe),
                args: ext_args,
                memory_bytes: 512 * 1024 * 1024,
                cpu_usage: 4.5,
            },
            GameProcessCandidate {
                pid: self_pid,
                proc_name: "Terraria.exe".to_string(),
                exe_path: Some(preferred_vanilla_exe.clone()),
                args: pref_vanilla_args.clone(),
                memory_bytes: 256 * 1024 * 1024,
                cpu_usage: 1.0,
            },
            GameProcessCandidate {
                pid: 250,
                proc_name: "Terraria.exe".to_string(),
                exe_path: Some(preferred_vanilla_exe),
                args: pref_vanilla_args,
                memory_bytes: 700 * 1024 * 1024,
                cpu_usage: 8.0,
            },
            GameProcessCandidate {
                pid: 180,
                proc_name: "dotnet.exe".to_string(),
                exe_path: None,
                args: pref_tml_args,
                memory_bytes: 900 * 1024 * 1024,
                cpu_usage: 12.5,
            },
            GameProcessCandidate {
                pid: 120,
                proc_name: "dotnet.exe".to_string(),
                exe_path: None,
                args: server_args,
                memory_bytes: 300 * 1024 * 1024,
                cpu_usage: 2.0,
            },
        ];

        let snapshots = collect_detected_game_processes(self_pid, &known_dirs, candidates);
        let pids_and_editions: Vec<(u32, GameEdition)> =
            snapshots.iter().map(|s| (s.pid, s.edition)).collect();
        assert_eq!(
            pids_and_editions,
            vec![
                (180, GameEdition::TModLoader),
                (250, GameEdition::Vanilla),
                (100, GameEdition::Vanilla),
            ]
        );
        assert_eq!(snapshots[0].memory_mb, 900);

        let mut sys = System::new().expect("create sysinfo System");
        let live = detect_all_existing_game_processes(&mut sys, &paths);
        assert!(live.iter().all(|s| s.pid != std::process::id()));
    }
}
