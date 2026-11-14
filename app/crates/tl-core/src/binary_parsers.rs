use crate::config::write_atomic;
use crate::error::{LauncherError, LauncherResult};
use chrono::{DateTime, Utc};
use flate2::read::DeflateDecoder;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModSide {
    Both = 0,
    Client = 1,
    Server = 2,
    NoSync = 3,
}

impl ModSide {
    pub fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::Client,
            2 => Self::Server,
            3 => Self::NoSync,
            _ => Self::Both,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Both => "Both",
            Self::Client => "Client",
            Self::Server => "Server",
            Self::NoSync => "NoSync",
        }
    }
}

/// Parsed metadata from a compiled `.tmod` archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmodMetadata {
    pub file_path: PathBuf,
    pub file_name: String,
    pub file_size: u64,
    pub tmodloader_version: String,
    pub internal_name: String,
    pub display_name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub homepage: String,
    pub mod_references: Vec<String>,
    pub weak_references: Vec<String>,
    pub side: ModSide,
    pub library_mod: bool,
    pub sha1_hex: String,
    pub icon_png: Option<Vec<u8>>,
}

/// Parsed metadata from a Terraria `.wld` file header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldMetadata {
    pub file_path: PathBuf,
    pub file_name: String,
    pub file_size: u64,
    pub format_version: i32,
    pub world_name: String,
    pub seed: String,
    pub unique_id: String,
    pub world_id: i32,
    pub max_tiles_x: i32,
    pub max_tiles_y: i32,
    pub game_mode: i32,
    pub is_favorite: bool,
    pub special_seeds: Vec<String>,
    pub has_twld: bool,
    pub has_bak: bool,
}

impl WorldMetadata {
    pub fn size_label(&self) -> &'static str {
        if self.max_tiles_x >= 8000 {
            "Large"
        } else if self.max_tiles_x >= 6000 {
            "Medium"
        } else if self.max_tiles_x > 0 {
            "Small"
        } else {
            "Unknown"
        }
    }

    pub fn difficulty_label(&self) -> &'static str {
        match self.game_mode {
            1 => "Expert",
            2 => "Master",
            3 => "Journey",
            _ => "Classic",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModPackInfo {
    pub name: String,
    pub path: PathBuf,
    pub enabled_mods: Vec<String>,
    pub local_tmod_count: usize,
    pub install_workshop_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldBackupInfo {
    pub file_name: String,
    pub file_path: PathBuf,
    pub file_size: u64,
    pub modified_display: String,
}

fn read_u8<R: Read>(r: &mut R) -> LauncherResult<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_bool<R: Read>(r: &mut R) -> LauncherResult<bool> {
    Ok(read_u8(r)? != 0)
}

fn read_i16_le<R: Read>(r: &mut R) -> LauncherResult<i16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(i16::from_le_bytes(b))
}

fn read_i32_le<R: Read>(r: &mut R) -> LauncherResult<i32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(i32::from_le_bytes(b))
}

fn read_u32_le<R: Read>(r: &mut R) -> LauncherResult<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64_le<R: Read>(r: &mut R) -> LauncherResult<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

