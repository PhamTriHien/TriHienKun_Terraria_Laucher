use crate::error::LauncherResult;
use crate::paths::LauncherPaths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

pub const DEFAULT_TERRARIA_INSTALL_SOURCE: &str =
    "PhamTriHien/TriHienKun_Terraria_Laucher#terraria-game";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GameEdition {
    Vanilla,
    #[default]
    TModLoader,
}

impl GameEdition {
    pub fn label(self) -> &'static str {
        match self {
            Self::Vanilla => "Terraria 1.4.5.8",
            Self::TModLoader => "tModLoader 2026.9",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GraphicsBackend {
    #[default]
    Auto,
    D3d11,
    Vulkan,
    OpenGl,
}

impl GraphicsBackend {
    pub const ALL: [Self; 4] = [Self::Auto, Self::D3d11, Self::Vulkan, Self::OpenGl];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Tự động (Mặc định)",
            Self::D3d11 => "Direct3D 11",
            Self::Vulkan => "Vulkan",
            Self::OpenGl => "OpenGL",
        }
    }

    pub fn fna_driver_name(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::D3d11 => Some("D3D11"),
            Self::Vulkan => Some("Vulkan"),
            Self::OpenGl => Some("OpenGL"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProcessPriority {
    #[default]
    Normal,
    AboveNormal,
    High,
}

impl ProcessPriority {
    pub const ALL: [Self; 3] = [Self::Normal, Self::AboveNormal, Self::High];

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Bình thường",
            Self::AboveNormal => "Ưu tiên cao hơn",
            Self::High => "Cao",
        }
    }
}

impl ThemeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Tối (Dark)",
            Self::Light => "Sáng (Light)",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Dark => "Tối",
            Self::Light => "Sáng",
        }
    }

    pub fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorldSizeOption {
    Small = 1,
    #[default]
    Medium = 2,
    Large = 3,
}

impl WorldSizeOption {
    pub fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::Small,
            3 => Self::Large,
            _ => Self::Medium,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "Nhỏ (Small)",
            Self::Medium => "Trung bình (Medium)",
            Self::Large => "Lớn (Large)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorldDifficultyOption {
    #[default]
    Classic = 0,
    Expert = 1,
    Master = 2,
    Journey = 3,
}

impl WorldDifficultyOption {
    pub fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::Expert,
            2 => Self::Master,
            3 => Self::Journey,
            _ => Self::Classic,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Classic => "Classic",
            Self::Expert => "Expert",
            Self::Master => "Master",
            Self::Journey => "Journey",
        }
    }
}

/// Typed representation of Terraria & tModLoader `serverconfig.txt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfigData {
    pub world: String,
    pub autocreate: Option<u8>,
    pub seed: String,
    pub seed_celebration: bool,
    pub seed_theconstant: bool,
    pub seed_notthebees: bool,
    pub seed_notraps: bool,
    pub seed_fortheworthy: bool,
    pub seed_remix: bool,
    pub seed_drunk: bool,
    pub seed_zenith: bool,
    pub worldname: String,
    pub difficulty: u8,
    pub maxplayers: u16,
    pub port: u16,
    pub password: String,
    pub motd: String,
    pub worldpath: String,
    pub worldrollbackstokeep: u8,
    pub banlist: String,
    pub secure: bool,
    pub language: String,
    pub upnp: bool,
    pub npcstream: u16,
    pub priority: u8,
    pub slowliquids: bool,
    pub modpath: String,
    pub modpack: String,
}

impl Default for ServerConfigData {
    fn default() -> Self {
        Self {
            world: String::new(),
            autocreate: Some(2),
            seed: String::new(),
            seed_celebration: false,
            seed_theconstant: false,
            seed_notthebees: false,
            seed_notraps: false,
            seed_fortheworthy: false,
            seed_remix: false,
            seed_drunk: false,
            seed_zenith: false,
            worldname: "TerrariaWorld".to_string(),
            difficulty: 1,
            maxplayers: 8,
            port: 7777,
            password: String::new(),
            motd: "Chao mung den voi Terraria Server!".to_string(),
            worldpath: String::new(),
            worldrollbackstokeep: 2,
            banlist: String::new(),
            secure: true,
            language: "en-US".to_string(),
            upnp: true,
            npcstream: 60,
            priority: 1,
            slowliquids: false,
            modpath: String::new(),
            modpack: String::new(),
        }
    }
}

