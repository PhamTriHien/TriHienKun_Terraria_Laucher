use crate::error::LauncherResult;
use crate::paths::LauncherPaths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

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
            banlist: "banlist.txt".to_string(),
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
            cfg.password = v.clone();
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
            lines.push(format!("password={}", self.password));
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LauncherConfig {
    pub active_edition: GameEdition,
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
    pub bore_server: String,
    pub bore_secret: String,
    pub proxy_url: String,
    pub auto_firewall: bool,
    pub auto_upnp: bool,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        let paths = LauncherPaths::resolve(None, None);
        let tml_profile = ServerProfile {
            id: "default-tml".to_string(),
            name: "tModLoader Server".to_string(),
            edition: GameEdition::TModLoader,
            auto_restart: false,
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
            active_modpack: String::new(),
            quick_join_host: String::new(),
            quick_join_port: 7777,
            quick_join_password: String::new(),
            custom_terraria_dir: String::new(),
            custom_tmodloader_dir: String::new(),
            custom_steamcmd_path: String::new(),
            terraria_install_source: String::new(),
            github_release_token: String::new(),
            active_server_id: tml_profile.id.clone(),
            servers: vec![tml_profile, vanilla_profile],
            saved_join_servers: Vec::new(),
            bore_server: "bore.pub".to_string(),
            bore_secret: String::new(),
            proxy_url: String::new(),
            auto_firewall: true,
            auto_upnp: true,
        }
    }
}

impl LauncherConfig {
    pub fn load_or_default(paths: &LauncherPaths) -> Self {
        let Ok(raw) = std::fs::read_to_string(&paths.config_path) else {
            return Self::default();
        };
        let mut parsed: Self = serde_json::from_str(&raw).unwrap_or_default();
        if parsed.servers.is_empty() {
            parsed.servers = Self::default().servers;
        }
        if !parsed
            .servers
            .iter()
            .any(|s| s.id == parsed.active_server_id)
        {
            parsed.active_server_id = parsed.servers[0].id.clone();
        }
        parsed
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

pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