/// Reads a C# `BinaryReader` 7-bit-encoded length-prefixed UTF-8 string.
pub fn read_dotnet_string<R: Read>(r: &mut R) -> LauncherResult<String> {
    let mut len: usize = 0;
    let mut shift = 0u32;
    for _ in 0..5 {
        let byte = read_u8(r)?;
        len |= ((byte & 0x7F) as usize) << shift;
        if (byte & 0x80) == 0 {
            break;
        }
        shift += 7;
    }
    if len > 16 * 1024 * 1024 {
        return Err(LauncherError::InvalidFormat(format!(
            "String length {len} exceeds safety limit"
        )));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

struct TmodEntryMeta {
    name: String,
    uncompressed_len: usize,
    compressed_len: usize,
}

/// Parses a `.tmod` file header, `Info` BuildProperties, and optional `icon.png`.
pub fn parse_tmod_file(path: &Path) -> LauncherResult<TmodMetadata> {
    let file_meta = fs::metadata(path)?;
    let file_size = file_meta.len();
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut reader = BufReader::new(File::open(path)?);
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    if &magic != b"TMOD" {
        return Err(LauncherError::InvalidFormat(format!(
            "{file_name}: invalid magic header (expected TMOD)"
        )));
    }

    let tmodloader_version = read_dotnet_string(&mut reader)?;
    let mut hash = [0u8; 20];
    reader.read_exact(&mut hash)?;
    let mut signature = [0u8; 256];
    reader.read_exact(&mut signature)?;
    let data_len = read_i32_le(&mut reader)?;
    if data_len < 0 {
        return Err(LauncherError::InvalidFormat(format!(
            "{file_name}: negative data length"
        )));
    }

    let internal_name = read_dotnet_string(&mut reader)?;
    let version = read_dotnet_string(&mut reader)?;
    let file_count = read_i32_le(&mut reader)?;
    if !(0..=100_000).contains(&file_count) {
        return Err(LauncherError::InvalidFormat(format!(
            "{file_name}: invalid entry count {file_count}"
        )));
    }

    let mut entries = Vec::with_capacity(file_count as usize);
    for _ in 0..file_count {
        let name = read_dotnet_string(&mut reader)?;
        let uncompressed_len = read_i32_le(&mut reader)?;
        let compressed_len = read_i32_le(&mut reader)?;
        if uncompressed_len < 0 || compressed_len < 0 {
            return Err(LauncherError::InvalidFormat(format!(
                "{file_name}: invalid entry size for {name}"
            )));
        }
        entries.push(TmodEntryMeta {
            name,
            uncompressed_len: uncompressed_len as usize,
            compressed_len: compressed_len as usize,
        });
    }

    let mut info_bytes: Option<Vec<u8>> = None;
    let mut icon_png: Option<Vec<u8>> = None;

    for entry in entries {
        if entry.name == "Info" || entry.name == "icon.png" {
            let mut raw = vec![0u8; entry.compressed_len];
            reader.read_exact(&mut raw)?;
            let decoded = if entry.compressed_len != entry.uncompressed_len {
                let mut decoder = DeflateDecoder::new(Cursor::new(raw));
                let mut out = Vec::with_capacity(entry.uncompressed_len);
                decoder.read_to_end(&mut out)?;
                out
            } else {
                raw
            };
            if entry.name == "Info" {
                info_bytes = Some(decoded);
            } else {
                icon_png = Some(decoded);
            }
        } else if entry.compressed_len > 0 {
            reader.seek(SeekFrom::Current(entry.compressed_len as i64))?;
        }
    }

    let mut meta = TmodMetadata {
        file_path: path.to_path_buf(),
        file_name,
        file_size,
        tmodloader_version,
        internal_name: internal_name.clone(),
        display_name: internal_name,
        version,
        author: String::new(),
        description: String::new(),
        homepage: String::new(),
        mod_references: Vec::new(),
        weak_references: Vec::new(),
        side: ModSide::Both,
        library_mod: false,
        sha1_hex: hex::encode(hash),
        icon_png,
    };

    if let Some(info) = info_bytes {
        apply_build_properties(&info, &mut meta)?;
    }

    Ok(meta)
}

fn read_dotnet_string_list<R: Read>(r: &mut R) -> LauncherResult<Vec<String>> {
    let mut items = Vec::new();
    loop {
        let s = read_dotnet_string(r)?;
        if s.is_empty() {
            break;
        }
        items.push(s);
    }
    Ok(items)
}

fn apply_build_properties(bytes: &[u8], meta: &mut TmodMetadata) -> LauncherResult<()> {
    let mut cursor = Cursor::new(bytes);
    loop {
        if (cursor.position() as usize) >= bytes.len() {
            break;
        }
        let tag = read_dotnet_string(&mut cursor)?;
        if tag.is_empty() {
            break;
        }
        match tag.as_str() {
            "dllReferences" | "sortAfter" | "sortBefore" => {
                let _ = read_dotnet_string_list(&mut cursor)?;
            }
            "modReferences" => {
                meta.mod_references = read_dotnet_string_list(&mut cursor)?;
            }
            "weakReferences" => {
                meta.weak_references = read_dotnet_string_list(&mut cursor)?;
            }
            "author" => {
                meta.author = read_dotnet_string(&mut cursor)?;
            }
            "version" => {
                let v = read_dotnet_string(&mut cursor)?;
                if !v.is_empty() {
                    meta.version = v;
                }
            }
            "displayName" => {
                let name = read_dotnet_string(&mut cursor)?;
                if !name.is_empty() {
                    meta.display_name = name;
                }
            }
            "homepage" => {
                meta.homepage = read_dotnet_string(&mut cursor)?;
            }
            "description" => {
                meta.description = read_dotnet_string(&mut cursor)?;
            }
            "eacPath" | "buildVersion" | "modSource" => {
                let _ = read_dotnet_string(&mut cursor)?;
            }
            "side" => {
                meta.side = ModSide::from_u8(read_u8(&mut cursor)?);
            }
            "libraryMod" => {
                meta.library_mod = true;
            }
            "noCompile" | "!playableOnPreview" | "translationMod" | "!hideCode"
            | "!hideResources" | "includeSource" => {}
            _ => {
                break;
            }
        }
    }
    Ok(())
}

/// Computes the file SHA-1 hex digest (used for integrity checks).
pub fn sha1_file_hex(path: &Path) -> LauncherResult<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Parses a Terraria `.wld` file header into structured metadata.
pub fn parse_world_file(path: &Path) -> LauncherResult<WorldMetadata> {
    let file_meta = fs::metadata(path)?;
    let file_size = file_meta.len();
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let has_twld = path.with_extension("twld").is_file();
    let has_bak = PathBuf::from(format!("{}.bak", path.display())).is_file();

    let mut reader = BufReader::new(File::open(path)?);
    let format_version = read_i32_le(&mut reader)?;
    if format_version < 135 {
        return Err(LauncherError::InvalidFormat(format!(
            "{file_name}: unsupported legacy world version {format_version}"
        )));
    }

    let mut magic_bytes = [0u8; 8];
    reader.read_exact(&mut magic_bytes)?;
    if &magic_bytes[..7] != b"relogic" || magic_bytes[7] != 2 {
        return Err(LauncherError::InvalidFormat(format!(
            "{file_name}: invalid Re-Logic world magic header"
        )));
    }

    let _revision = read_u32_le(&mut reader)?;
    let flags = read_u64_le(&mut reader)?;
    let is_favorite = (flags & 1) != 0;

    let section_count = read_i16_le(&mut reader)?;
    if section_count <= 0 || section_count > 64 {
        return Err(LauncherError::InvalidFormat(format!(
            "{file_name}: invalid section count {section_count}"
        )));
    }
    let header_offset = read_i32_le(&mut reader)?;
    for _ in 1..section_count {
        let _ = read_i32_le(&mut reader)?;
    }

    reader.seek(SeekFrom::Start(header_offset as u64))?;

    let world_name = read_dotnet_string(&mut reader)?;
    let seed = if format_version >= 179 {
        let s = read_dotnet_string(&mut reader)?;
        let _world_gen_ver = read_u64_le(&mut reader)?;
        s
    } else {
        read_i32_le(&mut reader)?.to_string()
    };

    let unique_id = if format_version >= 181 {
        let mut guid = [0u8; 16];
        reader.read_exact(&mut guid)?;
        Uuid::from_bytes_le(guid).to_string()
    } else {
        String::new()
    };

    let world_id = read_i32_le(&mut reader)?;
    let _left = read_i32_le(&mut reader)?;
    let _right = read_i32_le(&mut reader)?;
    let _top = read_i32_le(&mut reader)?;
    let _bottom = read_i32_le(&mut reader)?;
    let max_tiles_y = read_i32_le(&mut reader)?;
    let max_tiles_x = read_i32_le(&mut reader)?;

    let mut special_seeds = Vec::new();
    let game_mode = if format_version >= 209 {
        let gm = read_i32_le(&mut reader)?;
        let mut read_seed_flag = |min_ver: i32, label: &str| -> LauncherResult<()> {
            if format_version >= min_ver && read_bool(&mut reader)? {
                special_seeds.push(label.to_string());
            }
            Ok(())
        };
        read_seed_flag(222, "Drunk")?;
        read_seed_flag(227, "For The Worthy")?;
        read_seed_flag(238, "Celebrationmk10")?;
        read_seed_flag(239, "The Constant")?;
        read_seed_flag(241, "Not The Bees")?;
        read_seed_flag(249, "Remix")?;
        read_seed_flag(266, "No Traps")?;
        read_seed_flag(267, "Zenith")?;
        gm
    } else {
        let mut gm = 0;
        if format_version >= 112 && read_bool(&mut reader)? {
            gm = 1;
        }
        if format_version >= 208 && read_bool(&mut reader)? {
            gm = 3;
        }
        gm
    };

    Ok(WorldMetadata {
        file_path: path.to_path_buf(),
        file_name,
        file_size,
        format_version,
        world_name,
        seed,
        unique_id,
        world_id,
        max_tiles_x,
        max_tiles_y,
        game_mode,
        is_favorite,
        special_seeds,
        has_twld,
        has_bak,
    })
}

pub fn load_enabled_mods(enabled_json_path: &Path) -> LauncherResult<Vec<String>> {
    if !enabled_json_path.is_file() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(enabled_json_path)?;
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let list: Vec<String> = serde_json::from_str(&raw)?;
    Ok(list)
}

pub fn save_enabled_mods(enabled_json_path: &Path, enabled: &[String]) -> LauncherResult<()> {
    if let Some(parent) = enabled_json_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut seen = HashSet::new();
    let deduped: Vec<String> = enabled
        .iter()
        .filter(|name| seen.insert((*name).clone()))
        .cloned()
        .collect();
    let json = serde_json::to_string_pretty(&deduped)?;
    write_atomic(enabled_json_path, json.as_bytes())?;
    Ok(())
}

pub fn list_modpacks(modpacks_dir: &Path) -> LauncherResult<Vec<ModPackInfo>> {
    if !modpacks_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(modpacks_dir)?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let mods_sub = path.join("Mods");
        let enabled_path = mods_sub.join("enabled.json");
        let enabled_mods = load_enabled_mods(&enabled_path).unwrap_or_default();
        let local_tmod_count = fs::read_dir(&mods_sub)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("tmod"))
                    .unwrap_or(false)
            })
            .count();
        let install_workshop_ids = fs::read_to_string(mods_sub.join("install.txt"))
            .map(|txt| {
                txt.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_digit()))
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default();

        result.push(ModPackInfo {
            name,
            path,
            enabled_mods,
            local_tmod_count,
            install_workshop_ids,
        });
    }
    result.sort_by_key(|a| a.name.to_lowercase());
    Ok(result)
}