fn decode_hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

pub fn encode_terraria_safe_arg(raw: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(raw.len());
    for &b in raw.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0F) as usize] as char);
            }
        }
    }
    out
}

pub fn decode_terraria_safe_arg(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (
                decode_hex_nibble(bytes[i + 1]),
                decode_hex_nibble(bytes[i + 2]),
            ) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl ServerConfigData {
    pub fn parse(text: &str) -> Self {
        let mut map: HashMap<String, String> = HashMap::new();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, val)) = line.split_once('=') {
                map.insert(key.trim().to_ascii_lowercase(), val.trim().to_string());
            }
        }

        let mut cfg = Self::default();
        let get_bool = |key: &str, default: bool| -> bool {
            map.get(key)
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(default)
        };

        if let Some(v) = map.get("world") {
            cfg.world = v.clone();
        }
        cfg.autocreate = map
            .get("autocreate")
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|&v| (1..=3).contains(&v));
        if let Some(v) = map.get("seed") {
            cfg.seed = v.clone();
        }
        cfg.seed_celebration = get_bool("seed_celebration", false);
        cfg.seed_theconstant = get_bool("seed_theconstant", false);
        cfg.seed_notthebees = get_bool("seed_notthebees", false);
        cfg.seed_notraps = get_bool("seed_notraps", false);
        cfg.seed_fortheworthy = get_bool("seed_fortheworthy", false);
        cfg.seed_remix = get_bool("seed_remix", false);
        cfg.seed_drunk = get_bool("seed_drunk", false);
        cfg.seed_zenith = get_bool("seed_zenith", false);
        if let Some(v) = map.get("worldname") {
            cfg.worldname = v.clone();
        }
        if let Some(v) = map.get("difficulty").and_then(|v| v.parse::<u8>().ok()) {
            cfg.difficulty = v.min(3);
        }
        if let Some(v) = map.get("maxplayers").and_then(|v| v.parse::<u16>().ok()) {
            cfg.maxplayers = v.clamp(1, 255);
        }
        if let Some(v) = map.get("port").and_then(|v| v.parse::<u16>().ok()) {
            if v > 0 {
                cfg.port = v;
            }
        }
        if let Some(v) = map.get("password") {
            cfg.password = decode_terraria_safe_arg(v);
        }
        if let Some(v) = map.get("motd") {
            cfg.motd = v.clone();
        }
        if let Some(v) = map.get("worldpath") {
            cfg.worldpath = v.clone();
        }
        if let Some(v) = map
            .get("worldrollbackstokeep")
            .and_then(|v| v.parse::<u8>().ok())
        {
            cfg.worldrollbackstokeep = v;
        }
        if let Some(v) = map.get("banlist") {
            cfg.banlist = v.clone();
        }
        cfg.secure = get_bool("secure", cfg.secure);
        if let Some(v) = map.get("language") {
            cfg.language = v.clone();
        }
        cfg.upnp = get_bool("upnp", cfg.upnp);
        if let Some(v) = map.get("npcstream").and_then(|v| v.parse::<u16>().ok()) {
            cfg.npcstream = v;
        }
        if let Some(v) = map.get("priority").and_then(|v| v.parse::<u8>().ok()) {
            cfg.priority = v.min(5);
        }
        cfg.slowliquids = get_bool("slowliquids", false);
        if let Some(v) = map.get("modpath") {
            cfg.modpath = v.clone();
        }
        if let Some(v) = map.get("modpack") {
            cfg.modpack = v.clone();
        }

        cfg
    }

    pub fn to_config_text(&self, edition: GameEdition) -> String {
        let mut lines = Vec::new();
        if !self.world.trim().is_empty() {
            lines.push(format!("world={}", self.world.trim()));
        }
        if let Some(size) = self.autocreate {
            lines.push(format!("autocreate={}", size.clamp(1, 3)));
        }
        if !self.seed.trim().is_empty() {
            lines.push(format!("seed={}", self.seed.trim()));
        }
        if edition == GameEdition::Vanilla {
            for (flag, key) in [
                (self.seed_celebration, "seed_celebration"),
                (self.seed_theconstant, "seed_theconstant"),
                (self.seed_notthebees, "seed_notthebees"),
                (self.seed_notraps, "seed_notraps"),
                (self.seed_fortheworthy, "seed_fortheworthy"),
                (self.seed_remix, "seed_remix"),
                (self.seed_drunk, "seed_drunk"),
                (self.seed_zenith, "seed_zenith"),
            ] {
                if flag {
                    lines.push(format!("{key}=1"));
                }
            }
            lines.push(format!(
                "worldrollbackstokeep={}",
                self.worldrollbackstokeep
            ));
            if self.slowliquids {
                lines.push("slowliquids=1".to_string());
            }
        }
        if !self.worldname.trim().is_empty() {
            lines.push(format!("worldname={}", self.worldname.trim()));
        }
        lines.push(format!("difficulty={}", self.difficulty.min(3)));
        lines.push(format!("maxplayers={}", self.maxplayers.clamp(1, 255)));
        lines.push(format!("port={}", self.port));
        if !self.password.is_empty() {
            lines.push(format!(
                "password={}",
                encode_terraria_safe_arg(&self.password)
            ));
        }
        if !self.motd.trim().is_empty() {
            lines.push(format!("motd={}", self.motd.trim()));
        }
        if !self.worldpath.trim().is_empty() {
            lines.push(format!("worldpath={}", self.worldpath.trim()));
        }
        if !self.banlist.trim().is_empty() {
            lines.push(format!("banlist={}", self.banlist.trim()));
        }
        lines.push(format!("secure={}", u8::from(self.secure)));
        if !self.language.trim().is_empty() {
            lines.push(format!("language={}", self.language.trim()));
        }
        lines.push(format!("upnp={}", u8::from(self.upnp)));
        lines.push(format!("npcstream={}", self.npcstream));
        lines.push(format!("priority={}", self.priority.min(5)));

        if edition == GameEdition::TModLoader {
            if !self.modpath.trim().is_empty() {
                lines.push(format!("modpath={}", self.modpath.trim()));
            }
            if !self.modpack.trim().is_empty() {
                lines.push(format!("modpack={}", self.modpack.trim()));
            }
        }
        lines.join("\r\n") + "\r\n"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerProfile {
    pub id: String,
    pub name: String,
    pub edition: GameEdition,
    pub auto_restart: bool,
    pub p2p_tunnel: bool,
    pub extra_args: String,
    pub config: ServerConfigData,
}

impl Default for ServerProfile {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: "tModLoader Server".to_string(),
            edition: GameEdition::TModLoader,
            auto_restart: false,
            p2p_tunnel: true,
            extra_args: String::new(),
            config: ServerConfigData::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedJoinServer {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub password: String,
    pub edition: GameEdition,
    #[serde(default)]
    pub modpack: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LauncherConfig {
    pub active_edition: GameEdition,
    pub theme_mode: ThemeMode,
    pub active_modpack: String,
    pub quick_join_host: String,
    pub quick_join_port: u16,
    pub quick_join_password: String,
    pub custom_terraria_dir: String,
    pub custom_tmodloader_dir: String,
    pub custom_steamcmd_path: String,
    pub terraria_install_source: String,
    pub github_release_token: String,
    pub active_server_id: String,
    pub servers: Vec<ServerProfile>,
    pub saved_join_servers: Vec<SavedJoinServer>,
    pub proxy_url: String,
    pub auto_firewall: bool,
    pub auto_upnp: bool,
    pub tml_graphics_backend: GraphicsBackend,
    pub dotnet_tiered_pgo: bool,
    pub dotnet_server_gc: bool,
    pub game_process_priority: ProcessPriority,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        let paths = LauncherPaths::resolve(None, None);
        let tml_profile = ServerProfile {
            id: "default-tml".to_string(),
            name: "tModLoader Server".to_string(),
            edition: GameEdition::TModLoader,
            auto_restart: false,
            p2p_tunnel: true,
            extra_args: String::new(),
            config: ServerConfigData {
                world: paths
                    .tml_worlds_dir()
                    .join("ModdedWorld.wld")
                    .to_string_lossy()
                    .to_string(),
                worldpath: paths.tml_worlds_dir().to_string_lossy().to_string(),
                worldname: "ModdedWorld".to_string(),
                banlist: paths
                    .servers_dir
                    .join("default-tml")
                    .join("banlist.txt")
                    .to_string_lossy()
                    .to_string(),
                ..ServerConfigData::default()
            },
        };
        let vanilla_profile = ServerProfile {
            id: "default-vanilla".to_string(),
            name: "Vanilla 1.4.5.8 Server".to_string(),
            edition: GameEdition::Vanilla,
            auto_restart: false,
            p2p_tunnel: true,
            extra_args: String::new(),
            config: ServerConfigData {
                world: paths
                    .vanilla_worlds_dir()
                    .join("VanillaWorld.wld")
                    .to_string_lossy()
                    .to_string(),
                worldpath: paths.vanilla_worlds_dir().to_string_lossy().to_string(),
                worldname: "VanillaWorld".to_string(),
                banlist: paths
                    .servers_dir
                    .join("default-vanilla")
                    .join("banlist.txt")
                    .to_string_lossy()
                    .to_string(),
                ..ServerConfigData::default()
            },
        };

        Self {
            active_edition: GameEdition::TModLoader,
            theme_mode: ThemeMode::Dark,
            active_modpack: String::new(),
            quick_join_host: String::new(),
            quick_join_port: 7777,
            quick_join_password: String::new(),
            custom_terraria_dir: String::new(),
            custom_tmodloader_dir: String::new(),
            custom_steamcmd_path: String::new(),
            terraria_install_source: DEFAULT_TERRARIA_INSTALL_SOURCE.to_string(),
            github_release_token: String::new(),
            active_server_id: tml_profile.id.clone(),
            servers: vec![tml_profile, vanilla_profile],
            saved_join_servers: Vec::new(),
            proxy_url: String::new(),
            auto_firewall: false,
            auto_upnp: false,
            tml_graphics_backend: GraphicsBackend::Auto,
            dotnet_tiered_pgo: true,
            dotnet_server_gc: false,
            game_process_priority: ProcessPriority::Normal,
        }
    }
}

impl LauncherConfig {
    fn normalized(mut self, _paths: &LauncherPaths) -> Self {
        if self.terraria_install_source.trim().is_empty() {
            self.terraria_install_source = DEFAULT_TERRARIA_INSTALL_SOURCE.to_string();
        }
        if self.servers.is_empty() {
            self.servers = Self::default().servers;
        }
        if !self
            .servers
            .iter()
            .any(|s| s.id == self.active_server_id)
        {
            self.active_server_id = self.servers[0].id.clone();
        }
        self
    }

    pub fn load_or_default(paths: &LauncherPaths) -> Self {
        if !paths.config_path.exists() {
            return Self::default().normalized(paths);
        }
        let raw = match std::fs::read_to_string(&paths.config_path) {
            Ok(raw) => raw,
            Err(err) => {
                tracing::warn!(
                    path = %paths.config_path.display(),
                    error = %err,
                    "Failed to read config.json; backing up corrupt file and using defaults"
                );
                let backup = paths.config_path.with_file_name("config.json.corrupt.bak");
                let _ = std::fs::copy(&paths.config_path, &backup);
                return Self::default().normalized(paths);
            }
        };
        if raw.trim().is_empty() {
            return Self::default().normalized(paths);
        }
        match serde_json::from_str::<Self>(&raw) {
            Ok(parsed) => parsed.normalized(paths),
            Err(err) => {
                tracing::warn!(
                    path = %paths.config_path.display(),
                    error = %err,
                    "Failed to parse config.json; backing up corrupt file and using defaults"
                );
                let backup = paths.config_path.with_file_name("config.json.corrupt.bak");
                if let Err(copy_err) = std::fs::copy(&paths.config_path, &backup) {
                    tracing::warn!(
                        backup = %backup.display(),
                        error = %copy_err,
                        "Failed to preserve corrupt config.json backup"
                    );
                }
                Self::default().normalized(paths)
            }
        }
    }

    pub fn save(&self, paths: &LauncherPaths) -> LauncherResult<()> {
        if let Some(parent) = paths.config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        write_atomic(&paths.config_path, json.as_bytes())?;
        Ok(())
    }

    pub fn active_server(&self) -> Option<&ServerProfile> {
        self.servers
            .iter()
            .find(|s| s.id == self.active_server_id)
            .or_else(|| self.servers.first())
    }

    pub fn active_server_mut(&mut self) -> Option<&mut ServerProfile> {
        let id = self.active_server_id.clone();
        if let Some(idx) = self.servers.iter().position(|s| s.id == id) {
            self.servers.get_mut(idx)
        } else {
            self.servers.first_mut()
        }
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("data");
    let tmp = parent.join(format!(".{file_name}.tl_atomic"));
    if let Err(err) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphics_backend_maps_to_expected_fna3d_driver_names() {
        assert_eq!(GraphicsBackend::Auto.fna_driver_name(), None);
        assert_eq!(GraphicsBackend::D3d11.fna_driver_name(), Some("D3D11"));
        assert_eq!(GraphicsBackend::Vulkan.fna_driver_name(), Some("Vulkan"));
        assert_eq!(GraphicsBackend::OpenGl.fna_driver_name(), Some("OpenGL"));
    }

    #[test]
    fn legacy_launcher_config_json_uses_safe_performance_defaults() {
        let legacy_json = r#"{"active_edition":"TModLoader"}"#;
        let parsed: LauncherConfig = serde_json::from_str(legacy_json).unwrap();
        assert!(!parsed.auto_firewall);
        assert!(!parsed.auto_upnp);
        assert_eq!(parsed.tml_graphics_backend, GraphicsBackend::Auto);
        assert!(parsed.dotnet_tiered_pgo);
        assert!(!parsed.dotnet_server_gc);
        assert_eq!(parsed.game_process_priority, ProcessPriority::Normal);
    }

    #[test]
    fn launcher_config_roundtrips_performance_settings() {
        let cfg = LauncherConfig {
            tml_graphics_backend: GraphicsBackend::Vulkan,
            dotnet_tiered_pgo: false,
            dotnet_server_gc: true,
            game_process_priority: ProcessPriority::High,
            ..Default::default()
        };

        let json = serde_json::to_string(&cfg).unwrap();
        let roundtrip: LauncherConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.tml_graphics_backend, GraphicsBackend::Vulkan);
        assert!(!roundtrip.dotnet_tiered_pgo);
        assert!(roundtrip.dotnet_server_gc);
        assert_eq!(roundtrip.game_process_priority, ProcessPriority::High);
    }

    #[test]
    fn terraria_safe_arg_roundtrips_and_preserves_legacy_unescaped_percent() {
        assert_eq!(encode_terraria_safe_arg("Alpha123-._~"), "Alpha123-._~");
        assert_eq!(decode_terraria_safe_arg("Alpha123-._~"), "Alpha123-._~");

        let special = "100% a+b";
        let encoded = encode_terraria_safe_arg(special);
        assert_eq!(encoded, "100%25%20a%2Bb");
        assert_eq!(decode_terraria_safe_arg(&encoded), special);

        let unicode = "mật khẩu 100% Việt";
        let encoded_unicode = encode_terraria_safe_arg(unicode);
        assert!(!encoded_unicode.contains(' '));
        assert_eq!(decode_terraria_safe_arg(&encoded_unicode), unicode);

        assert_eq!(decode_terraria_safe_arg("100%"), "100%");
        assert_eq!(decode_terraria_safe_arg("100%G1%2"), "100%G1%2");
    }

    #[test]
    fn serverconfig_password_escapes_percent_and_spaces_on_roundtrip() {
        let cfg = ServerConfigData {
            password: "100% my pass+word".to_string(),
            ..ServerConfigData::default()
        };

        let vanilla_text = cfg.to_config_text(GameEdition::Vanilla);
        assert!(vanilla_text.contains("password=100%25%20my%20pass%2Bword\r\n"));
        let parsed_vanilla = ServerConfigData::parse(&vanilla_text);
        assert_eq!(parsed_vanilla.password, "100% my pass+word");

        let tml_text = cfg.to_config_text(GameEdition::TModLoader);
        assert!(tml_text.contains("password=100%25%20my%20pass%2Bword\r\n"));
        let parsed_tml = ServerConfigData::parse(&tml_text);
        assert_eq!(parsed_tml.password, "100% my pass+word");

        let legacy_raw = "port=7777\r\npassword=100%\r\n";
        let parsed_legacy = ServerConfigData::parse(legacy_raw);
        assert_eq!(parsed_legacy.password, "100%");
    }

    #[test]
    fn saved_join_server_defaults_and_roundtrips_modpack() {
        let legacy_json =
            r#"{"name":"Srv","host":"127.0.0.1","port":7777,"password":"","edition":"Vanilla"}"#;
        let parsed: SavedJoinServer = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(parsed.modpack, "");

        let with_pack = SavedJoinServer {
            modpack: "Calamity".to_string(),
            ..parsed
        };
        let encoded = serde_json::to_string(&with_pack).unwrap();
        let roundtrip: SavedJoinServer = serde_json::from_str(&encoded).unwrap();
        assert_eq!(roundtrip.modpack, "Calamity");
    }

    #[test]
    fn server_profile_defaults_and_roundtrips_p2p_tunnel() {
        let legacy_json =
            r#"{"id":"legacy-srv","name":"Legacy Server","edition":"TModLoader","auto_restart":false,"extra_args":""}"#;
        let parsed: ServerProfile = serde_json::from_str(legacy_json).unwrap();
        assert!(parsed.p2p_tunnel);

        let default_profile = ServerProfile::default();
        assert!(default_profile.p2p_tunnel);

        let default_cfg = LauncherConfig::default();
        assert_eq!(default_cfg.servers.len(), 2);
        assert!(default_cfg.servers.iter().all(|s| s.p2p_tunnel));

        let explicit_false_json =
            r#"{"id":"p2p-off","name":"Direct Port","edition":"Vanilla","auto_restart":false,"p2p_tunnel":false,"extra_args":""}"#;
        let disabled: ServerProfile = serde_json::from_str(explicit_false_json).unwrap();
        assert!(!disabled.p2p_tunnel);

        let encoded = serde_json::to_string(&disabled).unwrap();
        assert!(encoded.contains(r#""p2p_tunnel":false"#));
        let roundtrip: ServerProfile = serde_json::from_str(&encoded).unwrap();
        assert!(!roundtrip.p2p_tunnel);
    }

    #[test]
    fn load_or_default_preserves_corrupt_config_backup_and_skips_blank() {
        let root = std::env::temp_dir().join(format!("tl_core_cfg_{}", uuid::Uuid::new_v4()));
        let paths = LauncherPaths {
            base_dir: root.clone(),
            terraria_dir: root.join("Terraria"),
            tmodloader_dir: root.join("tModLoader"),
            data_dir: root.join("data"),
            vanilla_saves_dir: root.join("data/Saves/Vanilla"),
            tml_saves_dir: root.join("data/Saves/tModLoader"),
            servers_dir: root.join("data/Servers"),
            backups_dir: root.join("data/Backups"),
            steamcmd_dir: root.join("data/SteamCMD"),
            config_path: root.join("data/config.json"),
        };
        std::fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();

        std::fs::write(&paths.config_path, "   \n\t ").unwrap();
        let blank_loaded = LauncherConfig::load_or_default(&paths);
        assert!(!blank_loaded.servers.is_empty());
        assert!(!paths
            .config_path
            .with_file_name("config.json.corrupt.bak")
            .exists());

        let corrupt_raw = "{ corrupt json !!!";
        std::fs::write(&paths.config_path, corrupt_raw).unwrap();
        let corrupt_loaded = LauncherConfig::load_or_default(&paths);
        assert_eq!(
            corrupt_loaded.terraria_install_source,
            DEFAULT_TERRARIA_INSTALL_SOURCE
        );
        let backup_path = paths.config_path.with_file_name("config.json.corrupt.bak");
        assert!(backup_path.is_file());
        assert_eq!(std::fs::read_to_string(&backup_path).unwrap(), corrupt_raw);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_atomic_avoids_tmp_extension_and_cleans_up_on_error() {
        let root = std::env::temp_dir().join(format!("tl_core_atomic_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();

        let target = root.join("enabled.json");
        write_atomic(&target, br#"["ModA"]"#).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), r#"["ModA"]"#);

        write_atomic(&target, br#"["ModB"]"#).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), r#"["ModB"]"#);

        let entries: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["enabled.json"]);

        let dir_target = root.join("existing_dir");
        std::fs::create_dir_all(&dir_target).unwrap();
        assert!(write_atomic(&dir_target, b"data").is_err());
        assert!(!root.join(".existing_dir.tl_atomic").exists());

        let _ = std::fs::remove_dir_all(&root);
    }
}
