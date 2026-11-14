use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Resolved filesystem layout under the launcher root directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LauncherPaths {
    pub base_dir: PathBuf,
    pub terraria_dir: PathBuf,
    pub tmodloader_dir: PathBuf,
    pub data_dir: PathBuf,
    pub vanilla_saves_dir: PathBuf,
    pub tml_saves_dir: PathBuf,
    pub servers_dir: PathBuf,
    pub backups_dir: PathBuf,
    pub steamcmd_dir: PathBuf,
    pub config_path: PathBuf,
}

impl LauncherPaths {
    /// Resolves paths relative to the current executable or falls back to `C:\TerrariaLauncher`.
    pub fn resolve(custom_terraria: Option<&str>, custom_tmodloader: Option<&str>) -> Self {
        let base_dir = detect_base_dir();
        let terraria_dir = custom_terraria
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| base_dir.join("Terraria"));
        let tmodloader_dir = custom_tmodloader
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| base_dir.join("tModLoader"));
        let data_dir = base_dir.join("data");
        let saves_dir = data_dir.join("Saves");

        Self {
            base_dir: base_dir.clone(),
            terraria_dir,
            tmodloader_dir,
            vanilla_saves_dir: saves_dir.join("Vanilla"),
            tml_saves_dir: saves_dir.join("tModLoader"),
            servers_dir: data_dir.join("Servers"),
            backups_dir: data_dir.join("Backups"),
            steamcmd_dir: data_dir.join("SteamCMD"),
            config_path: data_dir.join("config.json"),
            data_dir,
        }
    }

    pub fn terraria_exe(&self) -> PathBuf {
        self.terraria_dir.join("Terraria.exe")
    }

    pub fn terraria_server_exe(&self) -> PathBuf {
        self.terraria_dir.join("TerrariaServer.exe")
    }

    pub fn xna_msi_path(&self) -> PathBuf {
        self.terraria_dir
            .join("__redist")
            .join("XNA_40")
            .join("xnafx40_redist.msi")
    }

    pub fn terraria_ico_path(&self) -> PathBuf {
        self.terraria_dir
            .join("app")
            .join("goggame-1207665503.ico")
    }

    pub fn tmodloader_dll(&self) -> PathBuf {
        self.tmodloader_dir.join("tModLoader.dll")
    }

    pub fn tml_mods_dir(&self) -> PathBuf {
        self.tml_saves_dir.join("Mods")
    }

    pub fn tml_modpacks_dir(&self) -> PathBuf {
        self.tml_saves_dir.join("ModPacks")
    }

    pub fn tml_enabled_json(&self) -> PathBuf {
        self.tml_mods_dir().join("enabled.json")
    }

    pub fn vanilla_worlds_dir(&self) -> PathBuf {
        self.vanilla_saves_dir.join("Worlds")
    }

    pub fn tml_worlds_dir(&self) -> PathBuf {
        self.tml_saves_dir.join("Worlds")
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in [
            &self.data_dir,
            &self.vanilla_saves_dir,
            &self.vanilla_worlds_dir(),
            &self.tml_saves_dir,
            &self.tml_mods_dir(),
            &self.tml_modpacks_dir(),
            &self.tml_worlds_dir(),
            &self.servers_dir,
            &self.backups_dir,
            &self.steamcmd_dir,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

fn detect_base_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            if parent.join("Terraria").is_dir() || parent.join("tModLoader").is_dir() {
                return parent.to_path_buf();
            }
            if let Some(grand) = parent.parent().and_then(Path::parent).and_then(Path::parent) {
                if grand.join("Terraria").is_dir() || grand.join("tModLoader").is_dir() {
                    return grand.to_path_buf();
                }
            }
        }
    }
    PathBuf::from(r"C:\TerrariaLauncher")
}

/// Verified status of required game binaries and runtime dependencies.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvironmentStatus {
    pub terraria_installed: bool,
    pub terraria_server_installed: bool,
    pub tmodloader_installed: bool,
    pub dotnet8_version: Option<String>,
    pub xna_installed: bool,
    pub xna_msi_available: bool,
    pub steamcmd_path: Option<PathBuf>,
}

pub fn detect_environment(paths: &LauncherPaths, custom_steamcmd: Option<&str>) -> EnvironmentStatus {
    let terraria_installed = paths.terraria_exe().is_file();
    let terraria_server_installed = paths.terraria_server_exe().is_file();
    let tmodloader_installed = paths.tmodloader_dll().is_file();
    let xna_msi_available = paths.xna_msi_path().is_file();
    let xna_installed = is_xna_framework_installed();
    let dotnet8_version = detect_dotnet8_runtime(paths);
    let steamcmd_path = find_steamcmd(paths, custom_steamcmd);

    EnvironmentStatus {
        terraria_installed,
        terraria_server_installed,
        tmodloader_installed,
        dotnet8_version,
        xna_installed,
        xna_msi_available,
        steamcmd_path,
    }
}

pub fn is_xna_framework_installed() -> bool {
    let gac_dir = Path::new(r"C:\Windows\Microsoft.NET\assembly\GAC_32\Microsoft.Xna.Framework");
    let Ok(entries) = std::fs::read_dir(gac_dir) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| entry.path().join("Microsoft.Xna.Framework.dll").is_file())
}

pub fn detect_dotnet8_runtime(paths: &LauncherPaths) -> Option<String> {
    let local_dotnet = paths.tmodloader_dir.join("dotnet").join("dotnet.exe");
    let cmd_bin = if local_dotnet.is_file() {
        local_dotnet.as_os_str().to_os_string()
    } else {
        std::ffi::OsString::from("dotnet")
    };

    let mut cmd = Command::new(cmd_bin);
    cmd.arg("--list-runtimes");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("Microsoft.NETCore.App ") {
                let ver = rest.split_whitespace().next()?;
                if ver.starts_with("8.") {
                    return Some(ver.to_string());
                }
            }
            None
        })
        .next_back()
}

pub fn find_steamcmd(paths: &LauncherPaths, custom: Option<&str>) -> Option<PathBuf> {
    if let Some(custom_str) = custom.map(str::trim).filter(|s| !s.is_empty()) {
        let p = PathBuf::from(custom_str);
        if p.is_file() {
            return Some(p);
        }
    }

    let candidates = [
        paths.steamcmd_dir.join("steamcmd.exe"),
        PathBuf::from(r"C:\steamcmd\steamcmd.exe"),
        PathBuf::from(r"C:\SteamCMD\steamcmd.exe"),
    ];
    for candidate in candidates {
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
        let winget_link = PathBuf::from(&local_app)
            .join("Microsoft")
            .join("WinGet")
            .join("Links")
            .join("steamcmd.exe");
        if winget_link.is_file() {
            return Some(winget_link);
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let exe = dir.join("steamcmd.exe");
            if exe.is_file() {
                return Some(exe);
            }
        }
    }

    None
}