pub fn create_modpack(
    modpacks_dir: &Path,
    mods_dir: &Path,
    pack_name: &str,
    enabled_mods: &[String],
    installed_tmods: &[TmodMetadata],
) -> LauncherResult<ModPackInfo> {
    let trimmed = pack_name.trim();
    if trimmed.is_empty()
        || trimmed.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
    {
        return Err(LauncherError::InvalidInput(
            "Tên ModPack không hợp lệ".to_string(),
        ));
    }

    let pack_dir = modpacks_dir.join(trimmed);
    let pack_mods_dir = pack_dir.join("Mods");
    fs::create_dir_all(&pack_mods_dir)?;

    save_enabled_mods(&pack_mods_dir.join("enabled.json"), enabled_mods)?;

    let enabled_set: HashSet<&str> = enabled_mods.iter().map(String::as_str).collect();
    let mut copied_count = 0usize;
    for tmod in installed_tmods {
        if enabled_set.contains(tmod.internal_name.as_str()) {
            let dest = pack_mods_dir.join(&tmod.file_name);
            let src = mods_dir.join(&tmod.file_name);
            if src.is_file() {
                fs::copy(&src, &dest)?;
                copied_count += 1;
            }
        }
    }

    Ok(ModPackInfo {
        name: trimmed.to_string(),
        path: pack_dir,
        enabled_mods: enabled_mods.to_vec(),
        local_tmod_count: copied_count,
        install_workshop_ids: Vec::new(),
    })
}

