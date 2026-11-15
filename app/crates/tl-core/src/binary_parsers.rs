use crate::config::write_atomic;
use crate::error::{LauncherError, LauncherResult};
use crate::modpack_share::{
    is_safe_modpack_component, resolve_unique_modpack_name, sanitize_modpack_name,
};
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
    pub detected_edition: Option<crate::config::GameEdition>,
    pub primary_world_file: Option<String>,
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
    if shift == 35 {
        return Err(LauncherError::InvalidFormat(
            "Chuỗi .NET 7-bit length không hợp lệ".into(),
        ));
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

    const MAX_TMOD_META_ENTRY_BYTES: usize = 16 * 1024 * 1024;
    let mut info_bytes: Option<Vec<u8>> = None;
    let mut icon_png: Option<Vec<u8>> = None;

    for entry in entries {
        if entry.name == "Info" || entry.name == "icon.png" {
            if entry.compressed_len > MAX_TMOD_META_ENTRY_BYTES
                || (entry.compressed_len as u64) > file_size
                || entry.uncompressed_len > MAX_TMOD_META_ENTRY_BYTES
            {
                return Err(LauncherError::InvalidFormat(format!(
                    "{file_name}: entry {} exceeds size limit",
                    entry.name
                )));
            }
            let mut raw = vec![0u8; entry.compressed_len];
            reader.read_exact(&mut raw)?;
            let decoded = if entry.compressed_len != entry.uncompressed_len {
                let mut decoder = DeflateDecoder::new(Cursor::new(raw))
                    .take((MAX_TMOD_META_ENTRY_BYTES + 1) as u64);
                let mut out = Vec::with_capacity(entry.uncompressed_len);
                decoder.read_to_end(&mut out)?;
                if out.len() > MAX_TMOD_META_ENTRY_BYTES {
                    return Err(LauncherError::InvalidFormat(format!(
                        "{file_name}: decoded entry {} exceeds size limit",
                        entry.name
                    )));
                }
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
            "dllReferences" | "sortAfter" | "sortBefore" | "sortIgnore" => {
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
    if header_offset < 0 {
        return Err(LauncherError::InvalidFormat(format!(
            "{file_name}: invalid world header offset"
        )));
    }
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
        read_seed_flag(302, "Skyblock")?;
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
    let clean_name = sanitize_modpack_name(pack_name)?;

    let pack_dir = modpacks_dir.join(&clean_name);
    let pack_mods_dir = pack_dir.join("Mods");
    fs::create_dir_all(&pack_mods_dir)?;

    save_enabled_mods(&pack_mods_dir.join("enabled.json"), enabled_mods)?;

    let enabled_set: HashSet<&str> = enabled_mods.iter().map(String::as_str).collect();
    for tmod in installed_tmods {
        if enabled_set.contains(tmod.internal_name.as_str()) {
            let dest = pack_mods_dir.join(&tmod.file_name);
            let src = mods_dir.join(&tmod.file_name);
            if src.is_file() {
                fs::copy(&src, &dest)?;
            }
        }
    }

    let mut local_tmod_count = 0usize;
    for entry in fs::read_dir(&pack_mods_dir)?.flatten() {
        let p = entry.path();
        if p.is_file() && is_tmod_file(&p) {
            let internal_name = parse_tmod_file(&p)
                .map(|meta| meta.internal_name)
                .unwrap_or_else(|_| {
                    p.file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
            if enabled_set.contains(internal_name.as_str()) {
                local_tmod_count += 1;
            } else {
                fs::remove_file(&p)?;
            }
        }
    }

    Ok(ModPackInfo {
        name: clean_name,
        path: pack_dir,
        enabled_mods: enabled_mods.to_vec(),
        local_tmod_count,
        install_workshop_ids: Vec::new(),
    })
}

pub fn write_modpack_install_txt(modpack_dir: &Path, workshop_ids: &[String]) -> LauncherResult<()> {
    let pack_mods_dir = modpack_dir.join("Mods");
    fs::create_dir_all(&pack_mods_dir)?;

    let mut seen = HashSet::new();
    let mut clean_ids = Vec::new();
    for raw in workshop_ids {
        let trimmed = raw.trim();
        if !trimmed.is_empty()
            && trimmed.chars().all(|c| c.is_ascii_digit())
            && seen.insert(trimmed)
        {
            clean_ids.push(trimmed);
        }
    }

    let mut content = clean_ids.join("\n");
    if !content.is_empty() {
        content.push('\n');
    }
    write_atomic(&pack_mods_dir.join("install.txt"), content.as_bytes())?;
    Ok(())
}

pub fn is_safe_single_dir_component(name: &str) -> bool {
    if name.is_empty() || name.trim() != name || !is_safe_modpack_component(name) {
        return false;
    }
    let mut comps = Path::new(name).components();
    matches!(comps.next(), Some(std::path::Component::Normal(_))) && comps.next().is_none()
}

fn is_tmod_file(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("tmod"))
}

pub fn backup_unsaved_enabled_mods_as_modpack(
    modpacks_dir: &Path,
    mods_dir: &Path,
    _active_modpack: &str,
    installed_tmods: &[TmodMetadata],
    preferred_name: &str,
) -> LauncherResult<Option<ModPackInfo>> {
    let enabled = load_enabled_mods(&mods_dir.join("enabled.json"))?;
    if enabled.is_empty() {
        return Ok(None);
    }

    let current_set: HashSet<&str> = enabled.iter().map(String::as_str).collect();
    let existing_packs = list_modpacks(modpacks_dir)?;
    if existing_packs.iter().any(|pack| {
        let pack_set: HashSet<&str> = pack.enabled_mods.iter().map(String::as_str).collect();
        pack_set == current_set
    }) {
        return Ok(None);
    }

    let backup_name = resolve_unique_modpack_name(modpacks_dir, preferred_name)?;
    let pack = create_modpack(
        modpacks_dir,
        mods_dir,
        &backup_name,
        &enabled,
        installed_tmods,
    )?;
    Ok(Some(pack))
}

fn files_have_same_content(src: &Path, dst: &Path) -> LauncherResult<bool> {
    let Ok(dst_meta) = fs::metadata(dst) else {
        return Ok(false);
    };
    if !dst_meta.is_file() {
        return Ok(false);
    }
    let src_meta = fs::metadata(src)?;
    if src_meta.len() != dst_meta.len() {
        return Ok(false);
    }
    Ok(sha1_file_hex(src)? == sha1_file_hex(dst)?)
}

fn copy_file_atomic(src: &Path, dst: &Path) -> LauncherResult<()> {
    let parent = dst
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(".tl_atomic_{}_{}", std::process::id(), Uuid::new_v4()));
    if let Err(err) = fs::copy(src, &tmp) {
        let _ = fs::remove_file(&tmp);
        return Err(err.into());
    }
    if let Err(err) = fs::rename(&tmp, dst) {
        let _ = fs::remove_file(&tmp);
        return Err(err.into());
    }
    Ok(())
}

pub fn sync_active_modpack_from_mods_dir(
    modpacks_dir: &Path,
    mods_dir: &Path,
    active_pack: &str,
    deleted_mod_internal_or_stem: Option<&str>,
) -> LauncherResult<()> {
    let pack_name = active_pack.trim();
    if !is_safe_single_dir_component(pack_name) {
        return Ok(());
    }
    let pack_mods_dir = modpacks_dir.join(pack_name).join("Mods");
    if !pack_mods_dir.is_dir() {
        return Ok(());
    }

    let enabled = load_enabled_mods(&mods_dir.join("enabled.json"))?;
    save_enabled_mods(&pack_mods_dir.join("enabled.json"), &enabled)?;

    let removed_filter = deleted_mod_internal_or_stem
        .map(str::trim)
        .filter(|s| !s.is_empty());

    for entry in fs::read_dir(&pack_mods_dir)?.flatten() {
        let p = entry.path();
        if p.is_file() && is_tmod_file(&p) {
            let stem = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let internal_name = parse_tmod_file(&p)
                .map(|meta| meta.internal_name)
                .unwrap_or_else(|_| stem.clone());
            let matches_removed = removed_filter.is_some_and(|removed| {
                stem.eq_ignore_ascii_case(removed) || internal_name.eq_ignore_ascii_case(removed)
            });
            let is_enabled = enabled
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&internal_name));
            if matches_removed || !is_enabled {
                fs::remove_file(&p)?;
            }
        }
    }

    for tmod in scan_tmod_directory(mods_dir)? {
        if enabled
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&tmod.internal_name))
        {
            let src = mods_dir.join(&tmod.file_name);
            let dst = pack_mods_dir.join(&tmod.file_name);
            if src.is_file() && !files_have_same_content(&src, &dst)? {
                copy_file_atomic(&src, &dst)?;
            }
        }
    }

    Ok(())
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
            if p.is_file() && is_tmod_file(&p) {
                let dest = target_mods_dir.join(entry.file_name());
                if !files_have_same_content(&p, &dest)? {
                    copy_file_atomic(&p, &dest)?;
                }
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

/// Scans compiled `.tmod` archives in `mods_dir`, skipping unreadable files.
pub fn scan_tmod_directory(mods_dir: &Path) -> LauncherResult<Vec<TmodMetadata>> {
    if !mods_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut parsed = Vec::new();
    for entry in fs::read_dir(mods_dir)?.flatten() {
        let p = entry.path();
        if p.is_file()
            && p.extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("tmod"))
        {
            if let Ok(meta) = parse_tmod_file(&p) {
                parsed.push(meta);
            }
        }
    }
    Ok(parsed)
}

/// Ensures a folder-based tModLoader ModPack has local copies of its enabled `.tmod` files
/// without touching `mods_dir/enabled.json`.
pub fn prepare_folder_modpack_for_server(
    modpacks_dir: &Path,
    mods_dir: &Path,
    pack_name: &str,
) -> LauncherResult<Option<PathBuf>> {
    let trimmed = pack_name.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if !is_safe_single_dir_component(trimmed) {
        return Err(LauncherError::InvalidInput(
            "Tên ModPack không hợp lệ".to_string(),
        ));
    }

    let pack_mods_dir = modpacks_dir.join(trimmed).join("Mods");
    if !pack_mods_dir.is_dir() {
        return Ok(None);
    }

    let enabled = load_enabled_mods(&pack_mods_dir.join("enabled.json")).unwrap_or_default();
    if !enabled.is_empty() && mods_dir.is_dir() {
        let enabled_set: HashSet<&str> = enabled.iter().map(String::as_str).collect();
        for tmod in scan_tmod_directory(mods_dir)? {
            if enabled_set.contains(tmod.internal_name.as_str()) {
                let dest = pack_mods_dir.join(&tmod.file_name);
                let src = mods_dir.join(&tmod.file_name);
                if !dest.exists() && src.is_file() {
                    fs::copy(&src, &dest)?;
                }
            }
        }
    }

    Ok(Some(pack_mods_dir))
}

fn append_file_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

fn format_modified_utc(meta: &fs::Metadata) -> String {
    meta.modified()
        .ok()
        .map(|t| {
            let dt: DateTime<Utc> = t.into();
            dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
        })
        .unwrap_or_default()
}

pub fn inspect_world_backup_zip(
    zip_path: &Path,
) -> (Option<crate::config::GameEdition>, Option<String>, bool) {
    let Ok(file) = File::open(zip_path) else {
        return (None, None, false);
    };
    let Ok(mut archive) = ZipArchive::new(file) else {
        return (None, None, false);
    };

    let mut has_twld = false;
    let mut primary_world_file: Option<String> = None;
    let mut world_format_version: Option<i32> = None;
    let mut has_relogic = false;

    for idx in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(idx) else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let Some(fname) = entry
            .enclosed_name()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        else {
            continue;
        };
        let lower = fname.to_ascii_lowercase();
        if lower.ends_with(".twld") {
            has_twld = true;
        } else if lower.ends_with(".wld") && primary_world_file.is_none() {
            primary_world_file = Some(fname);
            let mut buf = [0u8; 16];
            let mut len = 0usize;
            while len < buf.len() {
                match entry.read(&mut buf[len..]) {
                    Ok(0) => break,
                    Ok(n) => len += n,
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            if len >= 4 {
                world_format_version = Some(i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]));
            }
            has_relogic = len >= 11 && &buf[4..11] == b"relogic";
        }
    }

    let detected_edition = if has_twld {
        Some(crate::config::GameEdition::TModLoader)
    } else if primary_world_file.is_some()
        && world_format_version
            .is_some_and(|ver| ver >= 280 || ((140..280).contains(&ver) && has_relogic))
    {
        Some(crate::config::GameEdition::Vanilla)
    } else {
        None
    };

    (detected_edition, primary_world_file, has_twld)
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
    let mut zip_name = format!("{stem}_{stamp}.zip");
    let mut zip_path = backups_dir.join(&zip_name);
    let mut suffix = 2usize;
    while zip_path.exists() {
        zip_name = format!("{stem}_{stamp}_{suffix}.zip");
        zip_path = backups_dir.join(&zip_name);
        suffix += 1;
    }

    let twld_path = world_path.with_extension("twld");
    let mut companions = vec![
        world_path.to_path_buf(),
        twld_path.clone(),
        append_file_suffix(world_path, ".bak"),
        append_file_suffix(&twld_path, ".bak"),
    ];
    for idx in 2..=9 {
        let suffix = format!(".bak{idx}");
        companions.push(append_file_suffix(world_path, &suffix));
        companions.push(append_file_suffix(&twld_path, &suffix));
    }

    let tmp_zip_path = backups_dir.join(format!(".{zip_name}.tl_atomic"));
    let write_result = (|| -> LauncherResult<()> {
        let file = File::create(&tmp_zip_path)?;
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        for item in companions {
            if item.is_file() {
                if let Some(name) = item.file_name().map(|s| s.to_string_lossy().into_owned()) {
                    writer.start_file(name, options)?;
                    let data = fs::read(&item)?;
                    writer.write_all(&data)?;
                }
            }
        }
        let mut finished_file = writer.finish()?;
        finished_file.flush()?;
        drop(finished_file);
        Ok(())
    })();
    if let Err(err) = write_result {
        let _ = fs::remove_file(&tmp_zip_path);
        return Err(err);
    }
    if let Err(err) = fs::rename(&tmp_zip_path, &zip_path) {
        let _ = fs::remove_file(&tmp_zip_path);
        return Err(err.into());
    }

    let meta = fs::metadata(&zip_path)?;
    let modified_display = format_modified_utc(&meta);
    let (detected_edition, primary_world_file, _) = inspect_world_backup_zip(&zip_path);

    Ok(WorldBackupInfo {
        file_name: zip_name,
        file_path: zip_path,
        file_size: meta.len(),
        modified_display,
        detected_edition,
        primary_world_file,
    })
}

