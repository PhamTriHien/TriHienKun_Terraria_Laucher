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
        self.tml_mods_dir().join("ModPacks")
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

        let modpacks_dir = self.tml_modpacks_dir();
        let legacy_modpacks = self.tml_saves_dir.join("ModPacks");
        if legacy_modpacks.is_dir() && legacy_modpacks != modpacks_dir {
            for entry in std::fs::read_dir(&legacy_modpacks)? {
                let entry = entry?;
                let src = entry.path();
                let dst = modpacks_dir.join(entry.file_name());
                if dst.exists() {
                    continue;
                }
                if std::fs::rename(&src, &dst).is_err() {
                    if src.is_dir() {
                        if let Err(err) = copy_dir_recursive(&src, &dst) {
                            let _ = std::fs::remove_dir_all(&dst);
                            return Err(err);
                        }
                        std::fs::remove_dir_all(&src)?;
                    } else if src.is_file() {
                        std::fs::copy(&src, &dst)?;
                        std::fs::remove_file(&src)?;
                    }
                }
            }
            if std::fs::read_dir(&legacy_modpacks)
                .map(|mut it| it.next().is_none())
                .unwrap_or(false)
            {
                let _ = std::fs::remove_dir(&legacy_modpacks);
            }
        }

        Ok(())
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Resolves the launcher root directory for a given executable path, falling back to `C:\TerrariaLauncher`.
pub fn detect_base_dir_for_exe(exe: Option<&Path>) -> PathBuf {
    if let Some(exe) = exe {
        if let Some(parent) = exe.parent() {
            if parent.join("Terraria").is_dir()
                || parent.join("tModLoader").is_dir()
                || parent.join("data").is_dir()
            {
                return parent.to_path_buf();
            }
            if let Some(grand) = parent.parent().and_then(Path::parent).and_then(Path::parent) {
                if grand.join("Terraria").is_dir()
                    || grand.join("tModLoader").is_dir()
                    || grand.join("app").join("Cargo.toml").is_file()
                {
                    return grand.to_path_buf();
                }
            }
            if exe
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.to_ascii_lowercase().starts_with("terrarialauncher"))
                && parent.is_dir()
            {
                return parent.to_path_buf();
            }
        }
    }
    PathBuf::from(r"C:\TerrariaLauncher")
}

