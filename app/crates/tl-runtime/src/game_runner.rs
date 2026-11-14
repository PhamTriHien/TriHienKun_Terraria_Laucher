use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tl_core::{is_xna_framework_installed, GameEdition, LauncherError, LauncherPaths, LauncherResult};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Default)]
pub struct GameLaunchOptions {
    pub edition: GameEdition,
    pub modpack: String,
    pub join_host: String,
    pub join_port: u16,
    pub join_password: String,
    pub extra_args: String,
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
}

impl ActiveGameProcess {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn poll(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().unwrap_or_default()
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
        Ok(())
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
    let exe = paths.terraria_exe();
    if !exe.is_file() {
        return Err(LauncherError::NotFound(format!(
            "Không tìm thấy Terraria.exe tại {}",
            exe.display()
        )));
    }
    if !is_xna_framework_installed() {
        return Err(LauncherError::NotFound(
            "Thiếu Microsoft XNA Framework 4.0. Hãy nhấn 'Cài đặt XNA 4.0' ở mục Kiểm tra môi trường.".to_string(),
        ));
    }

    let mut cmd = Command::new(&exe);
    cmd.current_dir(&paths.terraria_dir);
    cmd.arg("-savedirectory").arg(&paths.vanilla_saves_dir);
    append_join_args(&mut cmd, opts);
    append_extra_args(&mut cmd, &opts.extra_args);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let child = cmd
        .spawn()
        .map_err(|e| LauncherError::Process(format!("Không thể chạy Terraria.exe: {e}")))?;

    Ok(ActiveGameProcess {
        edition: GameEdition::Vanilla,
        child,
        sys: System::new()
            .map_err(|e| LauncherError::Process(format!("Không thể khởi tạo sysinfo: {e}")))?,
    })
}

fn launch_tmodloader(
    paths: &LauncherPaths,
    opts: &GameLaunchOptions,
) -> LauncherResult<ActiveGameProcess> {
    let dll = paths.tmodloader_dll();
    if !dll.is_file() {
        return Err(LauncherError::NotFound(format!(
            "Không tìm thấy tModLoader.dll tại {}",
            dll.display()
        )));
    }

    let dotnet = resolve_dotnet_executable(paths);
    let mut cmd = Command::new(&dotnet);
    cmd.current_dir(&paths.tmodloader_dir);
    cmd.env("DOTNET_ROLL_FORWARD", "Disable");
    prepend_native_lib_path(&mut cmd, &paths.tmodloader_dir);

    cmd.arg(&dll)
        .arg("-nosteam")
        .arg("-tmlsavedirectory")
        .arg(&paths.tml_saves_dir);

    if !opts.modpack.trim().is_empty() {
        cmd.arg("-modpack").arg(opts.modpack.trim());
    }

    append_join_args(&mut cmd, opts);
    append_extra_args(&mut cmd, &opts.extra_args);

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let child = cmd
        .spawn()
        .map_err(|e| LauncherError::Process(format!("Không thể khởi chạy tModLoader: {e}")))?;

    Ok(ActiveGameProcess {
        edition: GameEdition::TModLoader,
        child,
        sys: System::new()
            .map_err(|e| LauncherError::Process(format!("Không thể khởi tạo sysinfo: {e}")))?,
    })
}

pub fn resolve_dotnet_executable(paths: &LauncherPaths) -> std::path::PathBuf {
    let local = paths.tmodloader_dir.join("dotnet").join("dotnet.exe");
    if local.is_file() {
        local
    } else {
        std::path::PathBuf::from("dotnet")
    }
}

pub fn prepend_native_lib_path(cmd: &mut Command, tml_dir: &Path) {
    let native_dir = tml_dir.join("Libraries").join("Native").join("Windows");
    if !native_dir.is_dir() {
        return;
    }
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut combined = native_dir.into_os_string();
    if !existing.is_empty() {
        combined.push(";");
        combined.push(existing);
    }
    cmd.env("PATH", combined);
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
        cmd.arg("-password").arg(&opts.join_password);
    }
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
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let op = to_wide("runas");
    let exe = to_wide(file);
    let args = to_wide(params);
    let res = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(op.as_ptr()),
            PCWSTR(exe.as_ptr()),
            PCWSTR(args.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (res.0 as isize) <= 32 {
        return Err(LauncherError::Process(format!(
            "Không thể khởi chạy tác vụ với quyền Administrator (mã {})",
            res.0 as isize
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn run_elevated_windows(_file: &str, _params: &str) -> LauncherResult<()> {
    Err(LauncherError::Process("Chỉ hỗ trợ Windows".to_string()))
}
