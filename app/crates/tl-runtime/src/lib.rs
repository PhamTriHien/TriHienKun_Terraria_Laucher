pub mod game_installer;
pub mod game_runner;
pub mod http_client;
pub mod mods_manager;
pub mod network_host;
pub mod server_manager;

pub use game_installer::{
    download_and_install_xna, fetch_terraria_branch_manifest, fetch_tmodloader_releases,
    install_dotnet8_portable, install_terraria_from_local_zip, install_terraria_from_source,
    install_tmodloader_release, load_installed_terraria_receipt, parse_dotnet_runtime_version,
    parse_github_branch_spec, parse_github_release_spec, InstallProgress, InstallTarget,
    InstalledTerrariaReceipt, ProgressCallback, TerrariaBranchManifest, TerrariaManifestPart,
    TmodLoaderReleaseInfo, TERRARIA_MANIFEST_FILENAME, TMODLOADER_GITHUB_REPO,
    XNA_40_OFFICIAL_MSI_URL,
};

pub use game_runner::{
    install_xna_framework_elevated, launch_game, ActiveGameProcess, GameLaunchOptions,
    RunningGameSnapshot,
};
pub use mods_manager::{
    browse_workshop_mods, delete_installed_mod, delete_staged_workshop_mod,
    download_and_install_tmod_url, download_workshop_mod_via_steamcmd, fetch_github_latest_tmod,
    fetch_workshop_details, fetch_workshop_image_bytes, format_unix_timestamp,
    import_local_mod_files, install_steamcmd_via_winget, load_workshop_manifest,
    parse_workshop_browse_html, parse_workshop_id, pick_best_workshop_tmod,
    record_workshop_install, scan_installed_mods, scan_staged_workshop_mods, set_mod_enabled,
    strip_steam_bbcode, workshop_manifest_path, CuratedModSource, InstalledModEntry,
    RemoteTmodAsset, StagedWorkshopMod, WorkshopItemDetails, WorkshopManifestRecord,
    WorkshopQueryResult, WorkshopSort, CURATED_GITHUB_MODS,
};
pub use network_host::{
    check_firewall_rule_exists, close_upnp_port, ensure_firewall_rule, open_upnp_port,
    probe_network_snapshot, probe_tcp_endpoint, remove_firewall_rule, start_bore_tunnel,
    BoreTunnelHandle, LocalInterfaceInfo, NetworkSnapshot, TunnelStatus, UpnpLeaseInfo,
};
pub use server_manager::{
    egui_repaint::RepaintHandle, load_banlist, save_banlist, start_server, BanEntry,
    ConnectedPlayer, ConsoleLogLine, ServerLifecycleState, ServerRuntimeSnapshot, ServerSupervisor,
};
