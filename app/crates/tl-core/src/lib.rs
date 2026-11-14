pub mod binary_parsers;
pub mod config;
pub mod error;
pub mod modpack_share;
pub mod paths;

pub use binary_parsers::{
    activate_modpack, backup_unsaved_enabled_mods_as_modpack, backup_world, create_modpack,
    delete_modpack, list_modpacks, list_world_backups, load_enabled_mods, parse_tmod_file,
    parse_world_file, restore_world_backup, save_enabled_mods, sha1_file_hex,
    write_modpack_install_txt, ModPackInfo, ModSide, TmodMetadata, WorldBackupInfo, WorldMetadata,
};
pub use config::{
    GameEdition, LauncherConfig, SavedJoinServer, ServerConfigData, ServerProfile,
    WorldDifficultyOption, WorldSizeOption, DEFAULT_TERRARIA_INSTALL_SOURCE,
};
pub use error::{LauncherError, LauncherResult};
pub use modpack_share::{
    decode_modpack_share_key, encode_modpack_share_key, extract_source_hint_from_homepage,
    resolve_unique_modpack_name, sanitize_modpack_name, validate_share_payload,
    ModPackShareKeyPayload, SharedModEntry, SharedModSource, SHARE_KEY_PREFIX,
};
pub use paths::{
    detect_environment, is_xna_framework_installed, EnvironmentStatus, LauncherPaths,
};

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::DeflateEncoder;
    use flate2::Compression;
    use std::io::Write;
    use std::path::PathBuf;

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
                },
                SharedModEntry {
                    internal_name: "RecipeBrowser".to_string(),
                    display_name: String::new(),
                    version: String::new(),
                    source: SharedModSource::Github {
                        repo: "JavidPack/RecipeBrowser".to_string(),
                        url: String::new(),
                    },
                },
                SharedModEntry {
                    internal_name: "MagicStorage".to_string(),
                    display_name: "Magic Storage".to_string(),
                    version: "0.7.0".to_string(),
                    source: SharedModSource::Github {
                        repo: "blushiemagic/MagicStorage".to_string(),
                        url: "https://github.com/blushiemagic/MagicStorage/releases/download/v0.7.0/MagicStorage.tmod".to_string(),
                    },
                },
                SharedModEntry {
                    internal_name: "CustomUrlMod".to_string(),
                    display_name: String::new(),
                    version: String::new(),
                    source: SharedModSource::Url {
                        url: "https://cdn.example.com/mods/CustomUrlMod.tmod?dl=1".to_string(),
                    },
                },
                SharedModEntry {
                    internal_name: "LocalOnlyMod".to_string(),
                    display_name: "Local Mod".to_string(),
                    version: "1.0".to_string(),
                    source: SharedModSource::ManualOnly,
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

        // Calling again with same enabled set (even different order) -> None
        save_enabled_mods(
            &mods_dir.join("enabled.json"),
            &["ModB".to_string(), "ModA".to_string()],
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

        // Active modpack exists on disk -> None even if set changed
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

        // Empty active_modpack and new set with name collision -> creates AutoBackup_2
        let second = backup_unsaved_enabled_mods_as_modpack(
            &modpacks_dir,
            &mods_dir,
            "",
            &metas,
            "AutoBackup",
        )
        .unwrap()
        .expect("expected second unique backup pack");
        assert_eq!(second.name, "AutoBackup_2");
        assert_eq!(second.local_tmod_count, 1);

        let _ = std::fs::remove_dir_all(&root);
    }
}