pub fn activate_modpack(
    modpack_dir: &Path,
    target_mods_dir: &Path,
    copy_tmods: bool,
) -> LauncherResult<Vec<String>> {
    let pack_mods = modpack_dir.join("Mods");
    if !pack_mods.is_dir() {
        return Err(LauncherError::NotFound(format!(
            "Không tìm thấy thư mục Mods trong ModPack {}",
            modpack_dir.display()
        )));
    }
    fs::create_dir_all(target_mods_dir)?;

    if copy_tmods {
        for entry in fs::read_dir(&pack_mods)?.flatten() {
            let p = entry.path();
            if p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("tmod"))
                .unwrap_or(false)
            {
                let dest = target_mods_dir.join(entry.file_name());
                fs::copy(&p, &dest)?;
            }
        }
    }

    let enabled = load_enabled_mods(&pack_mods.join("enabled.json"))?;
    save_enabled_mods(&target_mods_dir.join("enabled.json"), &enabled)?;
    Ok(enabled)
}

pub fn delete_modpack(modpack_dir: &Path) -> LauncherResult<()> {
    if modpack_dir.is_dir() {
        fs::remove_dir_all(modpack_dir)?;
    }
    Ok(())
}

/// Archives `.wld` and matching `.twld`/`.bak` files into a timestamped backup ZIP.
pub fn backup_world(world_path: &Path, backups_dir: &Path) -> LauncherResult<WorldBackupInfo> {
    if !world_path.is_file() {
        return Err(LauncherError::NotFound(format!(
            "Không tìm thấy file thế giới {}",
            world_path.display()
        )));
    }
    fs::create_dir_all(backups_dir)?;

    let stem = world_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "world".to_string());
    let stamp = Utc::now().format("%Y%m%d_%H%M%S");
    let zip_name = format!("{stem}_{stamp}.zip");
    let zip_path = backups_dir.join(&zip_name);

    let file = File::create(&zip_path)?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let companions = [
        world_path.to_path_buf(),
        world_path.with_extension("twld"),
        PathBuf::from(format!("{}.bak", world_path.display())),
    ];

    for item in companions {
        if item.is_file() {
            if let Some(name) = item.file_name().map(|s| s.to_string_lossy().into_owned()) {
                writer.start_file(name, options)?;
                let data = fs::read(&item)?;
                writer.write_all(&data)?;
            }
        }
    }
    writer.finish()?;

    let meta = fs::metadata(&zip_path)?;
    let modified_display = meta
        .modified()
        .ok()
        .map(|t| {
            let dt: DateTime<Utc> = t.into();
            dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
        })
        .unwrap_or_default();

    Ok(WorldBackupInfo {
        file_name: zip_name,
        file_path: zip_path,
        file_size: meta.len(),
        modified_display,
    })
}

