pub mod game_installer;
pub mod game_runner;
pub mod http_client;
pub mod modpack_sync;
pub mod mods_manager;
pub mod network_host;
pub mod server_manager;

pub use game_installer::{
    download_and_install_xna, fetch_terraria_branch_manifest, fetch_tmodloader_releases,
    install_dotnet8_portable, install_steamcmd_with_progress, install_terraria_from_local_zip,
    install_terraria_from_source, install_tmodloader_release, load_installed_terraria_receipt,
    parse_dotnet_runtime_version, parse_github_branch_spec, parse_github_release_spec,
    read_required_tml_dotnet_version, sanitize_winget_stream_line, InstallProgress, InstallTarget,
    InstalledTerrariaReceipt, ProgressCallback, TerrariaBranchManifest, TerrariaManifestPart,
    TmodLoaderReleaseInfo, STEAMCMD_OFFICIAL_ZIP_URL, TERRARIA_MANIFEST_FILENAME,
    TMODLOADER_GITHUB_REPO, XNA_40_OFFICIAL_MSI_URL,
};

pub use game_runner::{
    apply_dotnet_std_env, apply_dotnet_tokio_env, detect_existing_game_process,
    install_xna_framework_elevated, launch_game, resolve_dotnet_launch,
    terminate_game_process_by_pid, ActiveGameProcess, DotnetLaunchSpec, GameLaunchOptions,
    RunningGameSnapshot, RuntimeTuning,
};
pub use modpack_sync::{
    build_modpack_share_preview, inspect_share_key_against_local, load_mod_sources,
    mod_sources_path, parse_mod_source_input, remove_mod_source, resolve_mod_source_for_meta,
    search_workshop_candidates_for_mod, sync_modpack_from_share_key, upsert_mod_source,
    ModPackExportEntry, ModPackImportPreview, ModPackSharePreview, ModPackSyncProgress,
    ModPackSyncReport, ModSourceRecord,
};
pub use mods_manager::{
    browse_workshop_mods, delete_installed_mod, delete_staged_workshop_mod,
    detect_installed_tml_year_month, download_and_install_tmod_url,
    download_and_install_tmod_url_with_source, download_workshop_mod_via_steamcmd,
    enable_mod_and_installed_deps, fetch_github_latest_tmod, fetch_workshop_details,
    fetch_workshop_image_bytes, format_unix_timestamp, import_local_mod_files,
    install_steamcmd_via_winget, load_workshop_manifest, parse_workshop_browse_html,
    parse_workshop_id, pick_best_workshop_tmod, pick_best_workshop_tmods,
    record_workshop_install, remove_workshop_manifest_entry, scan_installed_mods,
    scan_staged_workshop_mods, set_mod_enabled, strip_steam_bbcode, workshop_manifest_path,
    CuratedModSource, InstalledModEntry, RemoteTmodAsset, StagedWorkshopMod, WorkshopItemDetails,
    WorkshopManifestRecord, WorkshopQueryResult, WorkshopSort, CURATED_GITHUB_MODS,
};
pub use network_host::{
    check_firewall_rule_exists, close_upnp_port, detect_primary_global_ipv6, ensure_firewall_rule,
    is_global_unicast_ipv6, is_local_probe_host, open_upnp_port, probe_local_endpoint_listening,
    probe_local_tcp_listening, probe_network_snapshot, probe_tcp_endpoint, remove_firewall_rule,
    resolve_ipv6_bridge_target, split_endpoint_host_port, start_direct_port_host,
    start_ipv4_to_ipv6_client_bridge, DirectHostActiveInfo, DirectHostConfig, DirectHostStatus,
    DirectPortHostHandle, LocalClientBridgeHandle, LocalInterfaceInfo, NetworkSnapshot,
    UpnpLeaseInfo, UpnpPortStatus,
};
pub use server_manager::{
    acquire_single_instance_guard, detect_existing_server_processes,
    egui_repaint::RepaintHandle, extract_ban_ip_key, focus_existing_launcher_window,
    is_loopback_ban_target, load_banlist, normalize_ip_addr, read_server_log_summary,
    resolve_tmodloader_server_modpack_config, save_banlist, start_server,
    summarize_server_log_lines, terminate_process_by_pid, BanEntry, ConnectedPlayer,
    ConsoleLogLine, DetectedExternalServer, ExternalServerLogSummary, ServerLifecycleState,
    ServerRuntimeSnapshot, ServerSupervisor, SingleInstanceGuard,
};
