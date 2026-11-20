use crate::{is_safe_single_dir_component, LauncherError, LauncherResult};
use serde_json::Value;
use std::fs::{self, File, Metadata};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

const MAX_CONFIG_BYTES: usize = 512 * 1024;
const BACKUP_DIR: &str = "_LauncherBackups";

pub fn list_mod_configs(dir: &Path) -> LauncherResult<Vec<String>> {
    if !check_directory(dir)? {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if check_name(&name).is_ok()
            && file_metadata(&entry.path()).is_ok_and(|meta| meta.is_some())
        {
            names.push(name);
        }
    }
    names.sort_by_cached_key(|name| name.to_lowercase());
    Ok(names)
}

pub fn read_mod_config(dir: &Path, name: &str) -> LauncherResult<Option<String>> {
    check_name(name)?;
    if !check_directory(dir)? {
        return Ok(None);
    }
    read_config_file(&dir.join(name))
}

pub fn read_mod_config_import(path: &Path) -> LauncherResult<(String, String)> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    check_name(name)?;
    let text = read_config_file(path)?.ok_or_else(|| {
        LauncherError::NotFound(format!("Không tìm thấy file cấu hình: {}", path.display()))
    })?;
    validate_mod_config(&text)?;
    Ok((name.to_string(), text))
}

pub fn validate_mod_config(text: &str) -> LauncherResult<()> {
    if text.len() > MAX_CONFIG_BYTES {
        return Err(file_too_large());
    }
    let parsed: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))?;
    if !parsed.is_object() {
        return Err(LauncherError::InvalidFormat(
            "Cấu hình mod phải là một đối tượng JSON (bắt đầu bằng { và kết thúc bằng }).".into(),
        ));
    }
    Ok(())
}

pub fn save_mod_config(
    dir: &Path,
    name: &str,
    baseline: Option<&str>,
    text: &str,
) -> LauncherResult<Option<PathBuf>> {
    check_name(name)?;
    validate_mod_config(text)?;
    if !check_directory(dir)? {
        fs::create_dir_all(dir)?;
        check_directory(dir)?;
    }
    let target = dir.join(name);
    let current = read_config_file(&target)?;
    if current.as_deref() != baseline {
        return Err(stale_config());
    }
    let mut staged = tempfile::NamedTempFile::new_in(dir)?;
    staged.write_all(text.as_bytes())?;
    staged.as_file().sync_all()?;
    if current.is_none() {
        staged.persist_noclobber(target).map_err(|err| {
            if err.error.kind() == ErrorKind::AlreadyExists {
                stale_config()
            } else {
                err.error.into()
            }
        })?;
        return Ok(None);
    }

    let backups = dir.join(BACKUP_DIR);
    if !check_directory(&backups)? {
        fs::create_dir_all(&backups)?;
        check_directory(&backups)?;
    }
    let backup_path = backups.join(format!("{}-{name}.bak", uuid::Uuid::new_v4()));
    if read_config_file(&target)?.as_deref() != baseline {
        return Err(stale_config());
    }
    replace_with_backup(staged, &target, &backup_path)?;
    Ok(Some(backup_path))
}