pub fn list_world_backups(backups_dir: &Path) -> LauncherResult<Vec<WorldBackupInfo>> {
    if !backups_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut backups = Vec::new();
    for entry in fs::read_dir(backups_dir)?.flatten() {
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("zip"))
                .unwrap_or(false)
        {
            if let Ok(meta) = entry.metadata() {
                let (detected_edition, primary_world_file, _) = inspect_world_backup_zip(&path);
                backups.push(WorldBackupInfo {
                    file_name: entry.file_name().to_string_lossy().into_owned(),
                    file_path: path,
                    file_size: meta.len(),
                    modified_display: format_modified_utc(&meta),
                    detected_edition,
                    primary_world_file,
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
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut restored = Vec::new();
    let mut restored_names = HashSet::new();
    let mut restored_wld_stems: Vec<String> = Vec::new();

    let stage_result = (|| -> LauncherResult<()> {
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
            let final_path = target_worlds_dir.join(&safe_name);
            let tmp_path =
                target_worlds_dir.join(format!(".{idx}_{}.tl_restore_tmp", Uuid::new_v4()));
            staged.push((tmp_path.clone(), final_path.clone()));

            {
                let mut out_file = File::create(&tmp_path)?;
                std::io::copy(&mut entry, &mut out_file)?;
                out_file.flush()?;
            }

            let name_str = safe_name.to_string_lossy().into_owned();
            restored_names.insert(name_str.to_ascii_lowercase());
            if final_path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("wld"))
            {
                if let Some(stem) = final_path.file_stem().and_then(|s| s.to_str()) {
                    if !restored_wld_stems.iter().any(|s| s == stem) {
                        restored_wld_stems.push(stem.to_string());
                    }
                }
            }
            restored.push(name_str);
        }
        Ok(())
    })();

    if let Err(err) = stage_result {
        for (tmp_path, _) in staged {
            let _ = fs::remove_file(tmp_path);
        }
        return Err(err);
    }

    for (idx, (tmp_path, final_path)) in staged.iter().enumerate() {
        if let Err(err) = fs::rename(tmp_path, final_path) {
            for (rem_tmp, _) in &staged[idx..] {
                let _ = fs::remove_file(rem_tmp);
            }
            return Err(err.into());
        }
    }

    for stem in restored_wld_stems {
        let mut stale_candidates = vec![
            format!("{stem}.wld.bak"),
            format!("{stem}.twld"),
            format!("{stem}.twld.bak"),
        ];
        for idx in 2..=9 {
            stale_candidates.push(format!("{stem}.wld.bak{idx}"));
            stale_candidates.push(format!("{stem}.twld.bak{idx}"));
        }
        for stale in stale_candidates {
            if !restored_names.contains(&stale.to_ascii_lowercase()) {
                let stale_path = target_worlds_dir.join(stale);
                if stale_path.is_file() {
                    fs::remove_file(&stale_path)?;
                }
            }
        }
    }

    Ok(restored)
}