pub fn list_world_backups(backups_dir: &Path) -> LauncherResult<Vec<WorldBackupInfo>> {
    if !backups_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut backups = Vec::new();
    for entry in fs::read_dir(backups_dir)?.flatten() {
        let path = entry.path();
        if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("zip"))
            .unwrap_or(false)
        {
            if let Ok(meta) = entry.metadata() {
                let modified_display = meta
                    .modified()
                    .ok()
                    .map(|t| {
                        let dt: DateTime<Utc> = t.into();
                        dt.format("%Y-%m-%d %H:%M UTC").to_string()
                    })
                    .unwrap_or_default();
                backups.push(WorldBackupInfo {
                    file_name: entry.file_name().to_string_lossy().into_owned(),
                    file_path: path,
                    file_size: meta.len(),
                    modified_display,
                });
            }
        }
    }
    backups.sort_by(|a, b| b.file_name.cmp(&a.file_name));
    Ok(backups)
}

pub fn restore_world_backup(zip_path: &Path, target_worlds_dir: &Path) -> LauncherResult<Vec<String>> {
    if !zip_path.is_file() {
        return Err(LauncherError::NotFound(format!(
            "Không tìm thấy file backup {}",
            zip_path.display()
        )));
    }
    fs::create_dir_all(target_worlds_dir)?;
    let mut archive = ZipArchive::new(File::open(zip_path)?)?;
    let mut restored = Vec::new();
    for idx in 0..archive.len() {
        let mut entry = archive.by_index(idx)?;
        if entry.is_dir() {
            continue;
        }
        let Some(safe_name) = entry
            .enclosed_name()
            .and_then(|p| p.file_name().map(|s| s.to_os_string()))
        else {
            continue;
        };
        let out_path = target_worlds_dir.join(&safe_name);
        let mut out_file = File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out_file)?;
        restored.push(safe_name.to_string_lossy().into_owned());
    }
    Ok(restored)
}