#[cfg(windows)]
fn replace_with_backup(
    staged: tempfile::NamedTempFile,
    target: &Path,
    backup: &Path,
) -> LauncherResult<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null;
    use windows_sys::Win32::Storage::FileSystem::{
        ReplaceFileW, SetFileAttributesW, FILE_ATTRIBUTE_NORMAL,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }
    let target_wide = wide(target);
    let backup_wide = wide(backup);
    let staged = staged.into_temp_path();
    let staged_wide = wide(&staged);
    if unsafe { SetFileAttributesW(staged_wide.as_ptr(), FILE_ATTRIBUTE_NORMAL) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if unsafe {
        ReplaceFileW(
            target_wide.as_ptr(),
            staged_wide.as_ptr(),
            backup_wide.as_ptr(),
            0,
            null(),
            null(),
        )
    } == 0
    {
        let err = std::io::Error::last_os_error();
        return Err(LauncherError::InvalidInput(format!(
            "Không thể thay file cấu hình: {err}. Kiểm tra file đích và bản sao lưu tại {}.",
            backup.display()
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_with_backup(
    staged: tempfile::NamedTempFile,
    target: &Path,
    backup: &Path,
) -> LauncherResult<()> {
    fs::hard_link(target, backup)?;
    staged.persist(target).map_err(|err| err.error)?;
    Ok(())
}

fn check_name(name: &str) -> LauncherResult<()> {
    let path = Path::new(name);
    if !is_safe_single_dir_component(name)
        || !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        return Err(LauncherError::InvalidInput(
            "Tên file cấu hình .json không hợp lệ.".into(),
        ));
    }
    Ok(())
}

fn check_directory(path: &Path) -> LauncherResult<bool> {
    match metadata(path)? {
        Some(meta) if meta.is_dir() && !is_reparse_point(&meta) => Ok(true),
        Some(_) => Err(unexpected_file(path)),
        None => Ok(false),
    }
}

fn file_metadata(path: &Path) -> LauncherResult<Option<Metadata>> {
    match metadata(path)? {
        Some(meta) if meta.is_file() && !is_reparse_point(&meta) => Ok(Some(meta)),
        Some(_) => Err(unexpected_file(path)),
        None => Ok(None),
    }
}

fn metadata(path: &Path) -> LauncherResult<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn is_reparse_point(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        meta.is_symlink()
    }
}

fn read_config_file(path: &Path) -> LauncherResult<Option<String>> {
    let Some(meta) = file_metadata(path)? else {
        return Ok(None);
    };
    if meta.len() > MAX_CONFIG_BYTES as u64 {
        return Err(file_too_large());
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(file_too_large());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| LauncherError::InvalidFormat("File cấu hình phải được mã hóa UTF-8.".into()))
}

fn file_too_large() -> LauncherError {
    LauncherError::InvalidInput(format!("File cấu hình vượt {MAX_CONFIG_BYTES} byte."))
}

fn stale_config() -> LauncherError {
    LauncherError::InvalidInput(
        "File trên đĩa đã thay đổi. Hãy sao chép phần đang chỉnh nếu cần, rồi mở lại file.".into(),
    )
}

fn unexpected_file(path: &Path) -> LauncherError {
    LauncherError::InvalidInput(format!(
        "Không thể dùng liên kết hoặc loại file không phù hợp: {}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_lists_imports_and_replaces_with_byte_exact_backup() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("ModConfigs");
        let name = "Example_Server.json";
        let initial = "\u{feff}{ \"Enabled\": true }\r\n";
        let updated = "{\"Enabled\":false}\n";
        assert!(list_mod_configs(&dir).unwrap().is_empty());
        assert!(save_mod_config(&dir, name, None, initial)
            .unwrap()
            .is_none());
        assert_eq!(list_mod_configs(&dir).unwrap(), [name]);
        assert_eq!(
            read_mod_config_import(&dir.join(name)).unwrap(),
            (name.into(), initial.into())
        );
        let backup = save_mod_config(&dir, name, Some(initial), updated)
            .unwrap()
            .unwrap();
        assert_eq!(backup.parent().unwrap(), dir.join(BACKUP_DIR));
        assert_eq!(fs::read(backup).unwrap(), initial.as_bytes());
        assert_eq!(
            read_mod_config(&dir, name).unwrap().as_deref(),
            Some(updated)
        );
    }

    #[test]
    fn refuses_invalid_paths_documents_and_stale_drafts() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        let name = "Mod_Config.json";
        assert!(save_mod_config(dir, "../outside.json", None, "{}").is_err());
        assert!(save_mod_config(dir, "NUL.json", None, "{}").is_err());
        assert!(save_mod_config(dir, "Mod_Config.json:alt", None, "{}").is_err());
        for invalid in ["[]", "{", "null", "{\"broken\":}"] {
            assert!(save_mod_config(dir, name, None, invalid).is_err());
        }
        assert!(!dir.join(name).exists());
        fs::write(dir.join(name), "{\"External\":true}").unwrap();
        assert!(save_mod_config(dir, name, None, "{}").is_err());
        assert!(save_mod_config(dir, name, Some("{}"), "{\"Draft\":true}").is_err());
        assert!(!dir.join(BACKUP_DIR).exists());
        assert_eq!(
            fs::read_to_string(dir.join(name)).unwrap(),
            "{\"External\":true}"
        );
    }
}