fn detect_base_dir() -> PathBuf {
    detect_base_dir_for_exe(std::env::current_exe().ok().as_deref())
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

fn query_dotnet8_runtime_bin(bin: &std::ffi::OsStr) -> Option<String> {
    let mut cmd = Command::new(bin);
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

/// Queries the bundled `.NET 8` runtime inside `tModLoader\dotnet\dotnet.exe` if present.
pub fn query_local_dotnet8_runtime(paths: &LauncherPaths) -> Option<String> {
    let local_dotnet = paths.tmodloader_dir.join("dotnet").join("dotnet.exe");
    if !local_dotnet.is_file() {
        return None;
    }
    query_dotnet8_runtime_bin(local_dotnet.as_os_str())
}

/// Queries the system-installed `.NET 8` runtime via `dotnet` on `PATH`.
pub fn query_system_dotnet8_runtime() -> Option<String> {
    query_dotnet8_runtime_bin(std::ffi::OsStr::new("dotnet"))
}

pub fn detect_dotnet8_runtime(paths: &LauncherPaths) -> Option<String> {
    query_local_dotnet8_runtime(paths).or_else(query_system_dotnet8_runtime)
}

fn parse_version_triplet(raw: &str) -> Option<(u64, u64, u64)> {
    let mut parts = raw.trim().split('.');
    let parse_part = |s: Option<&str>| -> Option<u64> {
        let token = s?.trim();
        let digits_end = token
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(token.len());
        if digits_end == 0 {
            return None;
        }
        token[..digits_end].parse::<u64>().ok()
    };
    let major = parse_part(parts.next())?;
    let minor = parse_part(parts.next())?;
    let patch = parse_part(parts.next())?;
    Some((major, minor, patch))
}

/// Returns `true` when `installed` matches `required` major.minor and has patch >= `required` patch.
pub fn version_at_least_patch(installed: &str, required: &str) -> bool {
    let Some((ins_maj, ins_min, ins_patch)) = parse_version_triplet(installed) else {
        return false;
    };
    let Some((req_maj, req_min, req_patch)) = parse_version_triplet(required) else {
        return false;
    };
    ins_maj == req_maj && ins_min == req_min && ins_patch >= req_patch
}

pub fn detect_exact_bundled_dotnet_runtime(paths: &LauncherPaths, required_version: &str) -> bool {
    let required = required_version.trim();
    if required.is_empty()
        || required == "."
        || required == ".."
        || required.contains(['/', '\\'])
    {
        return false;
    }
    let dotnet_dir = paths.tmodloader_dir.join("dotnet");
    dotnet_dir.join("dotnet.exe").is_file()
        && dotnet_dir.join("host").join("fxr").is_dir()
        && dotnet_dir
            .join("shared")
            .join("Microsoft.NETCore.App")
            .join(required)
            .is_dir()
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
        let winget_root = PathBuf::from(&local_app).join("Microsoft").join("WinGet");
        let winget_link = winget_root.join("Links").join("steamcmd.exe");
        if winget_link.is_file() {
            return Some(winget_link);
        }
        let packages_dir = winget_root.join("Packages");
        if let Ok(entries) = std::fs::read_dir(&packages_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir()
                    && entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.to_ascii_lowercase().starts_with("valve.steamcmd_"))
                {
                    let exe = path.join("steamcmd.exe");
                    if exe.is_file() {
                        return Some(exe);
                    }
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths_under(root: &Path) -> LauncherPaths {
        let data_dir = root.join("data");
        let saves_dir = data_dir.join("Saves");
        LauncherPaths {
            base_dir: root.to_path_buf(),
            terraria_dir: root.join("Terraria"),
            tmodloader_dir: root.join("tModLoader"),
            vanilla_saves_dir: saves_dir.join("Vanilla"),
            tml_saves_dir: saves_dir.join("tModLoader"),
            servers_dir: data_dir.join("Servers"),
            backups_dir: data_dir.join("Backups"),
            steamcmd_dir: data_dir.join("SteamCMD"),
            config_path: data_dir.join("config.json"),
            data_dir,
        }
    }

    #[test]
    fn tml_modpacks_dir_matches_tmodloader_mods_layout_and_migrates_legacy() {
        let root = std::env::temp_dir().join(format!("tl_core_paths_{}", uuid::Uuid::new_v4()));
        let paths = test_paths_under(&root);
        assert_eq!(
            paths.tml_modpacks_dir(),
            paths.tml_saves_dir.join("Mods").join("ModPacks")
        );

        let legacy_dir = paths.tml_saves_dir.join("ModPacks");
        let legacy_pack_a = legacy_dir.join("PackA").join("Mods");
        std::fs::create_dir_all(&legacy_pack_a).unwrap();
        std::fs::write(legacy_pack_a.join("enabled.json"), r#"["ModA"]"#).unwrap();

        let legacy_conflict = legacy_dir.join("ConflictPack").join("Mods");
        std::fs::create_dir_all(&legacy_conflict).unwrap();
        std::fs::write(legacy_conflict.join("enabled.json"), r#"["OldVersion"]"#).unwrap();

        let dest_conflict = paths.tml_modpacks_dir().join("ConflictPack").join("Mods");
        std::fs::create_dir_all(&dest_conflict).unwrap();
        std::fs::write(dest_conflict.join("enabled.json"), r#"["NewVersion"]"#).unwrap();

        paths.ensure_dirs().unwrap();

        assert_eq!(
            std::fs::read_to_string(
                paths
                    .tml_modpacks_dir()
                    .join("PackA")
                    .join("Mods")
                    .join("enabled.json")
            )
            .unwrap(),
            r#"["ModA"]"#
        );
        assert!(!legacy_dir.join("PackA").exists());
        assert_eq!(
            std::fs::read_to_string(dest_conflict.join("enabled.json")).unwrap(),
            r#"["NewVersion"]"#
        );
        assert!(legacy_dir.join("ConflictPack").exists());

        std::fs::remove_dir_all(legacy_dir.join("ConflictPack")).unwrap();
        paths.ensure_dirs().unwrap();
        assert!(!legacy_dir.exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn detect_exact_bundled_dotnet_runtime_checks_required_layout() {
        let root = std::env::temp_dir().join(format!("tl_core_dotnet_{}", uuid::Uuid::new_v4()));
        let paths = test_paths_under(&root);
        let dotnet_dir = paths.tmodloader_dir.join("dotnet");
        std::fs::create_dir_all(&dotnet_dir).unwrap();

        assert!(!detect_exact_bundled_dotnet_runtime(&paths, ""));
        assert!(!detect_exact_bundled_dotnet_runtime(&paths, "   "));
        assert!(!detect_exact_bundled_dotnet_runtime(&paths, "8.0.15"));

        std::fs::write(dotnet_dir.join("dotnet.exe"), b"MZ").unwrap();
        assert!(!detect_exact_bundled_dotnet_runtime(&paths, "8.0.15"));

        std::fs::create_dir_all(dotnet_dir.join("host").join("fxr")).unwrap();
        assert!(!detect_exact_bundled_dotnet_runtime(&paths, "8.0.15"));

        std::fs::create_dir_all(
            dotnet_dir
                .join("shared")
                .join("Microsoft.NETCore.App")
                .join("8.0.10"),
        )
        .unwrap();
        assert!(!detect_exact_bundled_dotnet_runtime(&paths, "8.0.15"));
        assert!(detect_exact_bundled_dotnet_runtime(&paths, " 8.0.10 "));

        std::fs::create_dir_all(
            dotnet_dir
                .join("shared")
                .join("Microsoft.NETCore.App")
                .join("8.0.15"),
        )
        .unwrap();
        assert!(detect_exact_bundled_dotnet_runtime(&paths, "8.0.15"));
        assert!(!detect_exact_bundled_dotnet_runtime(&paths, ".."));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn detect_base_dir_for_exe_handles_portable_cargo_and_fallback() {
        let root =
            std::env::temp_dir().join(format!("tl_core_basedir_{}", uuid::Uuid::new_v4()));

        let clean_portable = root.join("CleanPortable");
        std::fs::create_dir_all(&clean_portable).unwrap();
        assert_eq!(
            detect_base_dir_for_exe(Some(&clean_portable.join("TerrariaLauncher.exe"))),
            clean_portable
        );
        assert_eq!(
            detect_base_dir_for_exe(Some(&clean_portable.join("TerrariaLauncher_v1.2.exe"))),
            clean_portable
        );
        assert_eq!(
            detect_base_dir_for_exe(Some(&clean_portable.join("other_tool.exe"))),
            PathBuf::from(r"C:\TerrariaLauncher")
        );

        let data_portable = root.join("DataPortable");
        std::fs::create_dir_all(data_portable.join("data")).unwrap();
        assert_eq!(
            detect_base_dir_for_exe(Some(&data_portable.join("CustomName.exe"))),
            data_portable
        );

        let cargo_repo = root.join("RepoRoot");
        let cargo_target_debug = cargo_repo.join("app").join("target").join("debug");
        std::fs::create_dir_all(&cargo_target_debug).unwrap();
        std::fs::write(cargo_repo.join("app").join("Cargo.toml"), "[workspace]\n").unwrap();
        assert_eq!(
            detect_base_dir_for_exe(Some(&cargo_target_debug.join("tl-app.exe"))),
            cargo_repo
        );

        assert_eq!(
            detect_base_dir_for_exe(Some(Path::new("TerrariaLauncher.exe"))),
            PathBuf::from(r"C:\TerrariaLauncher")
        );
        assert_eq!(
            detect_base_dir_for_exe(None),
            PathBuf::from(r"C:\TerrariaLauncher")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn version_at_least_patch_enforces_same_major_minor_and_patch_floor() {
        assert!(version_at_least_patch("8.0.15", "8.0.0"));
        assert!(version_at_least_patch("8.0.15", "8.0.15"));
        assert!(version_at_least_patch("8.0.15-preview.1", "8.0.15"));
        assert!(!version_at_least_patch("8.0.2", "8.0.15"));
        assert!(!version_at_least_patch("9.0.0", "8.0.0"));
        assert!(!version_at_least_patch("8.1.0", "8.0.0"));
        assert!(!version_at_least_patch("invalid", "8.0.0"));
    }
}
