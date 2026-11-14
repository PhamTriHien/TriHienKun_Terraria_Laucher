pub mod binary_parsers;
pub mod config;
pub mod error;
pub mod paths;

pub use binary_parsers::{
    activate_modpack, backup_world, create_modpack, delete_modpack, list_modpacks,
    list_world_backups, load_enabled_mods, parse_tmod_file, parse_world_file, restore_world_backup,
    save_enabled_mods, sha1_file_hex, ModPackInfo, ModSide, TmodMetadata, WorldBackupInfo,
    WorldMetadata,
};
pub use config::{
    GameEdition, LauncherConfig, SavedJoinServer, ServerConfigData, ServerProfile,
    WorldDifficultyOption, WorldSizeOption, DEFAULT_TERRARIA_INSTALL_SOURCE,
};
pub use error::{LauncherError, LauncherResult};
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
}
