pub mod binary_parsers;
pub mod config;
pub mod error;
pub mod modpack_share;
pub mod paths;

pub use binary_parsers::{
    activate_modpack, activate_modpack_with_options, backup_unsaved_enabled_mods_as_modpack,
    backup_world, backup_world_for_edition, compare_mod_version_strings, create_modpack,
    delete_modpack, inspect_world_backup_zip, is_safe_single_dir_component, list_modpacks,
    list_world_backups, load_enabled_mods, parse_tmod_file, parse_world_file,
    prepare_folder_modpack_for_server, restore_world_backup, save_enabled_mods,
    scan_tmod_directory, sha1_file_hex, sync_active_modpack_from_mods_dir,
    write_modpack_install_txt, ModPackInfo, ModSide, TmodMetadata, WorldBackupInfo,
    WorldMetadata,
};
pub use config::{
    decode_terraria_safe_arg, encode_terraria_safe_arg, GameEdition, GraphicsBackend,
    LauncherConfig, ProcessPriority, SavedJoinServer, ServerConfigData, ServerProfile, ThemeMode,
    WorldDifficultyOption, WorldSizeOption, DEFAULT_TERRARIA_INSTALL_SOURCE,
};
pub use error::{LauncherError, LauncherResult};
pub use modpack_share::{
    decode_modpack_share_key, encode_modpack_share_key, extract_source_hint_from_homepage,
    resolve_unique_modpack_name, sanitize_modpack_name, validate_share_payload,
    ModPackShareKeyPayload, SharedModEntry, SharedModSource, SHARE_KEY_PREFIX,
};
pub use paths::{
    detect_base_dir_for_exe, detect_environment, detect_exact_bundled_dotnet_runtime,
    is_dotnet8_ready_for_paths, is_xna_framework_installed, parse_dotnet_runtime_version_opt,
    query_local_dotnet8_runtime, query_system_dotnet8_runtime,
    read_declared_tml_dotnet_version_opt, version_at_least_patch, EnvironmentStatus,
    LauncherPaths,
};

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::DeflateEncoder;
    use flate2::Compression;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    fn write_dotnet_str(buf: &mut Vec<u8>, s: &str) {
        let mut len = s.len();
        loop {
            let mut byte = (len & 0x7F) as u8;
            len >>= 7;
            if len != 0 {
                byte |= 0x80;
            }
            buf.push(byte);
            if len == 0 {
                break;
            }
        }
        buf.extend_from_slice(s.as_bytes());
    }

    #[test]
    fn serverconfig_roundtrip_preserves_values() {
        let cfg = ServerConfigData {
            world: r"C:\TerrariaLauncher\data\Saves\tModLoader\Worlds\Test.wld".into(),
            autocreate: Some(3),
            seed: "for the worthy".into(),
            worldname: "Test".into(),
            difficulty: 2,
            maxplayers: 16,
            port: 7778,
            password: "secret".into(),
            motd: "Welcome!".into(),
            secure: true,
            upnp: false,
            modpack: "MyPack".into(),
            ..ServerConfigData::default()
        };
        let text = cfg.to_config_text(GameEdition::TModLoader);
        let reparsed = ServerConfigData::parse(&text);
        assert_eq!(reparsed.world, cfg.world);
        assert_eq!(reparsed.autocreate, Some(3));
        assert_eq!(reparsed.seed, "for the worthy");
        assert_eq!(reparsed.difficulty, 2);
        assert_eq!(reparsed.maxplayers, 16);
        assert_eq!(reparsed.port, 7778);
        assert_eq!(reparsed.password, "secret");
        assert!(!reparsed.upnp);
        assert_eq!(reparsed.modpack, "MyPack");
    }

    #[test]
    fn tmod_parser_reads_compressed_info_and_header() {
        let mut info = Vec::new();
        write_dotnet_str(&mut info, "displayName");
        write_dotnet_str(&mut info, "Example Mod");
        write_dotnet_str(&mut info, "author");
        write_dotnet_str(&mut info, "Terrarian");
        write_dotnet_str(&mut info, "modReferences");
        write_dotnet_str(&mut info, "BaseLib@1.0");
        write_dotnet_str(&mut info, "");
        write_dotnet_str(&mut info, "side");
        info.push(0);
        write_dotnet_str(&mut info, "");

        let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&info).unwrap();
        let compressed_info = enc.finish().unwrap();

        let mut tmod = Vec::new();
        tmod.extend_from_slice(b"TMOD");
        write_dotnet_str(&mut tmod, "2026.9.3.1");
        tmod.extend_from_slice(&[0xABu8; 20]);
        tmod.extend_from_slice(&[0u8; 256]);
        tmod.extend_from_slice(&128i32.to_le_bytes());
        write_dotnet_str(&mut tmod, "ExampleMod");
        write_dotnet_str(&mut tmod, "1.2.3");
        tmod.extend_from_slice(&1i32.to_le_bytes());
        write_dotnet_str(&mut tmod, "Info");
        tmod.extend_from_slice(&(info.len() as i32).to_le_bytes());
        tmod.extend_from_slice(&(compressed_info.len() as i32).to_le_bytes());
        tmod.extend_from_slice(&compressed_info);

        let tmp = std::env::temp_dir().join("tl_core_test_example.tmod");
        std::fs::write(&tmp, &tmod).unwrap();
        let parsed = parse_tmod_file(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        assert_eq!(parsed.internal_name, "ExampleMod");
        assert_eq!(parsed.display_name, "Example Mod");
        assert_eq!(parsed.author, "Terrarian");
        assert_eq!(parsed.version, "1.2.3");
        assert_eq!(parsed.mod_references, vec!["BaseLib@1.0"]);
        assert_eq!(parsed.side, ModSide::Both);
    }

    #[test]
    fn world_parser_reads_header_fields() {
        let mut header = Vec::new();
        write_dotnet_str(&mut header, "MyWorld");
        write_dotnet_str(&mut header, "123456");
        header.extend_from_slice(&1u64.to_le_bytes());
        header.extend_from_slice(&[7u8; 16]);
        header.extend_from_slice(&42i32.to_le_bytes());
        header.extend_from_slice(&[0u8; 16]);
        header.extend_from_slice(&1800i32.to_le_bytes());
        header.extend_from_slice(&6400i32.to_le_bytes());
        header.extend_from_slice(&2i32.to_le_bytes());
        header.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 1]);

        let mut wld = Vec::new();
        wld.extend_from_slice(&279i32.to_le_bytes());
        wld.extend_from_slice(b"relogic");
        wld.push(2);
        wld.extend_from_slice(&5u32.to_le_bytes());
        wld.extend_from_slice(&1u64.to_le_bytes());
        wld.extend_from_slice(&1i16.to_le_bytes());
        let ptr_pos = wld.len();
        wld.extend_from_slice(&0i32.to_le_bytes());
        let header_offset = wld.len() as i32;
        wld[ptr_pos..ptr_pos + 4].copy_from_slice(&header_offset.to_le_bytes());
        wld.extend_from_slice(&header);

        let tmp: PathBuf = std::env::temp_dir().join("tl_core_test_world.wld");
        std::fs::write(&tmp, &wld).unwrap();
        let parsed = parse_world_file(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        assert_eq!(parsed.world_name, "MyWorld");
        assert_eq!(parsed.seed, "123456");
        assert_eq!(parsed.world_id, 42);
        assert_eq!(parsed.max_tiles_x, 6400);
        assert_eq!(parsed.size_label(), "Medium");
        assert_eq!(parsed.difficulty_label(), "Master");
        assert!(parsed.is_favorite);
        assert_eq!(parsed.special_seeds, vec!["Drunk", "Zenith"]);
    }

    fn make_temp_test_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{prefix}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn dummy_tmod_meta(internal_name: &str, file_name: &str) -> TmodMetadata {
        TmodMetadata {
            file_path: PathBuf::from(file_name),
            file_name: file_name.to_string(),
            file_size: 4,
            tmodloader_version: "2026.9.3.1".to_string(),
            internal_name: internal_name.to_string(),
            display_name: internal_name.to_string(),
            version: "1.0.0".to_string(),
            author: String::new(),
            description: String::new(),
            homepage: String::new(),
            mod_references: Vec::new(),
            weak_references: Vec::new(),
            side: ModSide::Both,
            library_mod: false,
            sha1_hex: String::new(),
            icon_png: None,
        }
    }

    #[test]
    fn modpack_share_key_roundtrip_and_whitespace_tolerance() {
        let payload = ModPackShareKeyPayload {
            v: 1,
            name: "Calamity Co-op".to_string(),
            mods: vec![
                SharedModEntry {
                    internal_name: "CalamityMod".to_string(),
                    display_name: "Calamity Mod".to_string(),
                    version: "2.0.4.5".to_string(),
                    source: SharedModSource::Workshop {
                        id: "2824688072".to_string(),
                    },
                    enabled: true,
                },
                SharedModEntry {
                    internal_name: "RecipeBrowser".to_string(),
                    display_name: String::new(),
                    version: String::new(),
                    source: SharedModSource::Github {
                        repo: "JavidPack/RecipeBrowser".to_string(),
                        url: String::new(),
                    },
                    enabled: true,
                },
                SharedModEntry {
                    internal_name: "MagicStorage".to_string(),
                    display_name: "Magic Storage".to_string(),
                    version: "0.7.0".to_string(),
                    source: SharedModSource::Github {
                        repo: "blushiemagic/MagicStorage".to_string(),
                        url: "https://github.com/blushiemagic/MagicStorage/releases/download/v0.7.0/MagicStorage.tmod".to_string(),
                    },
                    enabled: true,
                },
                SharedModEntry {
                    internal_name: "CustomUrlMod".to_string(),
                    display_name: String::new(),
                    version: String::new(),
                    source: SharedModSource::Url {
                        url: "https://cdn.example.com/mods/CustomUrlMod.tmod?dl=1".to_string(),
                    },
                    enabled: true,
                },
                SharedModEntry {
                    internal_name: "LocalOnlyMod".to_string(),
                    display_name: "Local Mod".to_string(),
                    version: "1.0".to_string(),
                    source: SharedModSource::ManualOnly,
                    enabled: true,
                },
            ],
        };

        let key = encode_modpack_share_key(&payload).unwrap();
        assert!(key.starts_with(SHARE_KEY_PREFIX));
        assert!(!key.contains('='));

        let compact_json = serde_json::to_string(&payload).unwrap();
        assert!(compact_json.contains(r#""k":"w""#));
        assert!(compact_json.contains(r#""k":"g""#));
        assert!(compact_json.contains(r#""k":"u""#));
        assert!(compact_json.contains(r#""k":"m""#));
        assert!(!compact_json.contains(r#""d":"""#));

        let mid = key.len() / 2;
        let wrapped = format!("  {}\r\n\t{}  \n", &key[..mid], &key[mid..]);
        let decoded = decode_modpack_share_key(&wrapped).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn modpack_share_key_rejects_corrupted_checksum_and_bad_prefix() {
        let payload = ModPackShareKeyPayload {
            v: 1,
            name: "ValidPack".to_string(),
            mods: vec![SharedModEntry {
                internal_name: "BossChecklist".to_string(),
                display_name: String::new(),
                version: String::new(),
                source: SharedModSource::Workshop {
                    id: "2563309347".to_string(),
                },
                enabled: true,
            }],
        };
        let key = encode_modpack_share_key(&payload).unwrap();

        let mut chars: Vec<char> = key.chars().collect();
        let last = chars.last_mut().unwrap();
        *last = if *last == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        assert!(decode_modpack_share_key(&tampered).is_err());

        assert!(decode_modpack_share_key("BADPREFIX:AAAA").is_err());
        assert!(decode_modpack_share_key("TLPACK1:AAAA").is_err());
    }

    #[test]
    fn modpack_share_key_rejects_deflate_bomb_and_invalid_payloads() {
        use sha2::{Digest, Sha256};

        let bomb_raw = vec![b' '; 600 * 1024];
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::best());
        enc.write_all(&bomb_raw).unwrap();
        let mut packed = enc.finish().unwrap();
        let digest = Sha256::digest(&packed);
        packed.extend_from_slice(&digest[..4]);

        let b64 = modpack_share::base64url_encode(&packed);
        let bomb_key = format!("{SHARE_KEY_PREFIX}{b64}");
        assert!(decode_modpack_share_key(&bomb_key).is_err());

        let bad_version = ModPackShareKeyPayload {
            v: 2,
            name: "Pack".to_string(),
            mods: vec![SharedModEntry {
                internal_name: "ModA".to_string(),
                display_name: String::new(),
                version: String::new(),
                source: SharedModSource::ManualOnly,
                enabled: true,
            }],
        };
        assert!(encode_modpack_share_key(&bad_version).is_err());

        let empty_mods = ModPackShareKeyPayload {
            v: 1,
            name: "Pack".to_string(),
            mods: Vec::new(),
        };
        assert!(encode_modpack_share_key(&empty_mods).is_err());

        let bad_workshop = ModPackShareKeyPayload {
            v: 1,
            name: "Pack".to_string(),
            mods: vec![SharedModEntry {
                internal_name: "ModA".to_string(),
                display_name: String::new(),
                version: String::new(),
                source: SharedModSource::Workshop {
                    id: "123abc".to_string(),
                },
                enabled: true,
            }],
        };
        assert!(encode_modpack_share_key(&bad_workshop).is_err());

        let bad_github = ModPackShareKeyPayload {
            v: 1,
            name: "Pack".to_string(),
            mods: vec![SharedModEntry {
                internal_name: "ModA".to_string(),
                display_name: String::new(),
                version: String::new(),
                source: SharedModSource::Github {
                    repo: "owner/repo/extra".to_string(),
                    url: String::new(),
                },
                enabled: true,
            }],
        };
        assert!(encode_modpack_share_key(&bad_github).is_err());

        let bad_github_url = ModPackShareKeyPayload {
            v: 1,
            name: "Pack".to_string(),
            mods: vec![SharedModEntry {
                internal_name: "ModA".to_string(),
                display_name: String::new(),
                version: String::new(),
                source: SharedModSource::Github {
                    repo: "owner/repo".to_string(),
                    url: "ftp://example.com/ModA.tmod".to_string(),
                },
                enabled: true,
            }],
        };
        assert!(encode_modpack_share_key(&bad_github_url).is_err());

        let bad_url = ModPackShareKeyPayload {
            v: 1,
            name: "Pack".to_string(),
            mods: vec![SharedModEntry {
                internal_name: "ModA".to_string(),
                display_name: String::new(),
                version: String::new(),
                source: SharedModSource::Url {
                    url: "javascript:alert(1)".to_string(),
                },
                enabled: true,
            }],
        };
        assert!(encode_modpack_share_key(&bad_url).is_err());
    }

    #[test]
    fn extract_source_hint_parses_workshop_github_and_tmod_urls() {
        assert_eq!(
            extract_source_hint_from_homepage(
                "https://steamcommunity.com/sharedfiles/filedetails/?id=2824688072"
            ),
            Some(SharedModSource::Workshop {
                id: "2824688072".to_string()
            })
        );
        assert_eq!(
            extract_source_hint_from_homepage(
                "http://steamcommunity.com/sharedfiles/filedetails/?l=english&id=2563309347&searchtext=boss"
            ),
            Some(SharedModSource::Workshop {
                id: "2563309347".to_string()
            })
        );
        assert_eq!(
            extract_source_hint_from_homepage("https://github.com/JavidPack/RecipeBrowser/"),
            Some(SharedModSource::Github {
                repo: "JavidPack/RecipeBrowser".to_string(),
                url: String::new(),
            })
        );
        assert_eq!(
            extract_source_hint_from_homepage(
                "https://github.com/JavidPack/RecipeBrowser/releases/download/v0.10/RecipeBrowser.tmod"
            ),
            Some(SharedModSource::Url {
                url: "https://github.com/JavidPack/RecipeBrowser/releases/download/v0.10/RecipeBrowser.tmod".to_string()
            })
        );
        assert_eq!(
            extract_source_hint_from_homepage("https://cdn.example.com/MyMod.TMOD?token=abc"),
            Some(SharedModSource::Url {
                url: "https://cdn.example.com/MyMod.TMOD?token=abc".to_string()
            })
        );

        assert_eq!(
            extract_source_hint_from_homepage("https://github.com/JavidPack/RecipeBrowser/wiki"),
            None
        );
        assert_eq!(
            extract_source_hint_from_homepage("https://steamcommunity.com/id/author"),
            None
        );
        assert_eq!(
            extract_source_hint_from_homepage("https://example.com/homepage"),
            None
        );
        assert_eq!(extract_source_hint_from_homepage("   "), None);
    }

    #[test]
    fn sanitize_and_resolve_unique_modpack_name_work_safely() {
        assert_eq!(sanitize_modpack_name("  My Pack  ").unwrap(), "My Pack");
        assert!(sanitize_modpack_name("").is_err());
        assert!(sanitize_modpack_name("   ").is_err());
        assert!(sanitize_modpack_name("..").is_err());
        assert!(sanitize_modpack_name("foo..bar").is_err());
        assert!(sanitize_modpack_name(".hidden").is_err());
        assert!(sanitize_modpack_name("trailing.").is_err());
        assert!(sanitize_modpack_name("bad/name").is_err());
        assert!(sanitize_modpack_name(r"bad\name").is_err());
        assert!(sanitize_modpack_name("bad:name").is_err());
        assert!(sanitize_modpack_name("CON").is_err());

        let long_unicode = "ộ".repeat(95);
        let capped = sanitize_modpack_name(&long_unicode).unwrap();
        assert_eq!(capped.chars().count(), 80);

        let root = make_temp_test_dir("tl_core_unique_name");
        assert_eq!(
            resolve_unique_modpack_name(&root, "Calamity").unwrap(),
            "Calamity"
        );
        std::fs::create_dir_all(root.join("Calamity")).unwrap();
        assert_eq!(
            resolve_unique_modpack_name(&root, "Calamity").unwrap(),
            "Calamity_2"
        );
        std::fs::create_dir_all(root.join("Calamity_2")).unwrap();
        assert_eq!(
            resolve_unique_modpack_name(&root, "Calamity").unwrap(),
            "Calamity_3"
        );
        assert!(create_modpack(&root, &root, "../escape", &[], &[]).is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_modpack_install_txt_dedups_and_filters() {
        let root = make_temp_test_dir("tl_core_install_txt");
        let pack_dir = root.join("PackA");
        write_modpack_install_txt(
            &pack_dir,
            &[
                " 2824688072 ".to_string(),
                "".to_string(),
                "invalid_id".to_string(),
                "2563309347".to_string(),
                "2824688072".to_string(),
                "123abc".to_string(),
            ],
        )
        .unwrap();

        let written = std::fs::read_to_string(pack_dir.join("Mods").join("install.txt")).unwrap();
        assert_eq!(written, "2824688072\n2563309347\n");

        let packs = list_modpacks(&root).unwrap();
        assert_eq!(packs.len(), 1);
        assert_eq!(
            packs[0].install_workshop_ids,
            vec!["2824688072", "2563309347"]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn backup_unsaved_enabled_mods_isolates_only_when_needed() {
        let root = make_temp_test_dir("tl_core_backup_unsaved");
        let modpacks_dir = root.join("ModPacks");
        let mods_dir = root.join("Mods");
        std::fs::create_dir_all(&modpacks_dir).unwrap();
        std::fs::create_dir_all(&mods_dir).unwrap();

        let metas = vec![
            dummy_tmod_meta("ModA", "ModA.tmod"),
            dummy_tmod_meta("ModB", "ModB.tmod"),
        ];
        std::fs::write(mods_dir.join("ModA.tmod"), b"TMODA").unwrap();
        std::fs::write(mods_dir.join("ModB.tmod"), b"TMODB").unwrap();

        // No enabled.json yet -> nothing to back up
        assert!(backup_unsaved_enabled_mods_as_modpack(
            &modpacks_dir,
            &mods_dir,
            "",
            &metas,
            "AutoBackup",
        )
        .unwrap()
        .is_none());

        save_enabled_mods(
            &mods_dir.join("enabled.json"),
            &["ModA".to_string(), "ModB".to_string()],
        )
        .unwrap();

        // Active modpack missing on disk + no existing pack matches -> creates backup
        let created = backup_unsaved_enabled_mods_as_modpack(
            &modpacks_dir,
            &mods_dir,
            "DeletedPack",
            &metas,
            "AutoBackup",
        )
        .unwrap()
        .expect("expected backup pack to be created");
        assert_eq!(created.name, "AutoBackup");
        assert_eq!(created.local_tmod_count, 2);
        assert!(created.path.join("Mods").join("ModA.tmod").is_file());
        assert!(created.path.join("Mods").join("ModB.tmod").is_file());

        // Calling again with same enabled set (different order and ASCII case) -> None
        save_enabled_mods(
            &mods_dir.join("enabled.json"),
            &["modb".to_string(), "MODA".to_string()],
        )
        .unwrap();
        assert!(backup_unsaved_enabled_mods_as_modpack(
            &modpacks_dir,
            &mods_dir,
            "",
            &metas,
            "AutoBackup",
        )
        .unwrap()
        .is_none());

        // When active_modpack exists on disk, update it in place and return Ok(None)
        save_enabled_mods(&mods_dir.join("enabled.json"), &["ModA".to_string()]).unwrap();
        assert!(backup_unsaved_enabled_mods_as_modpack(
            &modpacks_dir,
            &mods_dir,
            "AutoBackup",
            &metas,
            "AutoBackup",
        )
        .unwrap()
        .is_none());
        assert_eq!(
            load_enabled_mods(&modpacks_dir.join("AutoBackup").join("Mods").join("enabled.json"))
                .unwrap(),
            vec!["ModA".to_string()]
        );
        assert!(modpacks_dir
            .join("AutoBackup")
            .join("Mods")
            .join("ModA.tmod")
            .is_file());
        assert!(!modpacks_dir
            .join("AutoBackup")
            .join("Mods")
            .join("ModB.tmod")
            .exists());
        assert!(!modpacks_dir.join("AutoBackup_2").exists());

        // When active_modpack is empty and enabled set does not match any existing pack -> creates AutoBackup_2
        save_enabled_mods(&mods_dir.join("enabled.json"), &["ModB".to_string()]).unwrap();
        let second = backup_unsaved_enabled_mods_as_modpack(
            &modpacks_dir,
            &mods_dir,
            "",
            &metas,
            "AutoBackup",
        )
        .unwrap()
        .expect("expected second unique backup pack when enabled set drifted without active pack");
        assert_eq!(second.name, "AutoBackup_2");
        assert_eq!(second.local_tmod_count, 1);

        // Calling again after AutoBackup_2 already matches -> None
        assert!(backup_unsaved_enabled_mods_as_modpack(
            &modpacks_dir,
            &mods_dir,
            "",
            &metas,
            "AutoBackup",
        )
        .unwrap()
        .is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn launcher_config_legacy_json_defaults_theme_mode_to_dark() {
        let legacy_json = r#"{
            "active_edition": "Vanilla",
            "active_modpack": "LegacyPack",
            "quick_join_host": "127.0.0.1",
            "quick_join_port": 7777
        }"#;

        let parsed: LauncherConfig = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(parsed.active_edition, GameEdition::Vanilla);
        assert_eq!(parsed.active_modpack, "LegacyPack");
        assert_eq!(parsed.quick_join_host, "127.0.0.1");
        assert_eq!(parsed.theme_mode, ThemeMode::Dark);
        assert_eq!(LauncherConfig::default().theme_mode, ThemeMode::Dark);
    }

    #[test]
    fn launcher_config_theme_mode_light_roundtrips() {
        let cfg = LauncherConfig {
            theme_mode: ThemeMode::Light,
            ..LauncherConfig::default()
        };

        let json = serde_json::to_string_pretty(&cfg).unwrap();
        assert!(json.contains(r#""theme_mode": "Light""#));
        let reparsed: LauncherConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(reparsed.theme_mode, ThemeMode::Light);
    }

    fn write_minimal_tmod(path: &Path, internal_name: &str, version: &str) {
        let mut tmod = Vec::new();
        tmod.extend_from_slice(b"TMOD");
        write_dotnet_str(&mut tmod, "2026.9.3.1");
        tmod.extend_from_slice(&[0x11u8; 20]);
        tmod.extend_from_slice(&[0u8; 256]);
        tmod.extend_from_slice(&64i32.to_le_bytes());
        write_dotnet_str(&mut tmod, internal_name);
        write_dotnet_str(&mut tmod, version);
        tmod.extend_from_slice(&0i32.to_le_bytes());
        std::fs::write(path, &tmod).unwrap();
    }

    #[test]
    fn prepare_folder_modpack_for_server_copies_enabled_only_and_validates_paths() {
        let root = make_temp_test_dir("tl_core_prepare_server_pack");
        let modpacks_dir = root.join("ModPacks");
        let mods_dir = root.join("Mods");
        std::fs::create_dir_all(&modpacks_dir).unwrap();
        std::fs::create_dir_all(&mods_dir).unwrap();

        let shared_enabled_path = mods_dir.join("enabled.json");
        let shared_enabled_raw = "[\n  \"DisabledMod\"\n]";
        std::fs::write(&shared_enabled_path, shared_enabled_raw).unwrap();

        write_minimal_tmod(&mods_dir.join("EnabledMissing.tmod"), "EnabledMissing", "1.0.0");
        write_minimal_tmod(&mods_dir.join("EnabledAlready.tmod"), "EnabledAlready", "2.0.0");
        write_minimal_tmod(&mods_dir.join("DisabledMod.tmod"), "DisabledMod", "1.0.0");

        let pack_mods = modpacks_dir.join("ServerPack").join("Mods");
        std::fs::create_dir_all(&pack_mods).unwrap();
        save_enabled_mods(
            &pack_mods.join("enabled.json"),
            &["EnabledMissing".to_string(), "EnabledAlready".to_string()],
        )
        .unwrap();
        write_minimal_tmod(&pack_mods.join("EnabledAlready.tmod"), "EnabledAlready", "3.0.0");

        let prepared =
            prepare_folder_modpack_for_server(&modpacks_dir, &mods_dir, " ServerPack ").unwrap();
        assert_eq!(prepared.as_deref(), Some(pack_mods.as_path()));
        assert!(pack_mods.join("EnabledMissing.tmod").is_file());
        assert_eq!(
            parse_tmod_file(&pack_mods.join("EnabledAlready.tmod"))
                .unwrap()
                .version,
            "3.0.0"
        );
        assert!(!pack_mods.join("DisabledMod.tmod").exists());
        assert_eq!(
            std::fs::read_to_string(&shared_enabled_path).unwrap(),
            shared_enabled_raw
        );

        assert_eq!(
            prepare_folder_modpack_for_server(&modpacks_dir, &mods_dir, "   ").unwrap(),
            None
        );
        assert_eq!(
            prepare_folder_modpack_for_server(&modpacks_dir, &mods_dir, "MissingPack").unwrap(),
            None
        );
        assert!(matches!(
            prepare_folder_modpack_for_server(&modpacks_dir, &mods_dir, "../escape"),
            Err(LauncherError::InvalidInput(_))
        ));
        assert!(matches!(
            prepare_folder_modpack_for_server(&modpacks_dir, &mods_dir, r"bad\pack"),
            Err(LauncherError::InvalidInput(_))
        ));
        assert!(matches!(
            prepare_folder_modpack_for_server(&modpacks_dir, &mods_dir, "C:pack"),
            Err(LauncherError::InvalidInput(_))
        ));
        assert!(matches!(
            prepare_folder_modpack_for_server(&modpacks_dir, &mods_dir, "."),
            Err(LauncherError::InvalidInput(_))
        ));
        assert!(matches!(
            prepare_folder_modpack_for_server(&modpacks_dir, &mods_dir, ".."),
            Err(LauncherError::InvalidInput(_))
        ));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn theme_mode_toggle_flips_between_dark_and_light() {
        assert_eq!(ThemeMode::default(), ThemeMode::Dark);
        assert!(ThemeMode::Dark.is_dark());
        assert!(!ThemeMode::Light.is_dark());
        assert_eq!(ThemeMode::Dark.toggle(), ThemeMode::Light);
        assert_eq!(ThemeMode::Light.toggle(), ThemeMode::Dark);
    }

    fn build_test_wld_bytes(format_version: i32, world_name: &str, seed_flags: &[u8]) -> Vec<u8> {
        let mut header = Vec::new();
        write_dotnet_str(&mut header, world_name);
        write_dotnet_str(&mut header, "123456");
        header.extend_from_slice(&1u64.to_le_bytes());
        header.extend_from_slice(&[7u8; 16]);
        header.extend_from_slice(&42i32.to_le_bytes());
        header.extend_from_slice(&[0u8; 16]);
        header.extend_from_slice(&1800i32.to_le_bytes());
        header.extend_from_slice(&6400i32.to_le_bytes());
        header.extend_from_slice(&0i32.to_le_bytes());
        header.extend_from_slice(seed_flags);

        let mut wld = Vec::new();
        wld.extend_from_slice(&format_version.to_le_bytes());
        wld.extend_from_slice(b"relogic");
        wld.push(2);
        wld.extend_from_slice(&5u32.to_le_bytes());
        wld.extend_from_slice(&1u64.to_le_bytes());
        wld.extend_from_slice(&1i16.to_le_bytes());
        let ptr_pos = wld.len();
        wld.extend_from_slice(&0i32.to_le_bytes());
        let header_offset = wld.len() as i32;
        wld[ptr_pos..ptr_pos + 4].copy_from_slice(&header_offset.to_le_bytes());
        wld.extend_from_slice(&header);
        wld
    }

    #[test]
    fn world_parser_reads_skyblock_seed_302() {
        let wld = build_test_wld_bytes(302, "SkyWorld", &[0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let tmp = std::env::temp_dir().join(format!("tl_core_sky_{}.wld", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, &wld).unwrap();
        let parsed = parse_world_file(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        assert_eq!(parsed.format_version, 302);
        assert_eq!(parsed.special_seeds, vec!["Skyblock"]);
    }

    #[test]
    fn create_modpack_removes_stale_tmods() {
        let root = make_temp_test_dir("tl_core_create_stale");
        let modpacks_dir = root.join("ModPacks");
        let mods_dir = root.join("Mods");
        std::fs::create_dir_all(&mods_dir).unwrap();

        write_minimal_tmod(&mods_dir.join("ModA.tmod"), "ModA", "1.0.0");
        let meta_a = parse_tmod_file(&mods_dir.join("ModA.tmod")).unwrap();

        let pack_mods = modpacks_dir.join("MyPack").join("Mods");
        std::fs::create_dir_all(&pack_mods).unwrap();
        write_minimal_tmod(&pack_mods.join("RenamedStale.tmod"), "RemovedMod", "1.0.0");
        std::fs::write(pack_mods.join("CorruptStale.tmod"), b"BAD").unwrap();

        let created = create_modpack(
            &modpacks_dir,
            &mods_dir,
            "MyPack",
            &["ModA".to_string()],
            &[meta_a],
        )
        .unwrap();

        assert_eq!(created.local_tmod_count, 1);
        assert!(pack_mods.join("ModA.tmod").is_file());
        assert!(!pack_mods.join("RenamedStale.tmod").exists());
        assert!(!pack_mods.join("CorruptStale.tmod").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_modpack_matches_enabled_mods_case_insensitively() {
        let root = make_temp_test_dir("tl_core_create_ci");
        let modpacks_dir = root.join("ModPacks");
        let mods_dir = root.join("Mods");
        std::fs::create_dir_all(&mods_dir).unwrap();

        write_minimal_tmod(&mods_dir.join("ModAlpha.tmod"), "ModAlpha", "1.0.0");
        write_minimal_tmod(&mods_dir.join("ModBeta.tmod"), "ModBeta", "1.0.0");
        let meta_alpha = parse_tmod_file(&mods_dir.join("ModAlpha.tmod")).unwrap();
        let meta_beta = parse_tmod_file(&mods_dir.join("ModBeta.tmod")).unwrap();

        let pack_mods = modpacks_dir.join("CasePack").join("Mods");
        std::fs::create_dir_all(&pack_mods).unwrap();
        write_minimal_tmod(&pack_mods.join("ModBeta.tmod"), "ModBeta", "1.0.0");
        write_minimal_tmod(&pack_mods.join("StaleMod.tmod"), "StaleMod", "1.0.0");

        let created = create_modpack(
            &modpacks_dir,
            &mods_dir,
            "CasePack",
            &["  modalpha  ".to_string()],
            &[meta_alpha, meta_beta],
        )
        .unwrap();

        assert_eq!(created.local_tmod_count, 1);
        assert!(pack_mods.join("ModAlpha.tmod").is_file());
        assert!(!pack_mods.join("ModBeta.tmod").exists());
        assert!(!pack_mods.join("StaleMod.tmod").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sync_active_modpack_from_mods_dir_updates_copies_and_deletes_removed() {
        let root = make_temp_test_dir("tl_core_sync_active");
        let modpacks_dir = root.join("ModPacks");
        let mods_dir = root.join("Mods");
        std::fs::create_dir_all(&mods_dir).unwrap();

        let pack_mods = modpacks_dir.join("ActivePack").join("Mods");
        std::fs::create_dir_all(&pack_mods).unwrap();
        write_minimal_tmod(&pack_mods.join("SameSize.tmod"), "SameSize", "1.0.0");
        write_minimal_tmod(&pack_mods.join("Outdated.tmod"), "Outdated", "1.0.0");
        write_minimal_tmod(&pack_mods.join("CustomFile.tmod"), "RemovedMod", "1.0.0");

        write_minimal_tmod(&mods_dir.join("SameSize.tmod"), "SameSize", "1.0.0");
        write_minimal_tmod(&mods_dir.join("Outdated.tmod"), "Outdated", "10.0.0.1");
        write_minimal_tmod(&mods_dir.join("NewEnabled.tmod"), "NewEnabled", "1.0.0");
        write_minimal_tmod(&mods_dir.join("DisabledMod.tmod"), "DisabledMod", "1.0.0");

        save_enabled_mods(
            &mods_dir.join("enabled.json"),
            &[
                "SameSize".to_string(),
                "Outdated".to_string(),
                "NewEnabled".to_string(),
            ],
        )
        .unwrap();

        sync_active_modpack_from_mods_dir(
            &modpacks_dir,
            &mods_dir,
            " ActivePack ",
            Some("removedmod"),
        )
        .unwrap();

        assert_eq!(
            load_enabled_mods(&pack_mods.join("enabled.json")).unwrap(),
            vec!["SameSize", "Outdated", "NewEnabled"]
        );
        assert!(!pack_mods.join("CustomFile.tmod").exists());
        assert!(pack_mods.join("NewEnabled.tmod").is_file());
        assert!(!pack_mods.join("DisabledMod.tmod").exists());
        assert_eq!(
            std::fs::metadata(pack_mods.join("Outdated.tmod"))
                .unwrap()
                .len(),
            std::fs::metadata(mods_dir.join("Outdated.tmod"))
                .unwrap()
                .len()
        );

        sync_active_modpack_from_mods_dir(&modpacks_dir, &mods_dir, "../escape", None).unwrap();
        sync_active_modpack_from_mods_dir(&modpacks_dir, &mods_dir, "MissingPack", None).unwrap();

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn activate_modpack_overwrites_differing_tmod_sha1_and_skips_identical() {
        let root = make_temp_test_dir("tl_core_activate_sha1");
        let pack_mods = root.join("Pack").join("Mods");
        let target_mods = root.join("TargetMods");
        std::fs::create_dir_all(&pack_mods).unwrap();
        std::fs::create_dir_all(&target_mods).unwrap();

        save_enabled_mods(
            &pack_mods.join("enabled.json"),
            &["ModA".to_string(), "ModB".to_string(), "ModC".to_string()],
        )
        .unwrap();
        std::fs::write(pack_mods.join("ModA.tmod"), b"PACK_VERSION_A2").unwrap();
        std::fs::write(pack_mods.join("ModB.tmod"), b"MISSING_IN_TARGET").unwrap();
        std::fs::write(pack_mods.join("ModC.tmod"), b"SAME_CONTENT_12").unwrap();

        std::fs::write(target_mods.join("ModA.tmod"), b"OLD__VERSION_A1").unwrap();
        std::fs::write(target_mods.join("ModC.tmod"), b"SAME_CONTENT_12").unwrap();

        let enabled = activate_modpack(&root.join("Pack"), &target_mods, true).unwrap();
        assert_eq!(enabled, vec!["ModA", "ModB", "ModC"]);
        assert_eq!(
            std::fs::read(target_mods.join("ModA.tmod")).unwrap(),
            b"PACK_VERSION_A2"
        );
        assert_eq!(
            std::fs::read(target_mods.join("ModB.tmod")).unwrap(),
            b"MISSING_IN_TARGET"
        );
        assert_eq!(
            std::fs::read(target_mods.join("ModC.tmod")).unwrap(),
            b"SAME_CONTENT_12"
        );
        let leftover_tmp = std::fs::read_dir(&target_mods)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".tl_atomic_"));
        assert!(!leftover_tmp);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn is_safe_single_dir_component_validates_single_safe_names() {
        assert!(is_safe_single_dir_component("default-tml"));
        assert!(is_safe_single_dir_component("My Pack"));
        assert!(is_safe_single_dir_component("Calamity_2"));
        assert!(!is_safe_single_dir_component(""));
        assert!(!is_safe_single_dir_component(" srv "));
        assert!(!is_safe_single_dir_component("."));
        assert!(!is_safe_single_dir_component(".."));
        assert!(!is_safe_single_dir_component("../escape"));
        assert!(!is_safe_single_dir_component("a/b"));
        assert!(!is_safe_single_dir_component(r"a\b"));
        assert!(!is_safe_single_dir_component("C:srv"));
        assert!(!is_safe_single_dir_component("CON"));
        assert!(!is_safe_single_dir_component(".hidden"));
        assert!(!is_safe_single_dir_component("trailing."));
        assert!(!is_safe_single_dir_component("foo..bar"));
    }

    #[test]
    fn world_backup_inspect_list_disambiguation_and_stale_twld_cleanup() {
        let root = make_temp_test_dir("tl_core_world_backup");
        let vanilla_dir = root.join("VanillaWorlds");
        let tml_dir = root.join("TmlWorlds");
        let backups_dir = root.join("Backups");
        std::fs::create_dir_all(&vanilla_dir).unwrap();
        std::fs::create_dir_all(&tml_dir).unwrap();

        let vanilla_wld = vanilla_dir.join("SkyWorld.wld");
        std::fs::write(
            &vanilla_wld,
            build_test_wld_bytes(280, "SkyWorld", &[0, 0, 0, 0, 0, 0, 0, 0]),
        )
        .unwrap();
        std::fs::write(vanilla_dir.join("SkyWorld.wld.bak"), b"BAK1").unwrap();
        std::fs::write(vanilla_dir.join("SkyWorld.wld.bak2"), b"BAK2").unwrap();
        std::fs::write(vanilla_dir.join("SkyWorld.wld.bak9"), b"BAK9").unwrap();

        let b1 = backup_world(&vanilla_wld, &backups_dir).unwrap();
        let b2 = backup_world(&vanilla_wld, &backups_dir).unwrap();
        assert_ne!(b1.file_name, b2.file_name);
        assert_eq!(b1.detected_edition, Some(GameEdition::Vanilla));
        assert_eq!(b1.primary_world_file.as_deref(), Some("SkyWorld.wld"));
        assert!(b1.modified_display.ends_with(" UTC"));
        assert_eq!(b1.modified_display.len(), "2026-11-15 12:34:56 UTC".len());

        let (ed, fname, has_twld) = inspect_world_backup_zip(&b1.file_path);
        assert_eq!(ed, Some(GameEdition::Vanilla));
        assert_eq!(fname.as_deref(), Some("SkyWorld.wld"));
        assert!(!has_twld);

        let listed = list_world_backups(&backups_dir).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|b| {
            b.detected_edition == Some(GameEdition::Vanilla)
                && b.primary_world_file.as_deref() == Some("SkyWorld.wld")
                && b.modified_display.len() == "2026-11-15 12:34:56 UTC".len()
        }));

        let target_restore = root.join("RestoreTarget");
        std::fs::create_dir_all(&target_restore).unwrap();
        std::fs::write(target_restore.join("SkyWorld.twld"), b"STALE_TWLD").unwrap();
        std::fs::write(target_restore.join("SkyWorld.twld.bak"), b"STALE_TWLD_BAK").unwrap();

        let restored = restore_world_backup(&b1.file_path, &target_restore).unwrap();
        assert!(restored.contains(&"SkyWorld.wld".to_string()));
        assert!(restored.contains(&"SkyWorld.wld.bak".to_string()));
        assert!(restored.contains(&"SkyWorld.wld.bak2".to_string()));
        assert!(restored.contains(&"SkyWorld.wld.bak9".to_string()));
        assert!(!target_restore.join("SkyWorld.twld").exists());
        assert!(!target_restore.join("SkyWorld.twld.bak").exists());

        let modded_wld = tml_dir.join("ModWorld.wld");
        std::fs::write(
            &modded_wld,
            build_test_wld_bytes(279, "ModWorld", &[0, 0, 0, 0, 0, 0, 0, 0]),
        )
        .unwrap();
        std::fs::write(tml_dir.join("ModWorld.twld"), b"TWLD_DATA").unwrap();
        std::fs::write(tml_dir.join("ModWorld.twld.bak"), b"TWLD_BAK").unwrap();

        let modded_backup = backup_world(&modded_wld, &backups_dir).unwrap();
        assert_eq!(modded_backup.detected_edition, Some(GameEdition::TModLoader));
        assert_eq!(
            modded_backup.primary_world_file.as_deref(),
            Some("ModWorld.wld")
        );
        let (_, _, modded_has_twld) = inspect_world_backup_zip(&modded_backup.file_path);
        assert!(modded_has_twld);

        let modded_restore_dir = root.join("ModdedRestoreTarget");
        let modded_restored =
            restore_world_backup(&modded_backup.file_path, &modded_restore_dir).unwrap();
        assert!(modded_restored.contains(&"ModWorld.twld".to_string()));
        assert!(modded_restored.contains(&"ModWorld.twld.bak".to_string()));
        assert!(modded_restore_dir.join("ModWorld.twld").is_file());

        let shared_wld = tml_dir.join("CompatWorld.wld");
        std::fs::write(
            &shared_wld,
            build_test_wld_bytes(279, "CompatWorld", &[0, 0, 0, 0, 0, 0, 0, 0]),
        )
        .unwrap();
        let compat_backup = backup_world(&shared_wld, &backups_dir).unwrap();
        assert_eq!(compat_backup.detected_edition, Some(GameEdition::Vanilla));
        assert_eq!(
            compat_backup.primary_world_file.as_deref(),
            Some("CompatWorld.wld")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    fn build_test_tmod_with_entry(
        internal_name: &str,
        version: &str,
        entry_name: &str,
        declared_uncompressed_len: i32,
        declared_compressed_len: i32,
        entry_payload: &[u8],
    ) -> Vec<u8> {
        let mut tmod = Vec::new();
        tmod.extend_from_slice(b"TMOD");
        write_dotnet_str(&mut tmod, "2026.9.3.1");
        tmod.extend_from_slice(&[0xABu8; 20]);
        tmod.extend_from_slice(&[0u8; 256]);
        tmod.extend_from_slice(&128i32.to_le_bytes());
        write_dotnet_str(&mut tmod, internal_name);
        write_dotnet_str(&mut tmod, version);
        tmod.extend_from_slice(&1i32.to_le_bytes());
        write_dotnet_str(&mut tmod, entry_name);
        tmod.extend_from_slice(&declared_uncompressed_len.to_le_bytes());
        tmod.extend_from_slice(&declared_compressed_len.to_le_bytes());
        tmod.extend_from_slice(entry_payload);
        tmod
    }

    #[test]
    fn tmod_parser_handles_sort_ignore_before_side_and_enforces_size_guards() {
        let mut info = Vec::new();
        write_dotnet_str(&mut info, "sortIgnore");
        write_dotnet_str(&mut info, "OtherMod");
        write_dotnet_str(&mut info, "");
        write_dotnet_str(&mut info, "side");
        info.push(1);
        write_dotnet_str(&mut info, "description");
        write_dotnet_str(&mut info, "Parsed after sortIgnore");
        write_dotnet_str(&mut info, "");

        let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&info).unwrap();
        let compressed_info = enc.finish().unwrap();

        let tmod_bytes = build_test_tmod_with_entry(
            "SortIgnoreMod",
            "1.0.0",
            "Info",
            info.len() as i32,
            compressed_info.len() as i32,
            &compressed_info,
        );
        let root = make_temp_test_dir("tl_core_tmod_guards");
        let valid_path = root.join("SortIgnoreMod.tmod");
        std::fs::write(&valid_path, &tmod_bytes).unwrap();

        let parsed = parse_tmod_file(&valid_path).unwrap();
        assert_eq!(parsed.side, ModSide::Client);
        assert_eq!(parsed.description, "Parsed after sortIgnore");

        let oversized_len: i32 = 16 * 1024 * 1024 + 1;
        let bad_header_bytes =
            build_test_tmod_with_entry("BadHdr", "1.0.0", "Info", oversized_len, 4, b"ABCD");
        let bad_header_path = root.join("BadHeader.tmod");
        std::fs::write(&bad_header_path, &bad_header_bytes).unwrap();
        assert!(matches!(
            parse_tmod_file(&bad_header_path),
            Err(LauncherError::InvalidFormat(_))
        ));

        let bomb_raw = vec![0u8; 16 * 1024 * 1024 + 64];
        let mut bomb_enc = DeflateEncoder::new(Vec::new(), Compression::best());
        bomb_enc.write_all(&bomb_raw).unwrap();
        let bomb_compressed = bomb_enc.finish().unwrap();
        let bomb_bytes = build_test_tmod_with_entry(
            "BombMod",
            "1.0.0",
            "Info",
            64,
            bomb_compressed.len() as i32,
            &bomb_compressed,
        );
        let bomb_path = root.join("BombMod.tmod");
        std::fs::write(&bomb_path, &bomb_bytes).unwrap();
        assert!(matches!(
            parse_tmod_file(&bomb_path),
            Err(LauncherError::InvalidFormat(_))
        ));

        let bad_varint = [0x80u8, 0x80, 0x80, 0x80, 0x80];
        assert!(matches!(
            binary_parsers::read_dotnet_string(&mut std::io::Cursor::new(&bad_varint)),
            Err(LauncherError::InvalidFormat(_))
        ));

        let mut neg_wld = Vec::new();
        neg_wld.extend_from_slice(&280i32.to_le_bytes());
        neg_wld.extend_from_slice(b"relogic");
        neg_wld.push(2);
        neg_wld.extend_from_slice(&1u32.to_le_bytes());
        neg_wld.extend_from_slice(&0u64.to_le_bytes());
        neg_wld.extend_from_slice(&1i16.to_le_bytes());
        neg_wld.extend_from_slice(&(-5i32).to_le_bytes());
        let neg_wld_path = root.join("NegOffset.wld");
        std::fs::write(&neg_wld_path, &neg_wld).unwrap();
        assert!(matches!(
            parse_world_file(&neg_wld_path),
            Err(LauncherError::InvalidFormat(_))
        ));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sync_active_modpack_from_mods_dir_updates_same_size_sha1_and_prunes_disabled() {
        let root = make_temp_test_dir("tl_core_sync_sha1_prune");
        let modpacks_dir = root.join("ModPacks");
        let mods_dir = root.join("Mods");
        std::fs::create_dir_all(&mods_dir).unwrap();

        let pack_mods = modpacks_dir.join("ActivePack").join("Mods");
        std::fs::create_dir_all(&pack_mods).unwrap();

        write_minimal_tmod(&pack_mods.join("SameLenMod.tmod"), "SameLenMod", "1.0.0");
        write_minimal_tmod(&mods_dir.join("SameLenMod.tmod"), "SameLenMod", "1.0.1");
        assert_eq!(
            std::fs::metadata(pack_mods.join("SameLenMod.tmod"))
                .unwrap()
                .len(),
            std::fs::metadata(mods_dir.join("SameLenMod.tmod"))
                .unwrap()
                .len()
        );
        assert_ne!(
            sha1_file_hex(&pack_mods.join("SameLenMod.tmod")).unwrap(),
            sha1_file_hex(&mods_dir.join("SameLenMod.tmod")).unwrap()
        );

        write_minimal_tmod(
            &pack_mods.join("RenamedDisabled.tmod"),
            "DisabledInternal",
            "1.0.0",
        );
        std::fs::write(pack_mods.join("FallbackDisabled.tmod"), b"BAD_TMOD").unwrap();
        std::fs::write(mods_dir.join("DisabledInternal.tmod"), b"NOT_USED").unwrap();

        save_enabled_mods(&mods_dir.join("enabled.json"), &["SameLenMod".to_string()]).unwrap();

        sync_active_modpack_from_mods_dir(&modpacks_dir, &mods_dir, "ActivePack", None).unwrap();

        assert_eq!(
            sha1_file_hex(&pack_mods.join("SameLenMod.tmod")).unwrap(),
            sha1_file_hex(&mods_dir.join("SameLenMod.tmod")).unwrap()
        );
        assert!(!pack_mods.join("RenamedDisabled.tmod").exists());
        assert!(!pack_mods.join("FallbackDisabled.tmod").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inspect_world_backup_zip_classifies_v279_relogic_and_rejects_bad_magic() {
        use zip::write::SimpleFileOptions;
        use zip::{CompressionMethod, ZipWriter};

        let root = make_temp_test_dir("tl_core_inspect_v279");
        let write_single_entry_zip = |zip_name: &str, entry_name: &str, content: &[u8]| -> PathBuf {
            let zip_path = root.join(zip_name);
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut writer = ZipWriter::new(file);
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            writer.start_file(entry_name, opts).unwrap();
            writer.write_all(content).unwrap();
            writer.finish().unwrap();
            zip_path
        };

        let mut v279_relogic = Vec::new();
        v279_relogic.extend_from_slice(&279i32.to_le_bytes());
        v279_relogic.extend_from_slice(b"relogic");
        v279_relogic.push(2);
        let vanilla_zip =
            write_single_entry_zip("vanilla_279.zip", "LegacyVanilla.wld", &v279_relogic);
        assert_eq!(
            inspect_world_backup_zip(&vanilla_zip),
            (
                Some(GameEdition::Vanilla),
                Some("LegacyVanilla.wld".to_string()),
                false
            )
        );

        let mut v279_bad_magic = Vec::new();
        v279_bad_magic.extend_from_slice(&279i32.to_le_bytes());
        v279_bad_magic.extend_from_slice(b"notrelo");
        v279_bad_magic.push(2);
        let unknown_zip = write_single_entry_zip("unknown_279.zip", "Unknown.wld", &v279_bad_magic);
        assert_eq!(
            inspect_world_backup_zip(&unknown_zip),
            (None, Some("Unknown.wld".to_string()), false)
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_world_backup_rolls_back_atomically_on_corrupt_zip_and_cleans_stale_baks() {
        use zip::write::SimpleFileOptions;
        use zip::{CompressionMethod, ZipWriter};

        let root = make_temp_test_dir("tl_core_restore_atomic");
        let src_dir = root.join("Source");
        let backups_dir = root.join("Backups");
        let target_dir = root.join("Target");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();

        let src_wld = src_dir.join("MyWorld.wld");
        std::fs::write(
            &src_wld,
            build_test_wld_bytes(279, "MyWorld", &[0, 0, 0, 0, 0, 0, 0, 0]),
        )
        .unwrap();
        std::fs::write(src_dir.join("MyWorld.twld"), b"SRC_TWLD").unwrap();
        std::fs::write(src_dir.join("MyWorld.twld.bak2"), b"SRC_TWLD_BAK2").unwrap();
        std::fs::write(src_dir.join("MyWorld.twld.bak9"), b"SRC_TWLD_BAK9").unwrap();

        let full_backup = backup_world(&src_wld, &backups_dir).unwrap();
        assert!(!backups_dir
            .join(format!(".{}.tl_atomic", full_backup.file_name))
            .exists());

        std::fs::write(target_dir.join("MyWorld.wld"), b"LIVE_WLD").unwrap();
        std::fs::write(target_dir.join("MyWorld.wld.bak"), b"STALE_WLD_BAK").unwrap();
        std::fs::write(target_dir.join("MyWorld.wld.bak2"), b"STALE_WLD_BAK2").unwrap();
        std::fs::write(target_dir.join("MyWorld.twld"), b"LIVE_TWLD").unwrap();
        std::fs::write(target_dir.join("MyWorld.twld.bak3"), b"STALE_TWLD_BAK3").unwrap();

        let mut zip_buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut zip_buf);
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            writer.start_file("MyWorld.wld", opts).unwrap();
            writer.write_all(b"NEW_SHOULD_NOT_LAND").unwrap();
            writer.start_file("MyWorld.wld.bak", opts).unwrap();
            writer.write_all(b"CORRUPT_ME_PAYLOAD_BYTES").unwrap();
            writer.finish().unwrap();
        }
        let mut raw_zip = zip_buf.into_inner();
        let needle = b"CORRUPT_ME_PAYLOAD_BYTES";
        let pos = raw_zip
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("expected stored payload bytes in test zip");
        raw_zip[pos] = b'X';
        let corrupt_zip_path = root.join("corrupt.zip");
        std::fs::write(&corrupt_zip_path, &raw_zip).unwrap();

        assert!(restore_world_backup(&corrupt_zip_path, &target_dir).is_err());
        assert_eq!(
            std::fs::read(target_dir.join("MyWorld.wld")).unwrap(),
            b"LIVE_WLD"
        );
        assert_eq!(
            std::fs::read(target_dir.join("MyWorld.wld.bak")).unwrap(),
            b"STALE_WLD_BAK"
        );
        assert_eq!(
            std::fs::read(target_dir.join("MyWorld.twld")).unwrap(),
            b"LIVE_TWLD"
        );
        let has_tmp = std::fs::read_dir(&target_dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".tl_restore_tmp"));
        assert!(!has_tmp);

        let restored = restore_world_backup(&full_backup.file_path, &target_dir).unwrap();
        assert!(restored.contains(&"MyWorld.twld.bak2".to_string()));
        assert!(restored.contains(&"MyWorld.twld.bak9".to_string()));
        assert_eq!(
            std::fs::read(target_dir.join("MyWorld.twld.bak2")).unwrap(),
            b"SRC_TWLD_BAK2"
        );
        assert_eq!(
            std::fs::read(target_dir.join("MyWorld.twld.bak9")).unwrap(),
            b"SRC_TWLD_BAK9"
        );
        assert!(!target_dir.join("MyWorld.wld.bak").exists());
        assert!(!target_dir.join("MyWorld.wld.bak2").exists());
        assert!(!target_dir.join("MyWorld.twld.bak3").exists());

        let _ = std::fs::remove_dir_all(&root);
    }
}
