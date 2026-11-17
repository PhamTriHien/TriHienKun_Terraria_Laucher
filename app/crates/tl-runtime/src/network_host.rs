use crate::game_runner::run_elevated_windows;
use crate::http_client::{
    build_http_client, build_http_client_with_options, ensure_rustls_provider, LAUNCHER_USER_AGENT,
};
use crate::p2p_tunnel::{
    apply_p2p_peer_ban, load_or_create_p2p_secret_key, normalize_p2p_ban_set,
    spawn_p2p_host_worker, ActiveP2pConns, BridgedPeerTarget, P2pHostStatus,
};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, SockRef, Socket, TcpKeepalive, Type};
use std::collections::{HashMap, HashSet};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, UdpSocket as StdUdpSocket,
};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};
use sysinfo::Networks;
use tl_core::{LauncherError, LauncherResult};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
#[cfg(windows)]
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
    MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SSDP_MULTICAST: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
const SSDP_SEARCH_TARGETS: &[&str] = &[
    "urn:schemas-upnp-org:device:InternetGatewayDevice:1",
    "urn:schemas-upnp-org:device:InternetGatewayDevice:2",
    "urn:schemas-upnp-org:service:WANIPConnection:1",
    "urn:schemas-upnp-org:service:WANIPConnection:2",
    "urn:schemas-upnp-org:service:WANPPPConnection:1",
];
const UPNP_IPV6_FIREWALL_SERVICE: &str = "urn:schemas-upnp-org:service:WANIPv6FirewallControl:1";
const IPV6_SSDP_SEARCH_TARGETS: &[&str] = &[
    UPNP_IPV6_FIREWALL_SERVICE,
    "urn:schemas-upnp-org:device:InternetGatewayDevice:2",
];
const IFCONFIG_PORT_PROBE_BASE_URL: &str = "https://ifconfig.co";
const PORTCHECKER_PROBE_BASE_URL: &str = "https://portchecker.io";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalInterfaceInfo {
    pub name: String,
    pub ipv4: Ipv4Addr,
    pub is_primary: bool,
    #[serde(default)]
    pub is_virtual: bool,
    #[serde(default)]
    pub is_vpn: bool,
    #[serde(default)]
    pub is_hypervisor: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSnapshot {
    pub primary_lan_ip: Option<Ipv4Addr>,
    pub interfaces: Vec<LocalInterfaceInfo>,
    #[serde(default)]
    pub global_ipv6: Option<Ipv6Addr>,
    pub public_ip: Option<Ipv4Addr>,
    pub public_ip_error: Option<String>,
    pub firewall_rule_active: Option<bool>,
    #[serde(default)]
    pub has_vpn_or_virtual: bool,
    #[serde(default)]
    pub local_port_listening: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpnpLeaseInfo {
    pub control_url: String,
    pub service_type: String,
    pub external_ip: Option<Ipv4Addr>,
    pub local_ip: Ipv4Addr,
    pub port: u16,
    pub is_cgnat: bool,
    pub lease_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExternalReachability {
    #[default]
    NotChecked,
    Checking,
    Reachable,
    Blocked,
    CheckUnavailable(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ipv6PinholeLeaseInfo {
    pub control_url: String,
    pub service_type: String,
    pub internal_client: Ipv6Addr,
    pub internal_port: u16,
    pub unique_id: u32,
    pub lease_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Ipv6PinholeStatus {
    #[default]
    Disabled,
    Opening,
    FirewallDisabled,
    Opened {
        unique_id: u32,
        gateway_label: String,
    },
    Rejected(String),
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpnpPortStatus {
    Disabled,
    Mapping,
    Mapped {
        external_ip: Ipv4Addr,
        external_port: u16,
        gateway_label: String,
    },
    CarrierGradeNat {
        wan_ip: Ipv4Addr,
        #[serde(default)]
        public_ip: Option<Ipv4Addr>,
        external_port: u16,
    },
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectHostActiveInfo {
    pub local_port: u16,
    pub host_port: u16,
    pub lan_endpoints: Vec<(String, String)>,
    #[serde(default)]
    pub vpn_endpoints: Vec<(String, String)>,
    pub ipv6_endpoint: Option<String>,
    #[serde(default)]
    pub ipv6_pinhole_status: Ipv6PinholeStatus,
    #[serde(default)]
    pub ipv6_reachability: ExternalReachability,
    pub upnp_status: UpnpPortStatus,
    #[serde(default)]
    pub ipv4_reachability: ExternalReachability,
    pub bridging_tcp: bool,
    pub active_clients: usize,
    #[serde(default)]
    pub p2p_status: P2pHostStatus,
}

impl DirectHostActiveInfo {
    pub fn primary_lan_endpoint(&self) -> Option<&str> {
        self.lan_endpoints.first().map(|(_, ep)| ep.as_str())
    }

    pub fn preferred_online_endpoint(&self) -> Option<String> {
        if let P2pHostStatus::Running { ticket, .. } = &self.p2p_status {
            return Some(ticket.clone());
        }
        if let UpnpPortStatus::Mapped {
            external_ip,
            external_port,
            ..
        } = &self.upnp_status
        {
            if matches!(
                self.ipv4_reachability,
                ExternalReachability::Reachable | ExternalReachability::NotChecked
            ) {
                return Some(format!("{external_ip}:{external_port}"));
            }
        }
        if let Some(v6) = &self.ipv6_endpoint {
            if self.ipv6_reachability == ExternalReachability::Reachable {
                return Some(v6.clone());
            }
        }
        self.vpn_endpoints.first().map(|(_, ep)| ep.clone())
    }

    pub fn is_online_setup_in_progress(&self) -> bool {
        matches!(self.p2p_status, P2pHostStatus::Starting)
            || matches!(self.upnp_status, UpnpPortStatus::Mapping)
            || matches!(self.ipv6_pinhole_status, Ipv6PinholeStatus::Opening)
            || matches!(self.ipv4_reachability, ExternalReachability::Checking)
            || matches!(self.ipv6_reachability, ExternalReachability::Checking)
    }

    pub fn candidate_wan_endpoint(&self) -> Option<(String, &ExternalReachability)> {
        if let UpnpPortStatus::Mapped {
            external_ip,
            external_port,
            ..
        } = &self.upnp_status
        {
            return Some((
                format!("{external_ip}:{external_port}"),
                &self.ipv4_reachability,
            ));
        }
        self.ipv6_endpoint
            .as_ref()
            .map(|v6| (v6.clone(), &self.ipv6_reachability))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum DirectHostStatus {
    Idle,
    Starting { local_port: u16, host_port: u16 },
    Running(DirectHostActiveInfo),
    Stopped,
    Error(String),
}

pub(crate) type BridgePeerMap = HashMap<u16, (BridgedPeerTarget, CancellationToken)>;

fn is_bannable_peer_ip(ip: IpAddr) -> bool {
    !ip.is_loopback() && !ip.is_unspecified()
}

fn normalize_ban_set(ips: impl IntoIterator<Item = IpAddr>) -> HashSet<IpAddr> {
    ips.into_iter()
        .map(|ip| ip.to_canonical())
        .filter(|ip| is_bannable_peer_ip(*ip))
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectHostConfig {
    pub local_target_port: u16,
    pub host_port: u16,
    pub bind_ipv4: Option<Ipv4Addr>,
    pub try_upnp: bool,
    pub cleanup_local_upnp_on_start: bool,
    pub initial_banned_ips: Vec<IpAddr>,
    #[serde(default)]
    pub verify_wan_reachability: bool,
    #[serde(default)]
    pub enable_p2p: bool,
    #[serde(default)]
    pub p2p_secret_key_path: Option<PathBuf>,
    #[serde(default)]
    pub initial_banned_p2p_peers: Vec<String>,
}

#[derive(Clone)]
pub struct DirectPortHostHandle {
    cancel: CancellationToken,
    status: Arc<RwLock<DirectHostStatus>>,
    banned_ips: Arc<RwLock<HashSet<IpAddr>>>,
    banned_p2p_peers: Arc<RwLock<HashSet<String>>>,
    active_p2p_conns: Arc<ActiveP2pConns>,
    bridge_peers: Arc<RwLock<BridgePeerMap>>,
    has_upnp_lease: Arc<AtomicBool>,
    has_p2p_endpoint: Arc<AtomicBool>,
    cleanup_done: Arc<(Mutex<bool>, Condvar)>,
    rechecking: Arc<AtomicBool>,
}

impl DirectPortHostHandle {
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_for_test(
        cancel: CancellationToken,
        status: Arc<RwLock<DirectHostStatus>>,
        banned_ips: Arc<RwLock<HashSet<IpAddr>>>,
        banned_p2p_peers: Arc<RwLock<HashSet<String>>>,
        active_p2p_conns: Arc<ActiveP2pConns>,
        bridge_peers: Arc<RwLock<BridgePeerMap>>,
        has_upnp_lease: Arc<AtomicBool>,
        has_p2p_endpoint: Arc<AtomicBool>,
        cleanup_done: Arc<(Mutex<bool>, Condvar)>,
        rechecking: Arc<AtomicBool>,
    ) -> Self {
        Self {
            cancel,
            status,
            banned_ips,
            banned_p2p_peers,
            active_p2p_conns,
            bridge_peers,
            has_upnp_lease,
            has_p2p_endpoint,
            cleanup_done,
            rechecking,
        }
    }

    pub fn stop(&self) {
        self.cancel.cancel();
    }

    pub fn stop_and_wait(&self, timeout: Duration) {
        self.stop();
        if !self.has_upnp_lease.load(Ordering::SeqCst)
            && !self.has_p2p_endpoint.load(Ordering::SeqCst)
        {
            return;
        }
        let (lock, cvar) = &*self.cleanup_done;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = cvar.wait_timeout_while(guard, timeout, |done| !*done);
    }

    pub fn current_status(&self) -> DirectHostStatus {
        self.status
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn recheck_reachability(
        &self,
        rt: &tokio::runtime::Handle,
        wake_ui: crate::server_manager::egui_repaint::RepaintHandle,
    ) {
        self.recheck_reachability_with_bases(
            rt,
            wake_ui,
            IFCONFIG_PORT_PROBE_BASE_URL,
            PORTCHECKER_PROBE_BASE_URL,
        );
    }

    fn recheck_reachability_with_bases(
        &self,
        rt: &tokio::runtime::Handle,
        wake_ui: crate::server_manager::egui_repaint::RepaintHandle,
        ifconfig_base_url: &str,
        portchecker_base_url: &str,
    ) {
        if self.cancel.is_cancelled() {
            return;
        }

        let (v4_target, v6_target) = {
            let guard = self.status.read().unwrap_or_else(|e| e.into_inner());
            let DirectHostStatus::Running(ref info) = *guard else {
                return;
            };
            if info.is_online_setup_in_progress() {
                return;
            }
            let v4 = match &info.upnp_status {
                UpnpPortStatus::Mapped {
                    external_ip,
                    external_port,
                    ..
                } => Some((*external_ip, *external_port)),
                _ => None,
            };
            let v6 = info.ipv6_endpoint.as_deref().and_then(|ep| {
                let (host, port) = split_endpoint_host_port(ep)?;
                host.parse::<Ipv6Addr>().ok().map(|ip| (ip, port))
            });
            (v4, v6)
        };

        if v4_target.is_none() && v6_target.is_none() {
            return;
        }

        if self
            .rechecking
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        struct RecheckResetGuard(Arc<AtomicBool>);
        impl Drop for RecheckResetGuard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let reset_guard = RecheckResetGuard(Arc::clone(&self.rechecking));

        {
            let mut guard = self.status.write().unwrap_or_else(|e| e.into_inner());
            let DirectHostStatus::Running(ref mut info) = *guard else {
                return;
            };
            if v4_target.is_some() {
                info.ipv4_reachability = ExternalReachability::Checking;
            }
            if v6_target.is_some() {
                info.ipv6_reachability = ExternalReachability::Checking;
            }
        }
        wake_ui.wake();

        let cancel = self.cancel.clone();
        let status = Arc::clone(&self.status);
        let ifconfig_base = ifconfig_base_url.to_string();
        let portchecker_base = portchecker_base_url.to_string();
        rt.spawn(async move {
            let _reset_guard = reset_guard;
            let probe_res = tokio::select! {
                _ = cancel.cancelled() => return,
                pair = async {
                    let v4_fut = async {
                        match v4_target {
                            Some((ip, port)) => Some(
                                probe_external_ipv4_reachability_at(
                                    &ifconfig_base,
                                    &portchecker_base,
                                    ip,
                                    port,
                                )
                                .await,
                            ),
                            None => None,
                        }
                    };
                    let v6_fut = async {
                        match v6_target {
                            Some((ip, port)) => Some(
                                probe_external_ipv6_reachability_at(&ifconfig_base, ip, port)
                                    .await,
                            ),
                            None => None,
                        }
                    };
                    tokio::join!(v4_fut, v6_fut)
                } => pair,
            };

            if cancel.is_cancelled() {
                return;
            }

            let (v4_reach, v6_reach) = probe_res;
            let mut updated = false;
            {
                let mut guard = status.write().unwrap_or_else(|e| e.into_inner());
                if let DirectHostStatus::Running(ref mut info) = *guard {
                    if let Some(res) = v4_reach {
                        info.ipv4_reachability = res;
                        updated = true;
                    }
                    if let Some(res) = v6_reach {
                        info.ipv6_reachability = res;
                        updated = true;
                    }
                }
            }
            if updated {
                wake_ui.wake();
            }
        });
    }

    pub fn set_banned_ips(&self, ips: impl IntoIterator<Item = IpAddr>) {
        let ban_set = normalize_ban_set(ips);
        *self.banned_ips.write().unwrap_or_else(|e| e.into_inner()) = ban_set.clone();
        let tokens_to_cancel: Vec<CancellationToken> = {
            let peers = self.bridge_peers.read().unwrap_or_else(|e| e.into_inner());
            peers
                .values()
                .filter_map(|(target, tok)| match target {
                    BridgedPeerTarget::Ip(peer_ip) if ban_set.contains(peer_ip) => {
                        Some(tok.clone())
                    }
                    _ => None,
                })
                .collect()
        };
        for tok in tokens_to_cancel {
            tok.cancel();
        }
    }

    pub fn set_banned_p2p_peers(&self, peers: impl IntoIterator<Item = String>) {
        apply_p2p_peer_ban(
            &self.banned_p2p_peers,
            &self.bridge_peers,
            &self.active_p2p_conns,
            peers,
        );
    }

    pub fn resolve_bridged_peer(&self, loopback_client_port: u16) -> Option<IpAddr> {
        self.bridge_peers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&loopback_client_port)
            .and_then(|(target, _)| match target {
                BridgedPeerTarget::Ip(ip) => Some(*ip),
                BridgedPeerTarget::P2p(_) => None,
            })
    }

    pub fn resolve_bridged_peer_target(
        &self,
        loopback_client_port: u16,
    ) -> Option<BridgedPeerTarget> {
        self.bridge_peers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&loopback_client_port)
            .map(|(target, _)| target.clone())
    }
}

#[derive(Debug)]
pub struct LocalClientBridgeHandle {
    cancel: CancellationToken,
    local_port: u16,
}

pub(crate) fn new_local_client_bridge_handle(
    cancel: CancellationToken,
    local_port: u16,
) -> LocalClientBridgeHandle {
    LocalClientBridgeHandle { cancel, local_port }
}

impl LocalClientBridgeHandle {
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

impl Drop for LocalClientBridgeHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub fn is_global_unicast_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    let segs = ip.segments();
    if (segs[0] & 0xe000) != 0x2000 {
        return false;
    }
    if segs[0] == 0x2001 && (segs[1] == 0x0db8 || segs[1] == 0x0000) {
        return false;
    }
    true
}

pub fn detect_primary_global_ipv6() -> Option<Ipv6Addr> {
    for probe in ["[2606:4700:4700::1111]:80", "[2001:4860:4860::8888]:53"] {
        if let Ok(sock) = StdUdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)) {
            if sock.connect(probe).is_ok() {
                if let Ok(local_addr) = sock.local_addr() {
                    if let IpAddr::V6(v6) = local_addr.ip() {
                        if is_global_unicast_ipv6(v6) {
                            return Some(v6);
                        }
                    }
                }
            }
        }
    }

    if let Ok(networks) = Networks::new_with_refreshed_list() {
        for (_iface_name, data) in &networks {
            for ip_net in data.ip_networks() {
                if let IpAddr::V6(v6) = ip_net.addr {
                    if is_global_unicast_ipv6(v6) {
                        return Some(v6);
                    }
                }
            }
        }
    }

    None
}

pub fn split_endpoint_host_port(endpoint: &str) -> Option<(String, u16)> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty()
        || trimmed.contains("://")
        || trimmed.contains('/')
        || trimmed.chars().any(char::is_whitespace)
    {
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix('[') {
        let (host_raw, suffix) = rest.split_once(']')?;
        let host = host_raw.trim();
        if host.is_empty() {
            return None;
        }
        let valid_v6 = if let Some((addr_part, scope_part)) = host.split_once('%') {
            !scope_part.is_empty()
                && !scope_part.contains(['%', '[', ']', ':'])
                && addr_part.parse::<Ipv6Addr>().is_ok()
        } else {
            host.parse::<Ipv6Addr>().is_ok()
        };
        if !valid_v6 {
            return None;
        }
        let port_str = suffix.trim().strip_prefix(':')?;
        let port = port_str.parse::<u16>().ok().filter(|&p| p > 0)?;
        return Some((host.to_string(), port));
    }

    if trimmed.matches(':').count() != 1 {
        return None;
    }
    let (host_part, port_part) = trimmed.split_once(':')?;
    if host_part.is_empty() || host_part.contains(['[', ']']) {
        return None;
    }
    let port = port_part.parse::<u16>().ok().filter(|&p| p > 0)?;
    Some((host_part.to_string(), port))
}

pub fn is_cgnat_or_private_ipv4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || (o[0] == 100 && (o[1] & 0b1100_0000) == 64)
}

fn is_numbered_tun_tap(token: &str, prefix: &str) -> bool {
    token
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.bytes().all(|b| b.is_ascii_digit()))
}

pub(crate) fn is_vpn_adapter_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    const VPN_SUBSTRINGS: &[&str] = &[
        "warp",
        "cloudflare",
        "wireguard",
        "tailscale",
        "zerotier",
        "radmin",
        "hamachi",
        "openvpn",
        "wintun",
        "tap-",
        "utun",
    ];
    if VPN_SUBSTRINGS.iter().any(|pat| lower.contains(pat)) {
        return true;
    }
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|tok| !tok.is_empty())
        .any(|tok| is_numbered_tun_tap(tok, "tun") || is_numbered_tun_tap(tok, "tap"))
}

pub(crate) fn is_hypervisor_adapter_name(name: &str) -> bool {
    if is_vpn_adapter_name(name) {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    const HYPERVISOR_SUBSTRINGS: &[&str] = &[
        "vethernet",
        "vmware",
        "vmnet",
        "virtualbox",
        "vbox",
        "hyper-v",
        "wsl",
        "docker",
        "loopback",
        "pseudo",
    ];
    HYPERVISOR_SUBSTRINGS.iter().any(|pat| lower.contains(pat))
}

#[cfg(test)]
pub(crate) fn is_virtual_adapter_name(name: &str) -> bool {
    is_vpn_adapter_name(name) || is_hypervisor_adapter_name(name)
}

fn rfc1918_rank(ip: Ipv4Addr) -> Option<u8> {
    let o = ip.octets();
    if o[0] == 192 && o[1] == 168 {
        Some(0)
    } else if o[0] == 10 {
        Some(1)
    } else if o[0] == 172 && (16..=31).contains(&o[1]) {
        Some(2)
    } else {
        None
    }
}

fn detect_udp_route_ipv4() -> Option<Ipv4Addr> {
    let sock = StdUdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified() => {
            Some(v4)
        }
        _ => None,
    }
}

fn interface_sort_key(
    iface: &LocalInterfaceInfo,
    udp_route_ip: Option<Ipv4Addr>,
) -> (u8, u8, u8, u8, &str, u32) {
    let rfc_rank = rfc1918_rank(iface.ipv4);
    let tier = if !iface.is_virtual {
        if rfc_rank.is_some() && Some(iface.ipv4) == udp_route_ip {
            0
        } else if rfc_rank.is_some() {
            1
        } else {
            2
        }
    } else {
        3
    };
    let sub_rfc = rfc_rank.unwrap_or(9);
    let sub_vpn = u8::from(iface.is_virtual && iface.is_vpn);
    let sub_route = u8::from(Some(iface.ipv4) != udp_route_ip);
    (
        tier,
        sub_rfc,
        sub_vpn,
        sub_route,
        iface.name.as_str(),
        u32::from(iface.ipv4),
    )
}

pub(crate) fn rank_local_interfaces(
    raw: Vec<(String, Ipv4Addr)>,
    udp_route_ip: Option<Ipv4Addr>,
) -> (Option<Ipv4Addr>, Vec<LocalInterfaceInfo>) {
    let mut list: Vec<LocalInterfaceInfo> = raw
        .into_iter()
        .filter(|(_, ipv4)| !ipv4.is_loopback() && !ipv4.is_link_local() && !ipv4.is_unspecified())
        .map(|(name, ipv4)| {
            let is_vpn = is_vpn_adapter_name(&name);
            let is_hypervisor = is_hypervisor_adapter_name(&name);
            let is_virtual = is_vpn || is_hypervisor;
            LocalInterfaceInfo {
                name,
                ipv4,
                is_primary: false,
                is_virtual,
                is_vpn,
                is_hypervisor,
            }
        })
        .collect();

    list.sort_by(|a, b| {
        interface_sort_key(a, udp_route_ip).cmp(&interface_sort_key(b, udp_route_ip))
    });

    if let Some(first) = list.first_mut() {
        first.is_primary = true;
        let primary = first.ipv4;
        (Some(primary), list)
    } else {
        (udp_route_ip, list)
    }
}

pub fn detect_primary_lan_ipv4() -> Option<Ipv4Addr> {
    collect_local_interfaces().0
}

pub fn collect_local_interfaces() -> (Option<Ipv4Addr>, Vec<LocalInterfaceInfo>) {
    let udp_route_ip = detect_udp_route_ipv4();
    let Ok(networks) = Networks::new_with_refreshed_list() else {
        return (udp_route_ip, Vec::new());
    };
    let mut raw = Vec::new();
    for (iface_name, data) in &networks {
        let name = iface_name.to_string_lossy().into_owned();
        for ip_net in data.ip_networks() {
            if let IpAddr::V4(v4) = ip_net.addr {
                raw.push((name.clone(), v4));
            }
        }
    }
    rank_local_interfaces(raw, udp_route_ip)
}

pub async fn probe_local_tcp_listening(port: u16) -> bool {
    if port == 0 {
        return false;
    }
    is_local_tcp_socket_listening(
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)),
        Duration::from_millis(300),
    )
    .await
}

const PUBLIC_IPV4_ENDPOINTS: [&str; 3] = [
    "https://api.ipify.org",
    "https://ipv4.icanhazip.com",
    "https://checkip.amazonaws.com",
];

async fn query_public_ipv4_endpoints(
    client: &reqwest::Client,
    endpoints: &[&str],
    per_req_timeout: Duration,
    overall_timeout: Duration,
) -> Option<Ipv4Addr> {
    tokio::time::timeout(overall_timeout, async {
        for &endpoint in endpoints {
            if let Ok(resp) = client.get(endpoint).timeout(per_req_timeout).send().await {
                if resp.status().is_success() {
                    if let Ok(text) = resp.text().await {
                        if let Ok(parsed) = text.trim().parse::<Ipv4Addr>() {
                            return Some(parsed);
                        }
                    }
                }
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

async fn query_public_ipv4_with_client(
    client: &reqwest::Client,
    per_req_timeout: Duration,
) -> Option<Ipv4Addr> {
    query_public_ipv4_endpoints(
        client,
        &PUBLIC_IPV4_ENDPOINTS,
        per_req_timeout,
        Duration::from_secs(4),
    )
    .await
}

async fn probe_network_snapshot_with_client(
    server_port: u16,
    firewall_port: u16,
    client_result: LauncherResult<reqwest::Client>,
) -> NetworkSnapshot {
    let (primary_lan_ip, interfaces) = collect_local_interfaces();
    let global_ipv6 = detect_primary_global_ipv6();
    let has_vpn_or_virtual = interfaces.iter().any(|i| i.is_vpn);
    let firewall_rule_active = Some(check_firewall_rule_exists(firewall_port));
    let local_port_listening = probe_local_tcp_listening(server_port).await;

    let (public_ip, public_ip_error) = match client_result {
        Ok(client) => {
            let ip =
                query_public_ipv4_with_client(&client, Duration::from_millis(1500)).await;
            let err = ip
                .is_none()
                .then(|| "Không lấy được IP Public từ internet".to_string());
            (ip, err)
        }
        Err(e) => (None, Some(e.to_string())),
    };

    NetworkSnapshot {
        primary_lan_ip,
        interfaces,
        global_ipv6,
        public_ip,
        public_ip_error,
        firewall_rule_active,
        has_vpn_or_virtual,
        local_port_listening,
    }
}

pub async fn probe_network_snapshot_ports(server_port: u16, firewall_port: u16) -> NetworkSnapshot {
    probe_network_snapshot_with_client(
        server_port,
        firewall_port,
        build_http_client_with_options(None, true, true),
    )
    .await
}

pub async fn probe_network_snapshot(port: u16, proxy_url: Option<&str>) -> NetworkSnapshot {
    probe_network_snapshot_with_client(port, port, build_http_client(proxy_url)).await
}

pub fn firewall_rule_name(port: u16) -> String {
    format!("TerrariaLauncher_TCP_{port}")
}

fn run_netsh(args: &[&str]) -> LauncherResult<Output> {
    let mut cmd = Command::new("netsh");
    cmd.args(args);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.output()
        .map_err(|e| LauncherError::Process(format!("Không chạy được netsh: {e}")))
}

pub(crate) fn parse_netsh_rule_enabled(stdout: &str) -> bool {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.to_ascii_lowercase().contains("no rules match") {
        return false;
    }
    let mut saw_kv_line = false;
    for line in trimmed.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        saw_kv_line = true;
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim().to_ascii_lowercase();
        if matches!(key.as_str(), "enabled" | "bật")
            && matches!(val.as_str(), "no" | "false" | "disabled" | "không")
        {
            return false;
        }
        if matches!(key.as_str(), "action" | "hành động")
            && matches!(val.as_str(), "block" | "chặn")
        {
            return false;
        }
    }
    saw_kv_line
}

fn show_firewall_rule_stdout(port: u16) -> Option<String> {
    let name = firewall_rule_name(port);
    let name_arg = format!("name={name}");
    let out = run_netsh(&["advfirewall", "firewall", "show", "rule", &name_arg]).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn check_firewall_rule_exists(port: u16) -> bool {
    if port == 0 {
        return false;
    }
    show_firewall_rule_stdout(port)
        .as_deref()
        .is_some_and(parse_netsh_rule_enabled)
}

pub fn ensure_firewall_rule(port: u16) -> LauncherResult<String> {
    if port == 0 {
        return Err(LauncherError::InvalidInput(
            "Cổng server không hợp lệ (port = 0)".to_string(),
        ));
    }
    let name = firewall_rule_name(port);
    let name_arg = format!("name={name}");
    let port_arg = format!("localport={port}");

    if let Some(stdout) = show_firewall_rule_stdout(port) {
        if parse_netsh_rule_enabled(&stdout) {
            return Ok(format!("Quy tắc Firewall {name} đã tồn tại"));
        }
        let set_args = [
            "advfirewall",
            "firewall",
            "set",
            "rule",
            &name_arg,
            "new",
            "dir=in",
            "action=allow",
            "enable=yes",
            "profile=any",
            "protocol=TCP",
            &port_arg,
        ];
        let out = run_netsh(&set_args)?;
        if !out.status.success() {
            run_elevated_windows("netsh.exe", &set_args.join(" "))?;
        }
        if !check_firewall_rule_exists(port) {
            return Err(LauncherError::Process(format!(
                "Không xác nhận được quy tắc Firewall {name} sau khi cập nhật"
            )));
        }
        return Ok(format!("Đã bật lại quy tắc Firewall TCP cổng {port}"));
    }

    let add_args = [
        "advfirewall",
        "firewall",
        "add",
        "rule",
        &name_arg,
        "dir=in",
        "action=allow",
        "enable=yes",
        "profile=any",
        "protocol=TCP",
        &port_arg,
    ];
    let out = run_netsh(&add_args)?;
    if !out.status.success() {
        run_elevated_windows("netsh.exe", &add_args.join(" "))?;
    }
    if !check_firewall_rule_exists(port) {
        return Err(LauncherError::Process(format!(
            "Không xác nhận được quy tắc Firewall {name} sau khi tạo"
        )));
    }
    Ok(format!("Đã mở Firewall TCP cổng {port}"))
}

pub fn remove_firewall_rule(port: u16) -> LauncherResult<String> {
    if port == 0 {
        return Err(LauncherError::InvalidInput(
            "Cổng server không hợp lệ (port = 0)".to_string(),
        ));
    }
    let name = firewall_rule_name(port);
    let name_arg = format!("name={name}");
    let args = ["advfirewall", "firewall", "delete", "rule", &name_arg];
    let out = run_netsh(&args)?;
    if out.status.success() {
        return Ok(format!("Đã xóa quy tắc Firewall {name}"));
    }
    if show_firewall_rule_stdout(port).is_none() {
        return Ok(format!("Quy tắc Firewall {name} không tồn tại"));
    }

    run_elevated_windows("netsh.exe", &args.join(" "))?;
    Ok(format!("Đã xóa quy tắc Firewall {name}"))
}

fn build_lan_upnp_client() -> LauncherResult<reqwest::Client> {
    ensure_rustls_provider();
    reqwest::Client::builder()
        .user_agent(LAUNCHER_USER_AGENT)
        .no_proxy()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(6))
        .build()
        .map_err(|e| LauncherError::Network(format!("Không thể khởi tạo UPnP HTTP client: {e}")))
}

fn build_ipv4_probe_client() -> LauncherResult<reqwest::Client> {
    ensure_rustls_provider();
    reqwest::Client::builder()
        .user_agent(LAUNCHER_USER_AGENT)
        .no_proxy()
        .local_address(Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)))
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| {
            LauncherError::Network(format!("Không thể khởi tạo IPv4 probe HTTP client: {e}"))
        })
}

fn build_ipv6_upnp_client(local_v6: Ipv6Addr) -> LauncherResult<reqwest::Client> {
    ensure_rustls_provider();
    reqwest::Client::builder()
        .user_agent(LAUNCHER_USER_AGENT)
        .no_proxy()
        .local_address(Some(IpAddr::V6(local_v6)))
        .connect_timeout(Duration::from_millis(2500))
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|e| {
            LauncherError::Network(format!("Không thể khởi tạo UPnP IPv6 HTTP client: {e}"))
        })
}

fn build_ipv6_probe_client(target_v6: Ipv6Addr) -> LauncherResult<reqwest::Client> {
    ensure_rustls_provider();
    reqwest::Client::builder()
        .user_agent(LAUNCHER_USER_AGENT)
        .no_proxy()
        .local_address(Some(IpAddr::V6(target_v6)))
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| {
            LauncherError::Network(format!("Không thể khởi tạo IPv6 probe HTTP client: {e}"))
        })
}

pub(crate) fn is_wan_behind_cgnat(wan_ip: Ipv4Addr, public_ip: Option<Ipv4Addr>) -> bool {
    is_cgnat_or_private_ipv4(wan_ip) || public_ip.is_some_and(|pub_ip| pub_ip != wan_ip)
}

fn push_unique_ipv4(target: &mut Vec<Ipv4Addr>, ip: Ipv4Addr) {
    if !target.contains(&ip) {
        target.push(ip);
    }
}

fn collect_lan_ipv4_candidates(preferred_local_ip: Option<Ipv4Addr>) -> (Vec<Ipv4Addr>, Vec<Ipv4Addr>) {
    let mut non_virtual_candidates = Vec::new();
    let mut fallback_candidates = Vec::new();

    if let Some(pinned) = preferred_local_ip {
        push_unique_ipv4(&mut non_virtual_candidates, pinned);
        return (non_virtual_candidates, fallback_candidates);
    }

    let (primary, interfaces) = collect_local_interfaces();
    for iface in &interfaces {
        if !iface.is_virtual && rfc1918_rank(iface.ipv4).is_some() {
            push_unique_ipv4(&mut non_virtual_candidates, iface.ipv4);
        }
    }
    for iface in &interfaces {
        if !iface.is_virtual && is_cgnat_or_private_ipv4(iface.ipv4) {
            push_unique_ipv4(&mut non_virtual_candidates, iface.ipv4);
        }
    }
    for iface in &interfaces {
        if iface.is_virtual && is_cgnat_or_private_ipv4(iface.ipv4) {
            push_unique_ipv4(&mut fallback_candidates, iface.ipv4);
        }
    }
    if non_virtual_candidates.is_empty() && fallback_candidates.is_empty() {
        if let Some(ip) = primary {
            push_unique_ipv4(&mut non_virtual_candidates, ip);
        }
    }

    (non_virtual_candidates, fallback_candidates)
}

struct UpnpServiceEndpoint {
    client: reqwest::Client,
    control_url: String,
    service_type: String,
    local_ip: Ipv4Addr,
    gateway_label: String,
}

fn build_upnp_add_port_mapping_body(
    service_type: &str,
    internal_port: u16,
    external_port: u16,
    local_ip: Ipv4Addr,
    lease_seconds: u32,
) -> String {
    format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:AddPortMapping xmlns:u="{service_type}">
      <NewRemoteHost></NewRemoteHost>
      <NewExternalPort>{external_port}</NewExternalPort>
      <NewProtocol>TCP</NewProtocol>
      <NewInternalPort>{internal_port}</NewInternalPort>
      <NewInternalClient>{local_ip}</NewInternalClient>
      <NewEnabled>1</NewEnabled>
      <NewPortMappingDescription>TerrariaLauncher</NewPortMappingDescription>
      <NewLeaseDuration>{lease_seconds}</NewLeaseDuration>
    </u:AddPortMapping>
  </s:Body>
</s:Envelope>"#
    )
}

async fn send_upnp_add_port_mapping_inner(
    client: &reqwest::Client,
    control_url: &str,
    service_type: &str,
    internal_port: u16,
    external_port: u16,
    local_ip: Ipv4Addr,
    lease_seconds: u32,
) -> Result<(), (bool, LauncherError)> {
    let add_body = build_upnp_add_port_mapping_body(
        service_type,
        internal_port,
        external_port,
        local_ip,
        lease_seconds,
    );
    let soap_resp = client
        .post(control_url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header("SOAPAction", format!("\"{service_type}#AddPortMapping\""))
        .body(add_body)
        .send()
        .await
        .map_err(|e| {
            (
                false,
                LauncherError::Network(format!("Lỗi gửi UPnP AddPortMapping: {e}")),
            )
        })?;

    if !soap_resp.status().is_success() {
        let err_body = soap_resp.text().await.unwrap_or_default();
        return Err((
            true,
            LauncherError::Network(format_upnp_soap_rejection(&err_body)),
        ));
    }

    Ok(())
}

async fn send_upnp_add_port_mapping(
    client: &reqwest::Client,
    control_url: &str,
    service_type: &str,
    internal_port: u16,
    external_port: u16,
    local_ip: Ipv4Addr,
    lease_seconds: u32,
) -> LauncherResult<()> {
    send_upnp_add_port_mapping_inner(
        client,
        control_url,
        service_type,
        internal_port,
        external_port,
        local_ip,
        lease_seconds,
    )
    .await
    .map_err(|(_, err)| err)
}

async fn add_port_mapping_with_lease_fallback(
    client: &reqwest::Client,
    control_url: &str,
    service_type: &str,
    internal_port: u16,
    external_port: u16,
    local_ip: Ipv4Addr,
) -> LauncherResult<u32> {
    match send_upnp_add_port_mapping_inner(
        client,
        control_url,
        service_type,
        internal_port,
        external_port,
        local_ip,
        0,
    )
    .await
    {
        Ok(()) => Ok(0),
        Err((true, _)) => {
            send_upnp_add_port_mapping(
                client,
                control_url,
                service_type,
                internal_port,
                external_port,
                local_ip,
                3600,
            )
            .await?;
            Ok(3600)
        }
        Err((false, err)) => Err(err),
    }
}

async fn discover_upnp_service_endpoint(
    internal_port: u16,
    external_port: u16,
    preferred_local_ip: Option<Ipv4Addr>,
) -> LauncherResult<UpnpServiceEndpoint> {
    if internal_port == 0 || external_port == 0 {
        return Err(LauncherError::InvalidInput(
            "Cổng server không hợp lệ (port = 0)".to_string(),
        ));
    }

    let (non_virtual_candidates, fallback_candidates) =
        collect_lan_ipv4_candidates(preferred_local_ip);

    if non_virtual_candidates.is_empty() && fallback_candidates.is_empty() {
        return Err(LauncherError::Network(
            "Không xác định được địa chỉ LAN IPv4 để mở UPnP".to_string(),
        ));
    }

    let (local_ip, location_url) = if !non_virtual_candidates.is_empty() {
        match discover_igd_location(&non_virtual_candidates).await {
            Ok(found) => found,
            Err(err) => {
                if fallback_candidates.is_empty() {
                    return Err(err);
                }
                discover_igd_location(&fallback_candidates).await?
            }
        }
    } else {
        discover_igd_location(&fallback_candidates).await?
    };

    let client = build_lan_upnp_client()?;
    let desc_resp = client
        .get(&location_url)
        .send()
        .await
        .map_err(|e| LauncherError::Network(format!("Lỗi tải mô tả UPnP router: {e}")))?;
    if !desc_resp.status().is_success() {
        return Err(LauncherError::Network(format!(
            "Router trả về HTTP {} khi đọc mô tả UPnP",
            desc_resp.status()
        )));
    }
    let desc_xml = desc_resp
        .text()
        .await
        .map_err(|e| LauncherError::Network(format!("Lỗi đọc XML router: {e}")))?;

    let gateway_label = parse_igd_friendly_name(&desc_xml);
    let (service_type, control_path) = parse_wan_control_service(&desc_xml).ok_or_else(|| {
        LauncherError::NotFound(
            "Router không cung cấp dịch vụ WANIPConnection/WANPPPConnection".to_string(),
        )
    })?;

    let control_url = resolve_upnp_url(&location_url, &control_path);
    Ok(UpnpServiceEndpoint {
        client,
        control_url,
        service_type,
        local_ip,
        gateway_label,
    })
}

async fn open_upnp_port_mapping_on(
    internal_port: u16,
    external_port: u16,
    preferred_local_ip: Option<Ipv4Addr>,
) -> LauncherResult<(UpnpLeaseInfo, String)> {
    let endpoint =
        discover_upnp_service_endpoint(internal_port, external_port, preferred_local_ip).await?;
    let lease_seconds = add_port_mapping_with_lease_fallback(
        &endpoint.client,
        &endpoint.control_url,
        &endpoint.service_type,
        internal_port,
        external_port,
        endpoint.local_ip,
    )
    .await?;

    let pub_client = build_ipv4_probe_client().ok();
    let (external_ip, public_ip) = tokio::join!(
        query_upnp_external_ip(
            &endpoint.client,
            &endpoint.control_url,
            &endpoint.service_type,
        ),
        async {
            match pub_client.as_ref() {
                Some(c) => query_public_ipv4_with_client(c, Duration::from_millis(2500)).await,
                None => None,
            }
        }
    );
    let is_cgnat = external_ip.is_some_and(|wan_ip| is_wan_behind_cgnat(wan_ip, public_ip));

    Ok((
        UpnpLeaseInfo {
            control_url: endpoint.control_url,
            service_type: endpoint.service_type,
            external_ip,
            local_ip: endpoint.local_ip,
            port: external_port,
            is_cgnat,
            lease_seconds,
        },
        endpoint.gateway_label,
    ))
}

async fn open_upnp_port_mapping(
    internal_port: u16,
    external_port: u16,
) -> LauncherResult<(UpnpLeaseInfo, String)> {
    open_upnp_port_mapping_on(internal_port, external_port, None).await
}

pub async fn open_upnp_port(port: u16) -> LauncherResult<UpnpLeaseInfo> {
    open_upnp_port_mapping(port, port)
        .await
        .map(|(lease, _)| lease)
}

async fn send_upnp_delete_port_mapping(
    client: &reqwest::Client,
    control_url: &str,
    service_type: &str,
    external_port: u16,
) -> LauncherResult<()> {
    let del_body = format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:DeletePortMapping xmlns:u="{}">
      <NewRemoteHost></NewRemoteHost>
      <NewExternalPort>{}</NewExternalPort>
      <NewProtocol>TCP</NewProtocol>
    </u:DeletePortMapping>
  </s:Body>
</s:Envelope>"#,
        service_type, external_port
    );

    let resp = client
        .post(control_url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header(
            "SOAPAction",
            format!("\"{}#DeletePortMapping\"", service_type),
        )
        .body(del_body)
        .send()
        .await
        .map_err(|e| LauncherError::Network(format!("Lỗi gửi UPnP DeletePortMapping: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        return Err(LauncherError::Network(format!(
            "Router từ chối UPnP DeletePortMapping ({status}): {err_body}"
        )));
    }
    Ok(())
}

pub async fn close_upnp_port(lease: &UpnpLeaseInfo) -> LauncherResult<()> {
    if lease.port == 0 {
        return Err(LauncherError::InvalidInput(
            "Cổng UPnP không hợp lệ (port = 0)".to_string(),
        ));
    }
    let client = build_lan_upnp_client()?;
    send_upnp_delete_port_mapping(
        &client,
        &lease.control_url,
        &lease.service_type,
        lease.port,
    )
    .await
}

async fn resolve_upnp_port_status(
    lease: &UpnpLeaseInfo,
    queried_public_ip: Option<Option<Ipv4Addr>>,
    gateway_label: String,
    cancel: &CancellationToken,
) -> Option<UpnpPortStatus> {
    let known_public_ip = queried_public_ip.flatten();
    if let Some(wan_ip) = lease.external_ip {
        if lease.is_cgnat || is_wan_behind_cgnat(wan_ip, known_public_ip) {
            return Some(UpnpPortStatus::CarrierGradeNat {
                wan_ip,
                public_ip: known_public_ip,
                external_port: lease.port,
            });
        }
        return Some(UpnpPortStatus::Mapped {
            external_ip: wan_ip,
            external_port: lease.port,
            gateway_label,
        });
    }
    if lease.is_cgnat {
        return Some(UpnpPortStatus::CarrierGradeNat {
            wan_ip: Ipv4Addr::UNSPECIFIED,
            public_ip: known_public_ip,
            external_port: lease.port,
        });
    }

    let fallback_pub = match queried_public_ip {
        Some(already) => already,
        None => match build_ipv4_probe_client() {
            Ok(client) => {
                tokio::select! {
                    _ = cancel.cancelled() => return None,
                    ip = query_public_ipv4_with_client(&client, Duration::from_millis(2500)) => ip,
                }
            }
            Err(_) => None,
        },
    };
    if let Some(pub_ip) = fallback_pub {
        if is_cgnat_or_private_ipv4(pub_ip) {
            Some(UpnpPortStatus::CarrierGradeNat {
                wan_ip: pub_ip,
                public_ip: Some(pub_ip),
                external_port: lease.port,
            })
        } else {
            Some(UpnpPortStatus::Mapped {
                external_ip: pub_ip,
                external_port: lease.port,
                gateway_label,
            })
        }
    } else {
        Some(UpnpPortStatus::Unavailable(
            "Đã mở cổng UPnP nhưng không xác định được IP WAN công cộng".to_string(),
        ))
    }
}

async fn run_direct_host_upnp_mapping(
    endpoint: UpnpServiceEndpoint,
    host_port: u16,
    pub_ip_endpoints: &[&str],
    cancel: &CancellationToken,
    has_upnp_lease: &AtomicBool,
) -> (
    Option<UpnpLeaseInfo>,
    Option<reqwest::Client>,
    Option<UpnpPortStatus>,
) {
    let pub_client = build_ipv4_probe_client().ok();
    let (wan_ip, public_ip) = tokio::select! {
        _ = cancel.cancelled() => return (None, None, None),
        pair = async {
            let wan_fut = query_upnp_external_ip(
                &endpoint.client,
                &endpoint.control_url,
                &endpoint.service_type,
            );
            let pub_fut = async {
                match pub_client.as_ref() {
                    Some(c) => {
                        query_public_ipv4_endpoints(
                            c,
                            pub_ip_endpoints,
                            Duration::from_millis(2500),
                            Duration::from_secs(4),
                        )
                        .await
                    }
                    None => None,
                }
            };
            tokio::join!(wan_fut, pub_fut)
        } => pair,
    };

    if cancel.is_cancelled() {
        return (None, None, None);
    }

    if let Some(w_ip) = wan_ip {
        if is_wan_behind_cgnat(w_ip, public_ip) {
            return (
                None,
                None,
                Some(UpnpPortStatus::CarrierGradeNat {
                    wan_ip: w_ip,
                    public_ip,
                    external_port: host_port,
                }),
            );
        }
    }

    has_upnp_lease.store(true, Ordering::SeqCst);
    let lease_seconds = match add_port_mapping_with_lease_fallback(
        &endpoint.client,
        &endpoint.control_url,
        &endpoint.service_type,
        host_port,
        host_port,
        endpoint.local_ip,
    )
    .await
    {
        Ok(seconds) => seconds,
        Err(err) => {
            return (
                None,
                None,
                Some(UpnpPortStatus::Unavailable(clean_launcher_error_msg(err))),
            );
        }
    };

    let mut lease = UpnpLeaseInfo {
        control_url: endpoint.control_url.clone(),
        service_type: endpoint.service_type.clone(),
        external_ip: wan_ip,
        local_ip: endpoint.local_ip,
        port: host_port,
        is_cgnat: false,
        lease_seconds,
    };

    if cancel.is_cancelled() {
        return (Some(lease), Some(endpoint.client), None);
    }

    if lease.external_ip.is_none() {
        let external_ip = tokio::select! {
            _ = cancel.cancelled() => return (Some(lease), Some(endpoint.client), None),
            ip = query_upnp_external_ip(
                &endpoint.client,
                &endpoint.control_url,
                &endpoint.service_type,
            ) => ip,
        };
        lease.external_ip = external_ip;
        lease.is_cgnat = external_ip.is_some_and(|w| is_wan_behind_cgnat(w, public_ip));
    }

    let status =
        resolve_upnp_port_status(&lease, Some(public_ip), endpoint.gateway_label, cancel).await;
    (Some(lease), Some(endpoint.client), status)
}

async fn setup_direct_host_upnp(
    local_target_port: u16,
    host_port: u16,
    bind_ipv4: Option<Ipv4Addr>,
    try_upnp: bool,
    cleanup_local_upnp_on_start: bool,
    cancel: &CancellationToken,
    has_upnp_lease: &AtomicBool,
) -> (
    Option<UpnpLeaseInfo>,
    Option<reqwest::Client>,
    Option<UpnpPortStatus>,
) {
    let discover_port = if try_upnp {
        host_port
    } else {
        local_target_port
    };
    let endpoint = tokio::select! {
        _ = cancel.cancelled() => return (None, None, None),
        res = discover_upnp_service_endpoint(discover_port, discover_port, bind_ipv4) => match res {
            Ok(ep) => ep,
            Err(err) => {
                return if try_upnp {
                    (
                        None,
                        None,
                        Some(UpnpPortStatus::Unavailable(clean_launcher_error_msg(err))),
                    )
                } else {
                    (None, None, None)
                };
            }
        },
    };

    if cancel.is_cancelled() {
        return (None, None, None);
    }

    if cleanup_local_upnp_on_start && (!try_upnp || host_port != local_target_port) {
        tokio::select! {
            _ = cancel.cancelled() => return (None, None, None),
            _ = send_upnp_delete_port_mapping(
                &endpoint.client,
                &endpoint.control_url,
                &endpoint.service_type,
                local_target_port,
            ) => {}
        }
    }

    if !try_upnp || cancel.is_cancelled() {
        return (None, None, None);
    }

    run_direct_host_upnp_mapping(
        endpoint,
        host_port,
        &PUBLIC_IPV4_ENDPOINTS,
        cancel,
        has_upnp_lease,
    )
    .await
}

pub(crate) fn extract_igd_location_from_ssdp(packet: &str) -> Option<String> {
    let mut lines = packet.lines().map(str::trim).filter(|l| !l.is_empty());
    let first_line = lines.next()?;
    let upper_first = first_line.to_ascii_uppercase();
    if upper_first.starts_with("HTTP/") && upper_first.split_whitespace().nth(1) != Some("200") {
        return None;
    }

    let mut location: Option<String> = None;
    let mut is_igd = false;

    for line in std::iter::once(first_line).chain(lines) {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        if key.eq_ignore_ascii_case("LOCATION") {
            let lower = val.to_ascii_lowercase();
            if lower.starts_with("http://") || lower.starts_with("https://") {
                location = Some(val.to_string());
            }
        } else if key.eq_ignore_ascii_case("ST")
            || key.eq_ignore_ascii_case("USN")
            || key.eq_ignore_ascii_case("NT")
        {
            let lower = val.to_ascii_lowercase();
            if lower.contains("internetgatewaydevice")
                || lower.contains("wanipconnection")
                || lower.contains("wanpppconnection")
                || lower.contains("wanipv6firewallcontrol")
            {
                is_igd = true;
            }
        }
    }

    if is_igd {
        location
    } else {
        None
    }
}

async fn probe_ssdp_on_interface(local_ip: Ipv4Addr) -> Option<(Ipv4Addr, String)> {
    let bind_addr = SocketAddrV4::new(local_ip, 0);
    let socket = UdpSocket::bind(bind_addr).await.ok()?;

    for target in SSDP_SEARCH_TARGETS {
        let req = format!(
            "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {target}\r\n\r\n"
        );
        let _ = socket.send_to(req.as_bytes(), SSDP_MULTICAST).await;
    }

    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut consecutive_errs = 0u8;
    while Instant::now() < deadline {
        let rem = deadline.saturating_duration_since(Instant::now());
        if rem.is_zero() {
            break;
        }
        match timeout(rem, socket.recv_from(&mut buf)).await {
            Ok(Ok((len, _))) => {
                consecutive_errs = 0;
                let text = String::from_utf8_lossy(&buf[..len]);
                if let Some(location) = extract_igd_location_from_ssdp(&text) {
                    return Some((local_ip, location));
                }
            }
            Ok(Err(_)) => {
                consecutive_errs = consecutive_errs.saturating_add(1);
                if consecutive_errs >= 16 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            Err(_) => break,
        }
    }
    None
}

async fn discover_igd_location(candidates: &[Ipv4Addr]) -> LauncherResult<(Ipv4Addr, String)> {
    if candidates.is_empty() {
        return Err(LauncherError::NotFound(
            "Không tìm thấy Router hỗ trợ UPnP trong mạng LAN".to_string(),
        ));
    }

    let mut set = JoinSet::new();
    for &ip in candidates {
        let _ = set.spawn(probe_ssdp_on_interface(ip));
    }

    while let Some(res) = set.join_next().await {
        if let Ok(Some(hit)) = res {
            return Ok(hit);
        }
    }

    Err(LauncherError::NotFound(
        "Không tìm thấy Router hỗ trợ UPnP trong mạng LAN".to_string(),
    ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ServiceTag {
    None,
    ServiceType,
    ControlUrl,
}

fn parse_upnp_services(xml: &str) -> Vec<(String, String)> {
    let mut reader = Reader::from_str(xml);
    let mut in_service = false;
    let mut cur_tag = ServiceTag::None;
    let mut cur_type = String::new();
    let mut cur_ctrl = String::new();
    let mut services = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let local = e.local_name();
                if local.as_ref().eq_ignore_ascii_case("service") {
                    in_service = true;
                    cur_tag = ServiceTag::None;
                    cur_type.clear();
                    cur_ctrl.clear();
                } else if in_service {
                    if local.as_ref().eq_ignore_ascii_case("servicetype") {
                        cur_tag = ServiceTag::ServiceType;
                        cur_type.clear();
                    } else if local.as_ref().eq_ignore_ascii_case("controlurl") {
                        cur_tag = ServiceTag::ControlUrl;
                        cur_ctrl.clear();
                    } else {
                        cur_tag = ServiceTag::None;
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                if in_service {
                    let local = e.local_name();
                    if local.as_ref().eq_ignore_ascii_case("servicetype") {
                        cur_type.clear();
                    } else if local.as_ref().eq_ignore_ascii_case("controlurl") {
                        cur_ctrl.clear();
                    }
                    cur_tag = ServiceTag::None;
                }
            }
            Ok(Event::Text(t)) if in_service => match cur_tag {
                ServiceTag::ServiceType => cur_type.push_str(t.as_ref()),
                ServiceTag::ControlUrl => cur_ctrl.push_str(t.as_ref()),
                ServiceTag::None => {}
            },
            Ok(Event::End(e)) => {
                let local = e.local_name();
                if local.as_ref().eq_ignore_ascii_case("service") && in_service {
                    in_service = false;
                    let st = cur_type.trim();
                    let ctrl = cur_ctrl.trim();
                    if !ctrl.is_empty() {
                        services.push((st.to_string(), ctrl.to_string()));
                    }
                }
                cur_tag = ServiceTag::None;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    services
}

pub(crate) fn parse_wan_control_service(xml: &str) -> Option<(String, String)> {
    let services = parse_upnp_services(xml);
    let ip_conn = services
        .iter()
        .find(|(st, _)| st.contains("WANIPConnection"))
        .cloned();
    ip_conn.or_else(|| {
        services
            .into_iter()
            .find(|(st, _)| st.contains("WANPPPConnection"))
    })
}

pub(crate) fn parse_wan_ipv6_firewall_service(xml: &str) -> Option<(String, String)> {
    parse_upnp_services(xml)
        .into_iter()
        .find(|(st, _)| st.to_ascii_lowercase().contains("wanipv6firewallcontrol"))
}

fn find_xml_tag_value<T>(
    xml: &str,
    tag_name: &str,
    mut parse: impl FnMut(&str) -> Option<T>,
) -> Option<T> {
    let mut reader = Reader::from_str(xml);
    let mut in_tag = false;
    let mut buf = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if e.local_name().as_ref().eq_ignore_ascii_case(tag_name) {
                    in_tag = true;
                    buf.clear();
                }
            }
            Ok(Event::Empty(e)) => {
                if e.local_name().as_ref().eq_ignore_ascii_case(tag_name) {
                    in_tag = false;
                    buf.clear();
                }
            }
            Ok(Event::Text(t)) if in_tag => {
                buf.push_str(t.as_ref());
            }
            Ok(Event::End(e)) => {
                if e.local_name().as_ref().eq_ignore_ascii_case(tag_name) && in_tag {
                    in_tag = false;
                    if let Some(parsed) = parse(buf.trim()) {
                        return Some(parsed);
                    }
                    buf.clear();
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    None
}

pub(crate) fn parse_igd_friendly_name(xml: &str) -> String {
    find_xml_tag_value(xml, "friendlyName", |s| {
        (!s.is_empty()).then(|| s.to_string())
    })
    .unwrap_or_else(|| "IGD Router".to_string())
}

fn resolve_upnp_url(base: &str, control_path: &str) -> String {
    if control_path.starts_with("http://") || control_path.starts_with("https://") {
        return control_path.to_string();
    }
    if let Some(scheme_idx) = base.find("://") {
        let after = &base[scheme_idx + 3..];
        let origin_end = after
            .find('/')
            .map(|i| scheme_idx + 3 + i)
            .unwrap_or(base.len());
        let origin = &base[..origin_end];
        if control_path.starts_with('/') {
            return format!("{origin}{control_path}");
        }
        let dir = match base[origin_end..].rfind('/') {
            Some(slash) => &base[..origin_end + slash + 1],
            None => return format!("{origin}/{control_path}"),
        };
        return format!("{dir}{control_path}");
    }
    control_path.to_string()
}

pub(crate) fn parse_upnp_external_ip_xml(xml: &str) -> Option<Ipv4Addr> {
    find_xml_tag_value(xml, "NewExternalIPAddress", |s| s.parse::<Ipv4Addr>().ok())
}

fn parse_upnp_bool(raw: &str) -> Option<bool> {
    let trimmed = raw.trim();
    if trimmed == "1" || trimmed.eq_ignore_ascii_case("true") {
        Some(true)
    } else if trimmed == "0" || trimmed.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FirewallStatusTag {
    None,
    FirewallEnabled,
    InboundPinholeAllowed,
}

pub(crate) fn parse_upnp_ipv6_firewall_status_xml(xml: &str) -> Option<(bool, bool)> {
    let mut reader = Reader::from_str(xml);
    let mut active_tag = FirewallStatusTag::None;
    let mut buf = String::new();
    let mut firewall_enabled: Option<bool> = None;
    let mut inbound_allowed: Option<bool> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let local = e.local_name();
                if local.as_ref().eq_ignore_ascii_case("FirewallEnabled") {
                    active_tag = FirewallStatusTag::FirewallEnabled;
                    buf.clear();
                } else if local.as_ref().eq_ignore_ascii_case("InboundPinholeAllowed") {
                    active_tag = FirewallStatusTag::InboundPinholeAllowed;
                    buf.clear();
                } else {
                    active_tag = FirewallStatusTag::None;
                }
            }
            Ok(Event::Empty(e)) => {
                let local = e.local_name();
                if local.as_ref().eq_ignore_ascii_case("FirewallEnabled")
                    || local.as_ref().eq_ignore_ascii_case("InboundPinholeAllowed")
                {
                    active_tag = FirewallStatusTag::None;
                    buf.clear();
                }
            }
            Ok(Event::Text(t)) if active_tag != FirewallStatusTag::None => {
                buf.push_str(t.as_ref());
            }
            Ok(Event::End(e)) => {
                let local = e.local_name();
                if local.as_ref().eq_ignore_ascii_case("FirewallEnabled")
                    && active_tag == FirewallStatusTag::FirewallEnabled
                {
                    active_tag = FirewallStatusTag::None;
                    firewall_enabled = parse_upnp_bool(&buf);
                    buf.clear();
                } else if local.as_ref().eq_ignore_ascii_case("InboundPinholeAllowed")
                    && active_tag == FirewallStatusTag::InboundPinholeAllowed
                {
                    active_tag = FirewallStatusTag::None;
                    inbound_allowed = parse_upnp_bool(&buf);
                    buf.clear();
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    match (firewall_enabled, inbound_allowed) {
        (Some(fw), Some(inbound)) => Some((fw, inbound)),
        _ => None,
    }
}

pub(crate) fn parse_upnp_pinhole_unique_id_xml(xml: &str) -> Option<u32> {
    find_xml_tag_value(xml, "UniqueID", |s| s.parse::<u32>().ok())
}

pub(crate) fn is_routable_non_link_local_ipv6(ip: Ipv6Addr) -> bool {
    !ip.is_loopback()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && (ip.segments()[0] & 0xffc0) != 0xfe80
}

fn split_http_url_authority(url: &str) -> Option<(&str, &str, &str)> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    let (scheme, rest) = if lower.starts_with("http://") {
        ("http://", &trimmed[7..])
    } else if lower.starts_with("https://") {
        ("https://", &trimmed[8..])
    } else {
        return None;
    };
    let (authority, suffix) = match rest.find(['/', '?', '#']) {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return None;
    }
    Some((scheme, authority, suffix))
}

fn url_host_is_link_local_ipv6(url: &str) -> bool {
    let Some((_, authority, _)) = split_http_url_authority(url) else {
        return false;
    };
    let Some(after_open) = authority.strip_prefix('[') else {
        return false;
    };
    let Some((inside, _)) = after_open.split_once(']') else {
        return false;
    };
    let addr_part = inside.split('%').next().unwrap_or(inside);
    addr_part
        .parse::<Ipv6Addr>()
        .is_ok_and(|ip| (ip.segments()[0] & 0xffc0) == 0xfe80)
}

fn url_host_is_ipv6(url: &str) -> bool {
    split_http_url_authority(url).is_some_and(|(_, authority, _)| authority.starts_with('['))
}

pub(crate) fn rewrite_http_url_ipv6_host(url: &str, host_v6: Ipv6Addr) -> Option<String> {
    let (scheme, authority, suffix) = split_http_url_authority(url)?;
    let port_part = if let Some(after_open) = authority.strip_prefix('[') {
        let (_host_inside, after_close) = after_open.split_once(']')?;
        if !after_close.is_empty() && !after_close.starts_with(':') {
            return None;
        }
        if let Some(p) = after_close.strip_prefix(':') {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
        }
        after_close
    } else if let Some((host_only, port_str)) = authority.split_once(':') {
        if host_only.is_empty()
            || port_str.is_empty()
            || !port_str.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        &authority[host_only.len()..]
    } else {
        ""
    };

    Some(format!("{scheme}[{host_v6}]{port_part}{suffix}"))
}

fn push_unique_str(target: &mut Vec<String>, value: String) {
    if !target.contains(&value) {
        target.push(value);
    }
}

pub(crate) fn collect_ipv6_ssdp_candidate_locations(
    packet: &str,
    sender: SocketAddr,
    out: &mut Vec<String>,
) {
    let Some(loc) = extract_igd_location_from_ssdp(packet) else {
        return;
    };
    let SocketAddr::V6(sender_v6) = sender else {
        return;
    };
    if is_routable_non_link_local_ipv6(*sender_v6.ip()) {
        if let Some(rewritten) = rewrite_http_url_ipv6_host(&loc, *sender_v6.ip()) {
            push_unique_str(out, rewritten);
        }
    }
    if !url_host_is_link_local_ipv6(&loc) {
        push_unique_str(out, loc);
    }
}

async fn probe_ipv6_ssdp_locations(local_v6: Ipv6Addr) -> Vec<String> {
    let socket = match UdpSocket::bind((local_v6, 0)).await {
        Ok(sock) => sock,
        Err(_) => match UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).await {
            Ok(sock) => sock,
            Err(_) => return Vec::new(),
        },
    };

    let multicast_targets: [(&str, SocketAddrV6); 2] = [
        (
            "[ff02::c]:1900",
            SocketAddrV6::new(Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0x000c), 1900, 0, 0),
        ),
        (
            "[ff05::c]:1900",
            SocketAddrV6::new(Ipv6Addr::new(0xff05, 0, 0, 0, 0, 0, 0, 0x000c), 1900, 0, 0),
        ),
    ];

    for (host_hdr, mcast_addr) in multicast_targets {
        for target in IPV6_SSDP_SEARCH_TARGETS {
            let req = format!(
                "M-SEARCH * HTTP/1.1\r\nHOST: {host_hdr}\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {target}\r\n\r\n"
            );
            let _ = socket.send_to(req.as_bytes(), mcast_addr).await;
        }
    }

    let mut locations = Vec::new();
    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_millis(2200);
    let mut consecutive_errs = 0u8;
    while Instant::now() < deadline {
        let rem = deadline.saturating_duration_since(Instant::now());
        if rem.is_zero() {
            break;
        }
        match timeout(rem, socket.recv_from(&mut buf)).await {
            Ok(Ok((len, sender))) => {
                consecutive_errs = 0;
                let text = String::from_utf8_lossy(&buf[..len]);
                collect_ipv6_ssdp_candidate_locations(&text, sender, &mut locations);
            }
            Ok(Err(_)) => {
                consecutive_errs = consecutive_errs.saturating_add(1);
                if consecutive_errs >= 16 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            Err(_) => break,
        }
    }
    locations
}

pub(crate) fn parse_upnp_soap_fault(xml: &str) -> Option<(String, String)> {
    let mut reader = Reader::from_str(xml);
    let mut in_code = false;
    let mut in_desc = false;
    let mut code = String::new();
    let mut desc = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let local = e.local_name();
                if local.as_ref().eq_ignore_ascii_case("errorCode") {
                    in_code = true;
                    code.clear();
                } else if local.as_ref().eq_ignore_ascii_case("errorDescription") {
                    in_desc = true;
                    desc.clear();
                }
            }
            Ok(Event::Text(t)) => {
                if in_code {
                    code.push_str(t.as_ref());
                } else if in_desc {
                    desc.push_str(t.as_ref());
                }
            }
            Ok(Event::End(e)) => {
                let local = e.local_name();
                if local.as_ref().eq_ignore_ascii_case("errorCode") {
                    in_code = false;
                } else if local.as_ref().eq_ignore_ascii_case("errorDescription") {
                    in_desc = false;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    let c = code.trim().to_string();
    let d = desc.trim().to_string();
    if c.is_empty() && d.is_empty() {
        None
    } else {
        Some((c, d))
    }
}

fn format_upnp_soap_rejection(err_body: &str) -> String {
    match parse_upnp_soap_fault(err_body) {
        Some((code, desc)) if code == "606" => format!(
            "Router từ chối UPnP (mã {code}: {desc}). Router hiện chặn mở cổng tự động — hãy dùng VPN LAN ảo (Radmin VPN / Tailscale / ZeroTier) hoặc mở Port Forwarding thủ công trên Modem."
        ),
        Some((code, desc)) if code == "718" => format!(
            "Router báo cổng đang bị tiến trình/máy khác chiếm trên UPnP (mã {code}: {desc})."
        ),
        Some((code, desc)) => format!("Router từ chối UPnP (mã {code}: {desc})."),
        None => {
            let trimmed = err_body.trim();
            if trimmed.is_empty() {
                "Router từ chối UPnP AddPortMapping.".to_string()
            } else {
                format!("Router từ chối UPnP AddPortMapping: {trimmed}")
            }
        }
    }
}

fn format_upnp_ipv6_soap_rejection(action: &str, status: Option<reqwest::StatusCode>, err_body: &str) -> String {
    match parse_upnp_soap_fault(err_body) {
        Some((code, desc)) if code == "606" => format!(
            "Router từ chối mở cổng IPv6 UPnP (mã {code}: {desc})."
        ),
        Some((code, desc)) => format!("Router từ chối UPnP {action} (mã {code}: {desc})."),
        None => {
            let trimmed = err_body.trim();
            if trimmed.is_empty() {
                match status {
                    Some(code) => format!("Router từ chối UPnP {action} (HTTP {code})."),
                    None => format!("Router từ chối UPnP {action}."),
                }
            } else {
                format!("Router từ chối UPnP {action}: {trimmed}")
            }
        }
    }
}

async fn post_upnp_soap(
    client: &reqwest::Client,
    control_url: &str,
    service_type: &str,
    action: &str,
    body: String,
) -> Result<String, (Option<reqwest::StatusCode>, String)> {
    let resp = client
        .post(control_url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header("SOAPAction", format!("\"{service_type}#{action}\""))
        .body(body)
        .send()
        .await
        .map_err(|e| (None, format!("Lỗi gửi UPnP {action}: {e}")))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err((Some(status), text));
    }
    Ok(text)
}

async fn query_upnp_external_ip(
    client: &reqwest::Client,
    control_url: &str,
    service_type: &str,
) -> Option<Ipv4Addr> {
    let body = format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:GetExternalIPAddress xmlns:u="{service_type}"/>
  </s:Body>
</s:Envelope>"#
    );
    let xml = post_upnp_soap(
        client,
        control_url,
        service_type,
        "GetExternalIPAddress",
        body,
    )
    .await
    .ok()?;
    parse_upnp_external_ip_xml(&xml)
}

async fn send_upnp_get_ipv6_firewall_status(
    client: &reqwest::Client,
    control_url: &str,
    service_type: &str,
) -> Result<(bool, bool), (Option<reqwest::StatusCode>, String)> {
    let body = format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:GetFirewallStatus xmlns:u="{service_type}"/>
  </s:Body>
</s:Envelope>"#
    );
    let xml = post_upnp_soap(client, control_url, service_type, "GetFirewallStatus", body).await?;
    parse_upnp_ipv6_firewall_status_xml(&xml).ok_or_else(|| {
        (
            None,
            "Phản hồi GetFirewallStatus từ router không hợp lệ".to_string(),
        )
    })
}

async fn send_upnp_add_ipv6_pinhole(
    client: &reqwest::Client,
    control_url: &str,
    service_type: &str,
    internal_client: Ipv6Addr,
    internal_port: u16,
    lease_seconds: u32,
) -> LauncherResult<u32> {
    let body = format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:AddPinhole xmlns:u="{service_type}">
      <RemoteHost></RemoteHost>
      <RemotePort>0</RemotePort>
      <InternalClient>{internal_client}</InternalClient>
      <InternalPort>{internal_port}</InternalPort>
      <Protocol>6</Protocol>
      <LeaseTime>{lease_seconds}</LeaseTime>
    </u:AddPinhole>
  </s:Body>
</s:Envelope>"#
    );
    let xml = post_upnp_soap(client, control_url, service_type, "AddPinhole", body)
        .await
        .map_err(|(status, err_body)| {
            LauncherError::Network(format_upnp_ipv6_soap_rejection(
                "AddPinhole",
                status,
                &err_body,
            ))
        })?;
    parse_upnp_pinhole_unique_id_xml(&xml).ok_or_else(|| {
        LauncherError::Network("Router không trả về UniqueID sau AddPinhole".to_string())
    })
}

async fn send_upnp_update_ipv6_pinhole(
    client: &reqwest::Client,
    control_url: &str,
    service_type: &str,
    unique_id: u32,
    lease_seconds: u32,
) -> LauncherResult<()> {
    let body = format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:UpdatePinhole xmlns:u="{service_type}">
      <UniqueID>{unique_id}</UniqueID>
      <NewLeaseTime>{lease_seconds}</NewLeaseTime>
    </u:UpdatePinhole>
  </s:Body>
</s:Envelope>"#
    );
    post_upnp_soap(client, control_url, service_type, "UpdatePinhole", body)
        .await
        .map(|_| ())
        .map_err(|(status, err_body)| {
            LauncherError::Network(format_upnp_ipv6_soap_rejection(
                "UpdatePinhole",
                status,
                &err_body,
            ))
        })
}

async fn send_upnp_delete_ipv6_pinhole(
    client: &reqwest::Client,
    control_url: &str,
    service_type: &str,
    unique_id: u32,
) -> LauncherResult<()> {
    let body = format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:DeletePinhole xmlns:u="{service_type}">
      <UniqueID>{unique_id}</UniqueID>
    </u:DeletePinhole>
  </s:Body>
</s:Envelope>"#
    );
    post_upnp_soap(client, control_url, service_type, "DeletePinhole", body)
        .await
        .map(|_| ())
        .map_err(|(status, err_body)| {
            LauncherError::Network(format_upnp_ipv6_soap_rejection(
                "DeletePinhole",
                status,
                &err_body,
            ))
        })
}

pub async fn close_ipv6_pinhole(lease: &Ipv6PinholeLeaseInfo) -> LauncherResult<()> {
    if lease.internal_port == 0 || lease.control_url.trim().is_empty() {
        return Err(LauncherError::InvalidInput(
            "Thông tin IPv6 Pinhole không hợp lệ".to_string(),
        ));
    }
    let client = if url_host_is_ipv6(&lease.control_url) {
        build_ipv6_upnp_client(lease.internal_client).or_else(|_| build_lan_upnp_client())?
    } else {
        build_lan_upnp_client()?
    };
    send_upnp_delete_ipv6_pinhole(
        &client,
        &lease.control_url,
        &lease.service_type,
        lease.unique_id,
    )
    .await
}

enum Ipv6PinholeCandidateResult {
    Opened(Ipv6PinholeLeaseInfo, reqwest::Client, String),
    FirewallDisabled,
    Rejected(String),
    Unsupported,
    Cancelled,
}

async fn try_ipv6_pinhole_on_candidate(
    client: &reqwest::Client,
    desc_url: &str,
    local_v6: Ipv6Addr,
    host_port: u16,
    cancel: &CancellationToken,
    has_upnp_lease: &AtomicBool,
) -> Ipv6PinholeCandidateResult {
    let desc_resp = tokio::select! {
        _ = cancel.cancelled() => return Ipv6PinholeCandidateResult::Cancelled,
        res = client.get(desc_url).send() => match res {
            Ok(resp) => resp,
            Err(_) => return Ipv6PinholeCandidateResult::Unsupported,
        },
    };
    if !desc_resp.status().is_success() {
        return Ipv6PinholeCandidateResult::Unsupported;
    }
    let Ok(desc_xml) = desc_resp.text().await else {
        return Ipv6PinholeCandidateResult::Unsupported;
    };

    let gateway_label = parse_igd_friendly_name(&desc_xml);
    let explicit_service = parse_wan_ipv6_firewall_service(&desc_xml);
    let has_explicit_service = explicit_service.is_some();
    let (service_type, control_url) = match explicit_service {
        Some((st, path)) => (st, resolve_upnp_url(desc_url, &path)),
        None => (
            UPNP_IPV6_FIREWALL_SERVICE.to_string(),
            resolve_upnp_url(desc_url, "/ctl/IP6FCtl"),
        ),
    };

    let status_res = tokio::select! {
        _ = cancel.cancelled() => return Ipv6PinholeCandidateResult::Cancelled,
        res = send_upnp_get_ipv6_firewall_status(client, &control_url, &service_type) => res,
    };

    match status_res {
        Ok((firewall_enabled, inbound_allowed)) => {
            if !firewall_enabled {
                return Ipv6PinholeCandidateResult::FirewallDisabled;
            }
            if !inbound_allowed {
                return Ipv6PinholeCandidateResult::Rejected(
                    "Router không cho phép mở cổng IPv6 qua UPnP (InboundPinholeAllowed = 0)"
                        .to_string(),
                );
            }
        }
        Err((status, err_body)) => {
            if has_explicit_service && parse_upnp_soap_fault(&err_body).is_some() {
                return Ipv6PinholeCandidateResult::Rejected(format_upnp_ipv6_soap_rejection(
                    "GetFirewallStatus",
                    status,
                    &err_body,
                ));
            }
            return Ipv6PinholeCandidateResult::Unsupported;
        }
    }

    if cancel.is_cancelled() {
        return Ipv6PinholeCandidateResult::Cancelled;
    }

    match send_upnp_add_ipv6_pinhole(client, &control_url, &service_type, local_v6, host_port, 3600)
        .await
    {
        Ok(unique_id) => {
            has_upnp_lease.store(true, Ordering::SeqCst);
            let lease = Ipv6PinholeLeaseInfo {
                control_url,
                service_type,
                internal_client: local_v6,
                internal_port: host_port,
                unique_id,
                lease_seconds: 3600,
            };
            Ipv6PinholeCandidateResult::Opened(lease, client.clone(), gateway_label)
        }
        Err(err) => Ipv6PinholeCandidateResult::Rejected(clean_launcher_error_msg(err)),
    }
}

async fn try_ipv6_pinhole_candidates(
    candidates: &[(reqwest::Client, String)],
    local_v6: Ipv6Addr,
    host_port: u16,
    cancel: &CancellationToken,
    has_upnp_lease: &AtomicBool,
) -> (
    Option<Ipv6PinholeLeaseInfo>,
    Option<reqwest::Client>,
    Option<Ipv6PinholeStatus>,
) {
    let mut last_rejection: Option<String> = None;

    for (client, desc_url) in candidates {
        if cancel.is_cancelled() {
            return (None, None, None);
        }
        match try_ipv6_pinhole_on_candidate(
            client,
            desc_url,
            local_v6,
            host_port,
            cancel,
            has_upnp_lease,
        )
        .await
        {
            Ipv6PinholeCandidateResult::Opened(lease, working_client, gateway_label) => {
                if cancel.is_cancelled() {
                    return (Some(lease), Some(working_client), None);
                }
                let unique_id = lease.unique_id;
                return (
                    Some(lease),
                    Some(working_client),
                    Some(Ipv6PinholeStatus::Opened {
                        unique_id,
                        gateway_label,
                    }),
                );
            }
            Ipv6PinholeCandidateResult::FirewallDisabled => {
                return (None, None, Some(Ipv6PinholeStatus::FirewallDisabled));
            }
            Ipv6PinholeCandidateResult::Rejected(reason) => {
                last_rejection = Some(reason);
            }
            Ipv6PinholeCandidateResult::Unsupported => {}
            Ipv6PinholeCandidateResult::Cancelled => return (None, None, None),
        }
    }

    match last_rejection {
        Some(reason) => (None, None, Some(Ipv6PinholeStatus::Rejected(reason))),
        None => (None, None, Some(Ipv6PinholeStatus::Unsupported)),
    }
}

async fn setup_direct_host_ipv6_pinhole(
    local_v6: Ipv6Addr,
    host_port: u16,
    bind_ipv4: Option<Ipv4Addr>,
    cancel: &CancellationToken,
    has_upnp_lease: &AtomicBool,
) -> (
    Option<Ipv6PinholeLeaseInfo>,
    Option<reqwest::Client>,
    Option<Ipv6PinholeStatus>,
) {
    if host_port == 0 {
        return (None, None, Some(Ipv6PinholeStatus::Unsupported));
    }

    let mut candidates: Vec<(reqwest::Client, String)> = Vec::new();
    if let Ok(v6_client) = build_ipv6_upnp_client(local_v6) {
        let v6_locs = tokio::select! {
            _ = cancel.cancelled() => return (None, None, None),
            locs = probe_ipv6_ssdp_locations(local_v6) => locs,
        };
        for loc in v6_locs {
            candidates.push((v6_client.clone(), loc));
        }
    }

    if !candidates.is_empty() {
        let (lease, client, status) =
            try_ipv6_pinhole_candidates(&candidates, local_v6, host_port, cancel, has_upnp_lease)
                .await;
        if lease.is_some()
            || matches!(
                status,
                Some(Ipv6PinholeStatus::FirewallDisabled | Ipv6PinholeStatus::Rejected(_))
            )
            || cancel.is_cancelled()
        {
            return (lease, client, status);
        }
    }

    let (non_virtual_v4, _) = collect_lan_ipv4_candidates(bind_ipv4);
    if !non_virtual_v4.is_empty() {
        let v4_discovery = tokio::select! {
            _ = cancel.cancelled() => return (None, None, None),
            res = discover_igd_location(&non_virtual_v4) => res.ok(),
        };
        if let Some((_, v4_desc_url)) = v4_discovery {
            if let Ok(lan_client) = build_lan_upnp_client() {
                return try_ipv6_pinhole_candidates(
                    &[(lan_client, v4_desc_url)],
                    local_v6,
                    host_port,
                    cancel,
                    has_upnp_lease,
                )
                .await;
            }
        }
    }

    if cancel.is_cancelled() {
        (None, None, None)
    } else {
        (None, None, Some(Ipv6PinholeStatus::Unsupported))
    }
}

#[derive(Deserialize)]
struct IfconfigPortProbeResponse {
    ip: Option<String>,
    port: Option<u16>,
    reachable: Option<bool>,
}

pub(crate) fn parse_ifconfig_port_probe_response(
    body: &str,
    expected_ip: IpAddr,
    expected_port: u16,
) -> ExternalReachability {
    let parsed: IfconfigPortProbeResponse = match serde_json::from_str(body) {
        Ok(val) => val,
        Err(_) => {
            return ExternalReachability::CheckUnavailable(
                "Phản hồi kiểm tra cổng ifconfig.co không hợp lệ".to_string(),
            );
        }
    };

    let Some(ip_str) = parsed.ip.as_deref() else {
        return ExternalReachability::CheckUnavailable(
            "Phản hồi ifconfig.co thiếu trường ip".to_string(),
        );
    };
    let Ok(seen_ip) = ip_str.trim().parse::<IpAddr>() else {
        return ExternalReachability::CheckUnavailable(format!(
            "IP phản hồi từ ifconfig.co không hợp lệ: {ip_str}"
        ));
    };
    if seen_ip.to_canonical() != expected_ip.to_canonical() {
        return ExternalReachability::CheckUnavailable(format!(
            "IP phản hồi ({seen_ip}) không khớp IP cần kiểm tra ({expected_ip})"
        ));
    }

    if let Some(seen_port) = parsed.port {
        if seen_port != expected_port {
            return ExternalReachability::CheckUnavailable(format!(
                "Cổng phản hồi ({seen_port}) không khớp cổng {expected_port}"
            ));
        }
    }

    match parsed.reachable {
        Some(true) => ExternalReachability::Reachable,
        Some(false) => ExternalReachability::Blocked,
        None => ExternalReachability::CheckUnavailable(
            "Phản hồi ifconfig.co thiếu trường reachable".to_string(),
        ),
    }
}

pub(crate) fn parse_portchecker_io_response(body: &str) -> ExternalReachability {
    let trimmed = body.trim();
    if trimmed.eq_ignore_ascii_case("true") {
        ExternalReachability::Reachable
    } else if trimmed.eq_ignore_ascii_case("false") {
        ExternalReachability::Blocked
    } else {
        ExternalReachability::CheckUnavailable(
            "Phản hồi portchecker.io không hợp lệ".to_string(),
        )
    }
}

async fn probe_external_ipv6_reachability_at(
    base_url: &str,
    target_v6: Ipv6Addr,
    port: u16,
) -> ExternalReachability {
    if port == 0 {
        return ExternalReachability::CheckUnavailable("Cổng không hợp lệ (port = 0)".to_string());
    }
    let client = match build_ipv6_probe_client(target_v6) {
        Ok(c) => c,
        Err(err) => return ExternalReachability::CheckUnavailable(clean_launcher_error_msg(err)),
    };
    let url = format!("{}/port/{port}", base_url.trim_end_matches('/'));
    let resp = match client
        .get(&url)
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(5))
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            return ExternalReachability::CheckUnavailable(format!(
                "Không thể kết nối dịch vụ kiểm tra cổng IPv6: {err}"
            ));
        }
    };
    if !resp.status().is_success() {
        return ExternalReachability::CheckUnavailable(format!(
            "Dịch vụ kiểm tra cổng IPv6 trả về HTTP {}",
            resp.status()
        ));
    }
    let body = match resp.text().await {
        Ok(t) => t,
        Err(err) => {
            return ExternalReachability::CheckUnavailable(format!(
                "Lỗi đọc phản hồi kiểm tra cổng IPv6: {err}"
            ));
        }
    };
    parse_ifconfig_port_probe_response(&body, IpAddr::V6(target_v6), port)
}

pub async fn probe_external_ipv6_reachability(
    target_v6: Ipv6Addr,
    port: u16,
) -> ExternalReachability {
    probe_external_ipv6_reachability_at(IFCONFIG_PORT_PROBE_BASE_URL, target_v6, port).await
}

async fn probe_external_ipv4_reachability_at(
    ifconfig_base_url: &str,
    portchecker_base_url: &str,
    expected_v4: Ipv4Addr,
    port: u16,
) -> ExternalReachability {
    if port == 0 {
        return ExternalReachability::CheckUnavailable("Cổng không hợp lệ (port = 0)".to_string());
    }
    let client = match build_ipv4_probe_client() {
        Ok(c) => c,
        Err(err) => return ExternalReachability::CheckUnavailable(clean_launcher_error_msg(err)),
    };

    let ifconfig_url = format!("{}/port/{port}", ifconfig_base_url.trim_end_matches('/'));
    let primary_err = match client
        .get(&ifconfig_url)
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.text().await {
            Ok(body) => {
                let parsed = parse_ifconfig_port_probe_response(&body, IpAddr::V4(expected_v4), port);
                match parsed {
                    ExternalReachability::Reachable | ExternalReachability::Blocked => {
                        return parsed;
                    }
                    ExternalReachability::CheckUnavailable(reason) => reason,
                    other => return other,
                }
            }
            Err(err) => format!("Lỗi đọc phản hồi ifconfig.co: {err}"),
        },
        Ok(resp) => format!("ifconfig.co trả về HTTP {}", resp.status()),
        Err(err) => format!("Không thể kết nối ifconfig.co: {err}"),
    };

    let portchecker_url = format!(
        "{}/api/{expected_v4}/{port}",
        portchecker_base_url.trim_end_matches('/')
    );
    match client
        .get(&portchecker_url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.text().await {
            Ok(body) => parse_portchecker_io_response(&body),
            Err(err) => ExternalReachability::CheckUnavailable(format!(
                "Lỗi đọc phản hồi portchecker.io: {err}"
            )),
        },
        Ok(resp) => ExternalReachability::CheckUnavailable(format!(
            "portchecker.io trả về HTTP {} ({primary_err})",
            resp.status(),
        )),
        Err(err) => ExternalReachability::CheckUnavailable(format!(
            "Không thể kiểm tra cổng từ bên ngoài: {err}"
        )),
    }
}

pub async fn probe_external_ipv4_reachability(
    expected_v4: Ipv4Addr,
    port: u16,
) -> ExternalReachability {
    probe_external_ipv4_reachability_at(
        IFCONFIG_PORT_PROBE_BASE_URL,
        PORTCHECKER_PROBE_BASE_URL,
        expected_v4,
        port,
    )
    .await
}

fn bind_tcp_v4_listener(bind_ip: Ipv4Addr, port: u16) -> std::io::Result<TcpListener> {
    let sock = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    #[cfg(not(windows))]
    let _ = sock.set_reuse_address(true);
    sock.bind(&SocketAddr::V4(SocketAddrV4::new(bind_ip, port)).into())?;
    sock.listen(128)?;
    sock.set_nonblocking(true)?;
    TcpListener::from_std(sock.into())
}

fn bind_tcp_v6_only_listener(port: u16) -> std::io::Result<TcpListener> {
    let sock = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    sock.set_only_v6(true)?;
    #[cfg(not(windows))]
    let _ = sock.set_reuse_address(true);
    sock.bind(&SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0)).into())?;
    sock.listen(128)?;
    sock.set_nonblocking(true)?;
    TcpListener::from_std(sock.into())
}

async fn is_tcp_addr_reachable(addr: SocketAddr, wait: Duration) -> bool {
    timeout(wait, TcpStream::connect(addr))
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_local_tcp_addr_listening_os(addr: SocketAddr) -> Option<bool> {
    let target_port = addr.port();
    if target_port == 0 {
        return Some(false);
    }

    let ulaf: u32 = match addr {
        SocketAddr::V4(_) => 2,
        SocketAddr::V6(_) => 23,
    };

    let mut dw_size: u32 = 4096;
    let mut buf: Vec<u32> = Vec::new();
    let mut succeeded = false;
    for _ in 0..4 {
        let word_len = (dw_size as usize)
            .div_ceil(std::mem::size_of::<u32>())
            .max(32);
        buf.resize(word_len, 0);
        dw_size = u32::try_from(buf.len().saturating_mul(std::mem::size_of::<u32>()))
            .unwrap_or(u32::MAX);

        let rc = unsafe {
            GetExtendedTcpTable(
                Some(buf.as_mut_ptr().cast()),
                &mut dw_size,
                false,
                ulaf,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if rc == NO_ERROR.0 {
            succeeded = true;
            break;
        }
        if rc == ERROR_INSUFFICIENT_BUFFER.0 {
            dw_size = dw_size.saturating_add(256);
            continue;
        }
        return None;
    }
    if !succeeded {
        return None;
    }

    let buf_bytes = buf.len().saturating_mul(std::mem::size_of::<u32>());
    match addr {
        SocketAddr::V4(v4_target) => {
            let rows_offset = std::mem::offset_of!(MIB_TCPTABLE_OWNER_PID, table);
            if buf_bytes < rows_offset.max(std::mem::size_of::<u32>()) {
                return None;
            }
            let table_ptr = buf.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
            let num_entries = unsafe { (*table_ptr).dwNumEntries } as usize;
            let max_entries =
                (buf_bytes - rows_offset) / std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
            if num_entries > max_entries {
                return None;
            }
            let rows_ptr =
                unsafe { std::ptr::addr_of!((*table_ptr).table).cast::<MIB_TCPROW_OWNER_PID>() };
            let rows = unsafe { std::slice::from_raw_parts(rows_ptr, num_entries) };
            let want_ip = *v4_target.ip();
            for row in rows {
                let port = u16::from_be((row.dwLocalPort & 0xFFFF) as u16);
                let row_ip = Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes());
                if port == target_port
                    && (want_ip.is_unspecified() || row_ip.is_unspecified() || row_ip == want_ip)
                {
                    return Some(true);
                }
            }
            Some(false)
        }
        SocketAddr::V6(v6_target) => {
            let rows_offset = std::mem::offset_of!(MIB_TCP6TABLE_OWNER_PID, table);
            if buf_bytes < rows_offset.max(std::mem::size_of::<u32>()) {
                return None;
            }
            let table_ptr = buf.as_ptr().cast::<MIB_TCP6TABLE_OWNER_PID>();
            let num_entries = unsafe { (*table_ptr).dwNumEntries } as usize;
            let max_entries =
                (buf_bytes - rows_offset) / std::mem::size_of::<MIB_TCP6ROW_OWNER_PID>();
            if num_entries > max_entries {
                return None;
            }
            let rows_ptr =
                unsafe { std::ptr::addr_of!((*table_ptr).table).cast::<MIB_TCP6ROW_OWNER_PID>() };
            let rows = unsafe { std::slice::from_raw_parts(rows_ptr, num_entries) };
            let want_ip = *v6_target.ip();
            for row in rows {
                let port = u16::from_be((row.dwLocalPort & 0xFFFF) as u16);
                let row_ip = Ipv6Addr::from(row.ucLocalAddr);
                if port == target_port
                    && (want_ip.is_unspecified() || row_ip.is_unspecified() || row_ip == want_ip)
                {
                    return Some(true);
                }
            }
            Some(false)
        }
    }
}

async fn is_local_tcp_socket_listening(addr: SocketAddr, fallback_wait: Duration) -> bool {
    #[cfg(windows)]
    if let Some(listening) = is_local_tcp_addr_listening_os(addr) {
        return listening;
    }
    is_tcp_addr_reachable(addr, fallback_wait).await
}

pub(crate) struct ClientGuard {
    pub(crate) counter: Arc<AtomicUsize>,
    pub(crate) status: Arc<RwLock<DirectHostStatus>>,
    pub(crate) wake_ui: crate::server_manager::egui_repaint::RepaintHandle,
    pub(crate) outbound_port: Option<u16>,
    pub(crate) bridge_peers: Arc<RwLock<BridgePeerMap>>,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        if let Some(port) = self.outbound_port {
            self.bridge_peers
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&port);
        }
        let next = self
            .counter
            .fetch_sub(1, Ordering::SeqCst)
            .saturating_sub(1);
        update_active_client_count(&self.status, &self.wake_ui, next);
    }
}

pub(crate) fn update_active_client_count(
    status: &Arc<RwLock<DirectHostStatus>>,
    wake_ui: &crate::server_manager::egui_repaint::RepaintHandle,
    count: usize,
) {
    let mut changed = false;
    {
        let mut guard = status.write().unwrap_or_else(|e| e.into_inner());
        if let DirectHostStatus::Running(ref mut info) = *guard {
            if info.active_clients != count {
                info.active_clients = count;
                changed = true;
            }
        }
    }
    if changed {
        wake_ui.wake();
    }
}

#[derive(Clone)]
struct TcpBridgeContext {
    local_target_port: u16,
    cancel: CancellationToken,
    active_clients: Arc<AtomicUsize>,
    status: Arc<RwLock<DirectHostStatus>>,
    wake_ui: crate::server_manager::egui_repaint::RepaintHandle,
    banned_ips: Arc<RwLock<HashSet<IpAddr>>>,
    bridge_peers: Arc<RwLock<BridgePeerMap>>,
}

fn is_peer_banned(banned_ips: &RwLock<HashSet<IpAddr>>, peer_ip: IpAddr) -> bool {
    is_bannable_peer_ip(peer_ip)
        && banned_ips
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&peer_ip)
}

pub(crate) fn configure_bridge_tcp_stream(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
    let keepalive = TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(15));
    let _ = SockRef::from(stream).set_tcp_keepalive(&keepalive);
}

fn spawn_tcp_bridge_worker(listener: TcpListener, ctx: TcpBridgeContext) {
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                _ = ctx.cancel.cancelled() => break,
                res = listener.accept() => res,
            };
            let Ok((mut inbound, peer_addr)) = accepted else {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            let peer_ip = peer_addr.ip().to_canonical();
            if is_peer_banned(&ctx.banned_ips, peer_ip) {
                drop(inbound);
                continue;
            }
            configure_bridge_tcp_stream(&inbound);
            let conn_ctx = ctx.clone();

            tokio::spawn(async move {
                let target_v4 = SocketAddr::V4(SocketAddrV4::new(
                    Ipv4Addr::LOCALHOST,
                    conn_ctx.local_target_port,
                ));
                let Ok(Ok(mut outbound)) =
                    timeout(Duration::from_secs(3), TcpStream::connect(target_v4)).await
                else {
                    return;
                };
                if is_peer_banned(&conn_ctx.banned_ips, peer_ip) {
                    return;
                }
                configure_bridge_tcp_stream(&outbound);

                let outbound_port = outbound.local_addr().ok().map(|a| a.port());
                let conn_cancel = CancellationToken::new();
                if let Some(port) = outbound_port {
                    conn_ctx
                        .bridge_peers
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(port, (BridgedPeerTarget::Ip(peer_ip), conn_cancel.clone()));
                }

                let now_count = conn_ctx.active_clients.fetch_add(1, Ordering::SeqCst) + 1;
                let _guard = ClientGuard {
                    counter: Arc::clone(&conn_ctx.active_clients),
                    status: Arc::clone(&conn_ctx.status),
                    wake_ui: conn_ctx.wake_ui.clone(),
                    outbound_port,
                    bridge_peers: Arc::clone(&conn_ctx.bridge_peers),
                };

                if is_peer_banned(&conn_ctx.banned_ips, peer_ip) {
                    return;
                }
                update_active_client_count(&conn_ctx.status, &conn_ctx.wake_ui, now_count);

                tokio::select! {
                    _ = conn_ctx.cancel.cancelled() => {}
                    _ = conn_cancel.cancelled() => {}
                    _ = copy_bidirectional(&mut inbound, &mut outbound) => {}
                }
            });
        }
    });
}

pub fn clean_launcher_error_msg(err: LauncherError) -> String {
    match err {
        LauncherError::Network(msg)
        | LauncherError::NotFound(msg)
        | LauncherError::InvalidInput(msg)
        | LauncherError::Process(msg)
        | LauncherError::InvalidFormat(msg) => msg,
        other => other.to_string(),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PartitionedHostEndpoints {
    pub lan: Vec<(String, String)>,
    pub vpn: Vec<(String, String)>,
}

pub(crate) fn partition_host_endpoints(
    interfaces: &[LocalInterfaceInfo],
    bind_ipv4: Option<Ipv4Addr>,
    host_port: u16,
) -> PartitionedHostEndpoints {
    let mut lan = Vec::new();
    let mut vpn = Vec::new();
    if let Some(pinned) = bind_ipv4 {
        let matched = interfaces.iter().find(|i| i.ipv4 == pinned);
        let iface_name = matched
            .map(|i| i.name.clone())
            .unwrap_or_else(|| "LAN".to_string());
        let entry = (iface_name, format!("{pinned}:{host_port}"));
        if matched.is_some_and(|i| i.is_vpn) {
            vpn.push(entry);
        } else {
            lan.push(entry);
        }
        return PartitionedHostEndpoints { lan, vpn };
    }

    let mut seen_ips = Vec::new();
    for iface in interfaces {
        if !iface.is_hypervisor && !seen_ips.contains(&iface.ipv4) {
            seen_ips.push(iface.ipv4);
            let entry = (iface.name.clone(), format!("{}:{host_port}", iface.ipv4));
            if iface.is_vpn {
                vpn.push(entry);
            } else {
                lan.push(entry);
            }
        }
    }
    if lan.is_empty() && vpn.is_empty() {
        lan.push(("Local".to_string(), format!("127.0.0.1:{host_port}")));
    }
    PartitionedHostEndpoints { lan, vpn }
}

pub fn start_direct_port_host(
    rt: &tokio::runtime::Handle,
    config: DirectHostConfig,
    wake_ui: crate::server_manager::egui_repaint::RepaintHandle,
) -> DirectPortHostHandle {
    let cancel = CancellationToken::new();
    let banned_ips = Arc::new(RwLock::new(normalize_ban_set(
        config.initial_banned_ips.iter().copied(),
    )));
    let banned_p2p_peers = Arc::new(RwLock::new(normalize_p2p_ban_set(
        config.initial_banned_p2p_peers.iter().cloned(),
    )));
    let active_p2p_conns = Arc::new(Mutex::new(Vec::new()));
    let bridge_peers = Arc::new(RwLock::new(HashMap::new()));
    let has_upnp_lease = Arc::new(AtomicBool::new(false));
    let has_p2p_endpoint = Arc::new(AtomicBool::new(false));
    let rechecking = Arc::new(AtomicBool::new(false));
    if config.local_target_port == 0 || config.host_port == 0 {
        let status = Arc::new(RwLock::new(DirectHostStatus::Error(
            "Cổng server hoặc cổng host không hợp lệ (port = 0)".to_string(),
        )));
        wake_ui.wake();
        return DirectPortHostHandle {
            cancel,
            status,
            banned_ips,
            banned_p2p_peers,
            active_p2p_conns,
            bridge_peers,
            has_upnp_lease,
            has_p2p_endpoint,
            cleanup_done: Arc::new((Mutex::new(true), Condvar::new())),
            rechecking,
        };
    }

    let cleanup_done = Arc::new((Mutex::new(false), Condvar::new()));
    let status = Arc::new(RwLock::new(DirectHostStatus::Starting {
        local_port: config.local_target_port,
        host_port: config.host_port,
    }));
    let handle = DirectPortHostHandle {
        cancel: cancel.clone(),
        status: Arc::clone(&status),
        banned_ips: Arc::clone(&banned_ips),
        banned_p2p_peers: Arc::clone(&banned_p2p_peers),
        active_p2p_conns: Arc::clone(&active_p2p_conns),
        bridge_peers: Arc::clone(&bridge_peers),
        has_upnp_lease: Arc::clone(&has_upnp_lease),
        has_p2p_endpoint: Arc::clone(&has_p2p_endpoint),
        cleanup_done: Arc::clone(&cleanup_done),
        rechecking,
    };

    rt.spawn(async move {
        struct CleanupDoneGuard(Arc<(Mutex<bool>, Condvar)>);
        impl Drop for CleanupDoneGuard {
            fn drop(&mut self) {
                let (lock, cvar) = &*self.0;
                let mut done = lock.lock().unwrap_or_else(|e| e.into_inner());
                *done = true;
                cvar.notify_all();
            }
        }
        let _cleanup_guard = CleanupDoneGuard(cleanup_done);
        let local_v4 = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::LOCALHOST,
            config.local_target_port,
        ));
        if !is_local_tcp_socket_listening(local_v4, Duration::from_millis(650)).await {
            {
                let mut guard = status.write().unwrap_or_else(|e| e.into_inner());
                *guard = DirectHostStatus::Error(format!(
                    "Chưa có Server nào đang lắng nghe tại 127.0.0.1:{} (hãy khởi chạy Server trước).",
                    config.local_target_port
                ));
            }
            wake_ui.wake();
            return;
        }

        let ((primary_lan_ip, interfaces), global_ipv6) = tokio::task::spawn_blocking(|| {
            (collect_local_interfaces(), detect_primary_global_ipv6())
        })
        .await
        .unwrap_or_else(|_| ((None, Vec::new()), None));

        let bind_ip = config.bind_ipv4.unwrap_or(Ipv4Addr::UNSPECIFIED);
        let active_clients = Arc::new(AtomicUsize::new(0));
        let bridge_ctx = TcpBridgeContext {
            local_target_port: config.local_target_port,
            cancel: cancel.clone(),
            active_clients: Arc::clone(&active_clients),
            status: Arc::clone(&status),
            wake_ui: wake_ui.clone(),
            banned_ips: Arc::clone(&banned_ips),
            bridge_peers: Arc::clone(&bridge_peers),
        };
        let mut bridging_tcp = false;
        let mut ipv6_ready = false;

        if config.host_port != config.local_target_port {
            let v4_listener = match bind_tcp_v4_listener(bind_ip, config.host_port) {
                Ok(listener) => listener,
                Err(e) => {
                    {
                        let mut guard = status.write().unwrap_or_else(|e| e.into_inner());
                        *guard = DirectHostStatus::Error(format!(
                            "Không thể mở cổng {} trên mạng hiện tại (cổng có thể đang bận): {e}",
                            config.host_port
                        ));
                    }
                    wake_ui.wake();
                    return;
                }
            };
            bridging_tcp = true;
            spawn_tcp_bridge_worker(v4_listener, bridge_ctx.clone());

            if config.bind_ipv4.is_none() {
                if let Ok(v6_listener) = bind_tcp_v6_only_listener(config.host_port) {
                    ipv6_ready = true;
                    spawn_tcp_bridge_worker(v6_listener, bridge_ctx.clone());
                }
            }
        } else {
            let check_ip = config.bind_ipv4.or(primary_lan_ip);
            let already_listening_v4 = match check_ip {
                Some(ip) => {
                    is_local_tcp_socket_listening(
                        SocketAddr::V4(SocketAddrV4::new(ip, config.host_port)),
                        Duration::from_millis(400),
                    )
                    .await
                }
                None => false,
            };

            if !already_listening_v4 {
                if let Ok(v4_listener) = bind_tcp_v4_listener(bind_ip, config.host_port) {
                    bridging_tcp = true;
                    spawn_tcp_bridge_worker(v4_listener, bridge_ctx.clone());
                } else if check_ip.is_some() {
                    let candidate_ips: Vec<Ipv4Addr> = match config.bind_ipv4 {
                        Some(ip) => vec![ip],
                        None => interfaces
                            .iter()
                            .filter(|i| !i.is_hypervisor || interfaces.len() == 1)
                            .map(|i| i.ipv4)
                            .collect(),
                    };
                    let mut bound_any = false;
                    for nic_ip in candidate_ips {
                        if let Ok(nic_listener) = bind_tcp_v4_listener(nic_ip, config.host_port) {
                            bound_any = true;
                            bridging_tcp = true;
                            spawn_tcp_bridge_worker(nic_listener, bridge_ctx.clone());
                        }
                    }
                    if !bound_any {
                        {
                            let mut guard = status.write().unwrap_or_else(|e| e.into_inner());
                            *guard = DirectHostStatus::Error(format!(
                                "Cổng {} trên mạng hiện tại đang bận hoặc bị chặn.",
                                config.host_port
                            ));
                        }
                        wake_ui.wake();
                        return;
                    }
                }
            }

            if config.bind_ipv4.is_none() {
                match bind_tcp_v6_only_listener(config.host_port) {
                    Ok(v6_listener) => {
                        bridging_tcp = true;
                        ipv6_ready = true;
                        spawn_tcp_bridge_worker(v6_listener, bridge_ctx.clone());
                    }
                    Err(_) => {
                        let check_v6 = global_ipv6.unwrap_or(Ipv6Addr::LOCALHOST);
                        let v6_sock = SocketAddr::V6(SocketAddrV6::new(
                            check_v6,
                            config.host_port,
                            0,
                            0,
                        ));
                        ipv6_ready =
                            is_local_tcp_socket_listening(v6_sock, Duration::from_millis(350))
                                .await;
                    }
                }
            }
        }

        let PartitionedHostEndpoints {
            lan: lan_endpoints,
            vpn: vpn_endpoints,
        } = partition_host_endpoints(&interfaces, config.bind_ipv4, config.host_port);

        let ipv6_endpoint = ipv6_ready
            .then(|| global_ipv6.map(|v6| format!("[{v6}]:{}", config.host_port)))
            .flatten();
        let has_v6_target = ipv6_ready && global_ipv6.is_some();

        let initial_upnp = if config.try_upnp {
            UpnpPortStatus::Mapping
        } else {
            UpnpPortStatus::Disabled
        };
        let initial_v6_pinhole = if config.try_upnp && has_v6_target {
            Ipv6PinholeStatus::Opening
        } else {
            Ipv6PinholeStatus::Disabled
        };
        let initial_v6_reachability = if config.verify_wan_reachability && has_v6_target {
            ExternalReachability::Checking
        } else {
            ExternalReachability::NotChecked
        };

        let initial_p2p_status = if config.enable_p2p {
            P2pHostStatus::Starting
        } else {
            P2pHostStatus::Disabled
        };

        let running_info = DirectHostActiveInfo {
            local_port: config.local_target_port,
            host_port: config.host_port,
            lan_endpoints,
            vpn_endpoints,
            ipv6_endpoint,
            ipv6_pinhole_status: initial_v6_pinhole,
            ipv6_reachability: initial_v6_reachability,
            upnp_status: initial_upnp,
            ipv4_reachability: ExternalReachability::NotChecked,
            bridging_tcp,
            active_clients: active_clients.load(Ordering::SeqCst),
            p2p_status: initial_p2p_status,
        };

        {
            let mut guard = status.write().unwrap_or_else(|e| e.into_inner());
            *guard = DirectHostStatus::Running(running_info);
        }
        wake_ui.wake();

        let p2p_join_handle = if config.enable_p2p && !cancel.is_cancelled() {
            has_p2p_endpoint.store(true, Ordering::SeqCst);
            let secret_key =
                load_or_create_p2p_secret_key(config.p2p_secret_key_path.as_deref());
            Some(spawn_p2p_host_worker(
                config.local_target_port,
                secret_key,
                cancel.clone(),
                Arc::clone(&active_clients),
                Arc::clone(&status),
                wake_ui.clone(),
                Arc::clone(&banned_p2p_peers),
                Arc::clone(&active_p2p_conns),
                Arc::clone(&bridge_peers),
            ))
        } else {
            None
        };

        let v4_upnp_fut = async {
            if config.try_upnp || config.cleanup_local_upnp_on_start {
                let (lease, client, upnp_status) = setup_direct_host_upnp(
                    config.local_target_port,
                    config.host_port,
                    config.bind_ipv4,
                    config.try_upnp,
                    config.cleanup_local_upnp_on_start,
                    &cancel,
                    &has_upnp_lease,
                )
                .await;

                let mapped_target = upnp_status.as_ref().and_then(|s| match s {
                    UpnpPortStatus::Mapped {
                        external_ip,
                        external_port,
                        ..
                    } => Some((*external_ip, *external_port)),
                    _ => None,
                });

                if let Some(resolved_upnp) = upnp_status {
                    let mut updated = false;
                    {
                        let mut guard = status.write().unwrap_or_else(|e| e.into_inner());
                        if let DirectHostStatus::Running(ref mut info) = *guard {
                            if config.verify_wan_reachability && mapped_target.is_some() {
                                info.ipv4_reachability = ExternalReachability::Checking;
                            }
                            info.upnp_status = resolved_upnp;
                            updated = true;
                        }
                    }
                    if updated {
                        wake_ui.wake();
                    }
                }
                (lease, client, mapped_target)
            } else {
                (None, None, None)
            }
        };

        let v6_pinhole_fut = async {
            if config.try_upnp {
                if let Some(v6_ip) = has_v6_target.then_some(global_ipv6).flatten() {
                    let (lease, client, pinhole_status) = setup_direct_host_ipv6_pinhole(
                        v6_ip,
                        config.host_port,
                        config.bind_ipv4,
                        &cancel,
                        &has_upnp_lease,
                    )
                    .await;

                    let opened = matches!(pinhole_status, Some(Ipv6PinholeStatus::Opened { .. }));
                    if let Some(resolved_pinhole) = pinhole_status {
                        let mut updated = false;
                        {
                            let mut guard = status.write().unwrap_or_else(|e| e.into_inner());
                            if let DirectHostStatus::Running(ref mut info) = *guard {
                                info.ipv6_pinhole_status = resolved_pinhole;
                                updated = true;
                            }
                        }
                        if updated {
                            wake_ui.wake();
                        }
                    }
                    (lease, client, opened)
                } else {
                    (None, None, false)
                }
            } else {
                (None, None, false)
            }
        };

        let (
            (active_lease, upnp_client, mapped_v4_target),
            (mut active_v6_lease, v6_client, v6_opened),
        ) = tokio::join!(v4_upnp_fut, v6_pinhole_fut);

        has_upnp_lease.store(
            active_lease.is_some() || active_v6_lease.is_some(),
            Ordering::SeqCst,
        );

        if !cancel.is_cancelled() && config.verify_wan_reachability {
            let v6_probe_target = has_v6_target
                .then_some(global_ipv6)
                .flatten()
                .map(|ip| (ip, config.host_port));
            if mapped_v4_target.is_some() || v6_probe_target.is_some() {
                if v6_opened {
                    tokio::select! {
                        _ = cancel.cancelled() => {}
                        _ = tokio::time::sleep(Duration::from_millis(150)) => {}
                    }
                }
                if !cancel.is_cancelled() {
                    let probe_res = tokio::select! {
                        _ = cancel.cancelled() => None,
                        pair = async {
                            let v4_fut = async {
                                match mapped_v4_target {
                                    Some((ip, port)) => {
                                        Some(probe_external_ipv4_reachability(ip, port).await)
                                    }
                                    None => None,
                                }
                            };
                            let v6_fut = async {
                                match v6_probe_target {
                                    Some((ip, port)) => {
                                        Some(probe_external_ipv6_reachability(ip, port).await)
                                    }
                                    None => None,
                                }
                            };
                            tokio::join!(v4_fut, v6_fut)
                        } => Some(pair),
                    };
                    if let Some((v4_reach, v6_reach)) = probe_res {
                        if !cancel.is_cancelled() {
                            let mut updated = false;
                            {
                                let mut guard = status.write().unwrap_or_else(|e| e.into_inner());
                                if let DirectHostStatus::Running(ref mut info) = *guard {
                                    if let Some(r) = v4_reach {
                                        info.ipv4_reachability = r;
                                        updated = true;
                                    }
                                    if let Some(r) = v6_reach {
                                        info.ipv6_reachability = r;
                                        updated = true;
                                    }
                                }
                            }
                            if updated {
                                wake_ui.wake();
                            }
                        }
                    }
                }
            }
        }

        while !cancel.is_cancelled() {
            let has_v4 = active_lease.is_some() && upnp_client.is_some();
            let has_v6 = active_v6_lease.is_some() && v6_client.is_some();
            if has_v4 || has_v6 {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(1500)) => {
                        let v4_renew = async {
                            if let (Some(lease), Some(client)) =
                                (active_lease.as_ref(), upnp_client.as_ref())
                            {
                                let _ = send_upnp_add_port_mapping(
                                    client,
                                    &lease.control_url,
                                    &lease.service_type,
                                    lease.port,
                                    lease.port,
                                    lease.local_ip,
                                    lease.lease_seconds,
                                )
                                .await;
                            }
                        };
                        let v6_renew = async {
                            if let (Some(v6_lease), Some(client)) =
                                (active_v6_lease.as_mut(), v6_client.as_ref())
                            {
                                if send_upnp_update_ipv6_pinhole(
                                    client,
                                    &v6_lease.control_url,
                                    &v6_lease.service_type,
                                    v6_lease.unique_id,
                                    v6_lease.lease_seconds,
                                )
                                .await
                                .is_err()
                                {
                                    if let Ok(new_uid) = send_upnp_add_ipv6_pinhole(
                                        client,
                                        &v6_lease.control_url,
                                        &v6_lease.service_type,
                                        v6_lease.internal_client,
                                        v6_lease.internal_port,
                                        v6_lease.lease_seconds,
                                    )
                                    .await
                                    {
                                        if v6_lease.unique_id != new_uid {
                                            v6_lease.unique_id = new_uid;
                                            let mut updated = false;
                                            {
                                                let mut guard =
                                                    status.write().unwrap_or_else(|e| e.into_inner());
                                                if let DirectHostStatus::Running(ref mut info) =
                                                    *guard
                                                {
                                                    if let Ipv6PinholeStatus::Opened {
                                                        ref mut unique_id,
                                                        ..
                                                    } = info.ipv6_pinhole_status
                                                    {
                                                        *unique_id = new_uid;
                                                        updated = true;
                                                    }
                                                }
                                            }
                                            if updated {
                                                wake_ui.wake();
                                            }
                                        }
                                    }
                                }
                            }
                        };
                        tokio::join!(v4_renew, v6_renew);
                    }
                }
            } else {
                cancel.cancelled().await;
                break;
            }
        }

        tokio::join!(
            async {
                if let Some(lease) = active_lease {
                    let _ = close_upnp_port(&lease).await;
                }
            },
            async {
                if let Some(v6_lease) = active_v6_lease {
                    let _ = close_ipv6_pinhole(&v6_lease).await;
                }
            },
            async {
                if let Some(p2p_task) = p2p_join_handle {
                    let _ = p2p_task.await;
                }
            }
        );

        active_p2p_conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        bridge_peers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        {
            let mut guard = status.write().unwrap_or_else(|e| e.into_inner());
            *guard = DirectHostStatus::Stopped;
        }
        wake_ui.wake();
    });

    handle
}

pub async fn probe_local_endpoint_listening(host: &str, port: u16) -> Result<u128, String> {
    let trimmed = host.trim();
    let clean_host = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .map(str::trim)
        .unwrap_or(trimmed);
    if clean_host.is_empty() || port == 0 {
        return Err("Host hoặc port không hợp lệ".to_string());
    }

    let started = Instant::now();
    let listening = if clean_host.eq_ignore_ascii_case("localhost") {
        let v4 = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
        let v6 = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, port, 0, 0));
        is_local_tcp_socket_listening(v4, Duration::from_millis(400)).await
            || is_local_tcp_socket_listening(v6, Duration::from_millis(400)).await
    } else if let Ok(ip) = clean_host.parse::<IpAddr>() {
        is_local_tcp_socket_listening(SocketAddr::new(ip, port), Duration::from_millis(400)).await
    } else {
        return probe_tcp_endpoint(clean_host, port).await;
    };

    if listening {
        Ok(started.elapsed().as_millis())
    } else {
        Err("Cổng chưa mở hoặc chưa có tiến trình lắng nghe".to_string())
    }
}

pub async fn probe_tcp_endpoint(host: &str, port: u16) -> Result<u128, String> {
    let trimmed = host.trim();
    let clean_host = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .map(str::trim)
        .unwrap_or(trimmed);
    if clean_host.is_empty() || port == 0 {
        return Err("Host hoặc port không hợp lệ".to_string());
    }
    let started = Instant::now();
    match timeout(Duration::from_secs(4), TcpStream::connect((clean_host, port))).await {
        Ok(Ok(_stream)) => Ok(started.elapsed().as_millis()),
        Ok(Err(e)) => Err(format!("Kết nối thất bại: {e}")),
        Err(_) => Err("Hết thời gian chờ (4s) — cổng chưa phản hồi".to_string()),
    }
}

pub async fn resolve_ipv6_bridge_target(host: &str, port: u16) -> Option<SocketAddrV6> {
    let trimmed = host.trim();
    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed)
        .trim();
    if unbracketed.is_empty() {
        return None;
    }

    let target_port = if port > 0 { port } else { 7777 };

    if let Some((addr_part, scope_part)) = unbracketed.split_once('%') {
        let addr_clean = addr_part.trim();
        let scope_clean = scope_part.trim();
        if scope_clean.is_empty() || scope_clean.contains(['%', '[', ']', ':']) {
            return None;
        }
        let v6 = addr_clean.parse::<Ipv6Addr>().ok()?;
        if let Ok(scope_id) = scope_clean.parse::<u32>() {
            return Some(SocketAddrV6::new(v6, target_port, 0, scope_id));
        }
        let Ok(Ok(addrs)) = timeout(
            Duration::from_millis(1500),
            tokio::net::lookup_host((unbracketed, target_port)),
        )
        .await
        else {
            return None;
        };
        return addrs.into_iter().find_map(|addr| match addr {
            SocketAddr::V6(v6_sock) => Some(SocketAddrV6::new(
                *v6_sock.ip(),
                target_port,
                v6_sock.flowinfo(),
                v6_sock.scope_id(),
            )),
            SocketAddr::V4(_) => None,
        });
    }

    if let Ok(v6) = unbracketed.parse::<Ipv6Addr>() {
        return Some(SocketAddrV6::new(v6, target_port, 0, 0));
    }
    if unbracketed.parse::<Ipv4Addr>().is_ok() || unbracketed.eq_ignore_ascii_case("localhost") {
        return None;
    }

    let Ok(Ok(addrs)) = timeout(
        Duration::from_millis(1500),
        tokio::net::lookup_host((unbracketed, target_port)),
    )
    .await
    else {
        return None;
    };

    let list: Vec<SocketAddr> = addrs.collect();
    if list.iter().any(|a| a.is_ipv4()) {
        return None;
    }
    list.iter().find_map(|addr| match addr {
        SocketAddr::V6(v6) => Some(*v6),
        SocketAddr::V4(_) => None,
    })
}

pub async fn start_ipv4_to_ipv6_client_bridge(
    target: SocketAddrV6,
) -> LauncherResult<LocalClientBridgeHandle> {
    if target.port() == 0 {
        return Err(LauncherError::InvalidInput(
            "Cổng đích IPv6 không hợp lệ (port = 0)".to_string(),
        ));
    }

    let target_ip = *target.ip();
    let remote = SocketAddr::V6(target);
    if (target_ip.is_loopback()
        || (target.scope_id() == 0 && detect_primary_global_ipv6() == Some(target_ip)))
        && !is_local_tcp_socket_listening(remote, Duration::from_millis(400)).await
    {
        return Err(LauncherError::Network(format!(
            "Không kết nối được tới {target} — cổng IPv6 trên máy này chưa mở hoặc Server chưa chạy."
        )));
    }

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| {
            LauncherError::Network(format!(
                "Không mở được cầu nối cục bộ 127.0.0.1 cho IPv6: {e}"
            ))
        })?;
    let local_port = listener
        .local_addr()
        .map_err(|e| {
            LauncherError::Network(format!(
                "Không lấy được cổng cầu nối cục bộ cho IPv6: {e}"
            ))
        })?
        .port();

    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                _ = task_cancel.cancelled() => break,
                res = listener.accept() => res,
            };
            let Ok((mut inbound, _)) = accepted else {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            configure_bridge_tcp_stream(&inbound);
            let child_cancel = task_cancel.clone();
            tokio::spawn(async move {
                let Ok(Ok(mut outbound)) =
                    timeout(Duration::from_millis(2500), TcpStream::connect(remote)).await
                else {
                    return;
                };
                configure_bridge_tcp_stream(&outbound);
                tokio::select! {
                    _ = child_cancel.cancelled() => {}
                    _ = copy_bidirectional(&mut inbound, &mut outbound) => {}
                }
            });
        }
    });

    Ok(LocalClientBridgeHandle { cancel, local_port })
}

pub fn is_local_probe_host(host: &str, snap: &NetworkSnapshot) -> bool {
    let trimmed = host.trim();
    let clean_host = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .map(str::trim)
        .unwrap_or(trimmed);
    if clean_host.is_empty() {
        return false;
    }
    if clean_host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match clean_host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            v4.is_loopback()
                || snap.primary_lan_ip == Some(v4)
                || snap.interfaces.iter().any(|iface| iface.ipv4 == v4)
        }
        Ok(IpAddr::V6(v6)) => v6.is_loopback() || snap.global_ipv6 == Some(v6),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vpn_and_virtual_adapter_detection_avoids_false_positives() {
        assert!(!is_vpn_adapter_name("Ethernet"));
        assert!(!is_vpn_adapter_name("Realtek PCIe GbE Family Controller"));
        assert!(!is_vpn_adapter_name("Wi-Fi"));
        assert!(!is_hypervisor_adapter_name("Ethernet"));
        assert!(!is_hypervisor_adapter_name("Wi-Fi"));
        assert!(!is_virtual_adapter_name("Ethernet"));
        assert!(!is_virtual_adapter_name("Wi-Fi"));

        for vpn in [
            "CloudflareWARP",
            "Cloudflare WARP",
            "WireGuard Tunnel",
            "Tailscale",
            "ZeroTier One [8056c2e21c000001]",
            "Radmin VPN",
            "Hamachi",
            "OpenVPN Data Channel Offload",
            "Wintun Userspace Tunnel",
            "TAP-Windows Adapter V9",
            "utun3",
            "tun0",
            "tap1",
            "ProtonVPN TUN",
        ] {
            assert!(is_vpn_adapter_name(vpn), "expected VPN match for {vpn}");
            assert!(
                !is_hypervisor_adapter_name(vpn),
                "gaming/tunnel VPN must not be classified as hypervisor: {vpn}"
            );
            assert!(
                is_virtual_adapter_name(vpn),
                "expected virtual match for {vpn}"
            );
        }

        for virt_only in [
            "vEthernet (WSL)",
            "VMware Network Adapter VMnet8",
            "VirtualBox Host-Only Ethernet Adapter",
            "Hyper-V Virtual Ethernet Adapter",
            "DockerNAT",
            "Loopback Pseudo-Interface 1",
        ] {
            assert!(
                !is_vpn_adapter_name(virt_only),
                "should not flag non-VPN virtual adapter as VPN: {virt_only}"
            );
            assert!(
                is_hypervisor_adapter_name(virt_only),
                "expected hypervisor match for {virt_only}"
            );
            assert!(
                is_virtual_adapter_name(virt_only),
                "expected virtual match for {virt_only}"
            );
        }
    }

    #[test]
    fn rank_local_interfaces_populates_vpn_and_hypervisor_flags() {
        let raw = vec![
            ("Radmin VPN".to_string(), Ipv4Addr::new(26, 40, 12, 5)),
            ("vEthernet (WSL)".to_string(), Ipv4Addr::new(172, 25, 160, 1)),
            ("Wi-Fi".to_string(), Ipv4Addr::new(192, 168, 1, 10)),
        ];
        let (primary, ranked) = rank_local_interfaces(raw, None);
        assert_eq!(primary, Some(Ipv4Addr::new(192, 168, 1, 10)));

        let wifi = ranked.iter().find(|i| i.name == "Wi-Fi").unwrap();
        assert!(wifi.is_primary);
        assert!(!wifi.is_virtual);
        assert!(!wifi.is_vpn);
        assert!(!wifi.is_hypervisor);

        let radmin = ranked.iter().find(|i| i.name == "Radmin VPN").unwrap();
        assert!(radmin.is_virtual);
        assert!(radmin.is_vpn);
        assert!(!radmin.is_hypervisor);

        let wsl = ranked.iter().find(|i| i.name == "vEthernet (WSL)").unwrap();
        assert!(wsl.is_virtual);
        assert!(!wsl.is_vpn);
        assert!(wsl.is_hypervisor);
    }

    #[test]
    fn ipv6_classification_and_endpoint_splitting_work() {
        assert!(is_global_unicast_ipv6(
            "2001:ee0:524e:3170:419:937:1f0c:2f30"
                .parse::<Ipv6Addr>()
                .unwrap()
        ));
        assert!(is_global_unicast_ipv6(
            "2402:800:61c5::1".parse::<Ipv6Addr>().unwrap()
        ));
        assert!(is_global_unicast_ipv6(
            "2606:4700:4700::1111".parse::<Ipv6Addr>().unwrap()
        ));
        assert!(!is_global_unicast_ipv6(Ipv6Addr::LOCALHOST));
        assert!(!is_global_unicast_ipv6(Ipv6Addr::UNSPECIFIED));
        assert!(!is_global_unicast_ipv6(
            "fe80::d4b0:a65b:2c42:915d".parse::<Ipv6Addr>().unwrap()
        ));
        assert!(!is_global_unicast_ipv6(
            "fd12:3456:789a::1".parse::<Ipv6Addr>().unwrap()
        ));
        assert!(!is_global_unicast_ipv6(
            "ff02::1".parse::<Ipv6Addr>().unwrap()
        ));
        assert!(!is_global_unicast_ipv6(
            "2001:db8::1".parse::<Ipv6Addr>().unwrap()
        ));
        assert!(!is_global_unicast_ipv6(
            "2001:0:4136:e378:8000:63bf:3fff:fdd2"
                .parse::<Ipv6Addr>()
                .unwrap()
        ));

        assert_eq!(
            split_endpoint_host_port("play.example.com:46889"),
            Some(("play.example.com".to_string(), 46889))
        );
        assert_eq!(
            split_endpoint_host_port("192.168.1.5:7777"),
            Some(("192.168.1.5".to_string(), 7777))
        );
        assert_eq!(
            split_endpoint_host_port("[2001:ee0::1]:7777"),
            Some(("2001:ee0::1".to_string(), 7777))
        );
        assert_eq!(
            split_endpoint_host_port("  [::1]:7777  "),
            Some(("::1".to_string(), 7777))
        );
        assert_eq!(
            split_endpoint_host_port("[fe80::1%12]:7777"),
            Some(("fe80::1%12".to_string(), 7777))
        );
        assert_eq!(split_endpoint_host_port("[fe80::1%]:7777"), None);
        assert_eq!(split_endpoint_host_port("[fe80::1%1%2]:7777"), None);
        assert_eq!(split_endpoint_host_port("[fe80::1%1:2]:7777"), None);
        assert_eq!(split_endpoint_host_port(""), None);
        assert_eq!(split_endpoint_host_port("play.example.com"), None);
        assert_eq!(split_endpoint_host_port("play.example.com:0"), None);
        assert_eq!(split_endpoint_host_port("play.example.com:70000"), None);
        assert_eq!(split_endpoint_host_port(":7777"), None);
        assert_eq!(split_endpoint_host_port("[2001:ee0::1]"), None);
        assert_eq!(split_endpoint_host_port("[2001:ee0::1]:0"), None);
        assert_eq!(split_endpoint_host_port("2001:ee0::1"), None);
        assert_eq!(
            split_endpoint_host_port("http://play.example.com:7777"),
            None
        );
    }

    #[test]
    fn igd_friendly_name_extracts_label_or_defaults() {
        let xml = r#"<?xml version="1.0"?><root><device><friendlyName>Viettel GPON Home Gateway</friendlyName></device></root>"#;
        assert_eq!(parse_igd_friendly_name(xml), "Viettel GPON Home Gateway");
        assert_eq!(parse_igd_friendly_name("<root><device/></root>"), "IGD Router");
    }

    #[test]
    fn direct_port_host_bridges_tcp_tracks_clients_and_stops_cleanly() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let invalid = start_direct_port_host(
                &tokio::runtime::Handle::current(),
                DirectHostConfig {
                    local_target_port: 0,
                    host_port: 7777,
                    bind_ipv4: None,
                    try_upnp: false,
                    cleanup_local_upnp_on_start: false,
                    initial_banned_ips: Vec::new(),
                    verify_wan_reachability: false,
                    enable_p2p: false,
                    p2p_secret_key_path: None,
                    initial_banned_p2p_peers: Vec::new(),
                },
                crate::server_manager::egui_repaint::RepaintHandle::default(),
            );
            assert!(matches!(
                invalid.current_status(),
                DirectHostStatus::Error(_)
            ));
            assert_eq!(invalid.resolve_bridged_peer(7777), None);
            invalid.set_banned_ips([IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))]);

            let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let upstream_port = upstream.local_addr().unwrap().port();
            tokio::spawn(async move {
                while let Ok((mut s, _)) = upstream.accept().await {
                    tokio::spawn(async move {
                        let mut req = [0u8; 4];
                        while s.read_exact(&mut req).await.is_ok() {
                            if &req == b"PING" && s.write_all(b"PONG").await.is_err() {
                                break;
                            }
                        }
                    });
                }
            });

            if let Ok(v6_listener) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await {
                let v6_port = v6_listener.local_addr().unwrap().port();
                let accept_fut = v6_listener.accept();
                let probe_fut = probe_tcp_endpoint("[::1]", v6_port);
                let (probe_res, _) = tokio::join!(probe_fut, accept_fut);
                assert!(probe_res.is_ok(), "bracketed IPv6 probe must succeed");
            }

            let probe_free = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
            let host_port = probe_free.local_addr().unwrap().port();
            drop(probe_free);

            let handle = start_direct_port_host(
                &tokio::runtime::Handle::current(),
                DirectHostConfig {
                    local_target_port: upstream_port,
                    host_port,
                    bind_ipv4: None,
                    try_upnp: false,
                    cleanup_local_upnp_on_start: false,
                    initial_banned_ips: Vec::new(),
                    verify_wan_reachability: false,
                    enable_p2p: false,
                    p2p_secret_key_path: None,
                    initial_banned_p2p_peers: Vec::new(),
                },
                crate::server_manager::egui_repaint::RepaintHandle::default(),
            );

            let start_deadline = Instant::now() + Duration::from_secs(3);
            let running_info = loop {
                match handle.current_status() {
                    DirectHostStatus::Running(info) => break info,
                    DirectHostStatus::Error(e) => panic!("direct host failed: {e}"),
                    _ if Instant::now() < start_deadline => {
                        tokio::time::sleep(Duration::from_millis(15)).await;
                    }
                    other => panic!("timed out waiting for Running, got {other:?}"),
                }
            };
            assert_eq!(running_info.local_port, upstream_port);
            assert_eq!(running_info.host_port, host_port);
            assert!(running_info.bridging_tcp);
            assert_eq!(running_info.upnp_status, UpnpPortStatus::Disabled);
            assert!(
                running_info.primary_lan_endpoint().is_some()
                    || !running_info.vpn_endpoints.is_empty()
            );

            let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, host_port))
                .await
                .expect("client must connect to bridged host_port");
            client.write_all(b"PING").await.unwrap();
            let mut resp = [0u8; 4];
            client.read_exact(&mut resp).await.unwrap();
            assert_eq!(&resp, b"PONG");

            match handle.current_status() {
                DirectHostStatus::Running(info) => assert_eq!(info.active_clients, 1),
                other => panic!("expected Running while client connected, got {other:?}"),
            }

            drop(client);
            let client_drop_deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let DirectHostStatus::Running(info) = handle.current_status() {
                    if info.active_clients == 0 {
                        break;
                    }
                }
                assert!(
                    Instant::now() < client_drop_deadline,
                    "active_clients did not return to 0 after disconnect"
                );
                tokio::time::sleep(Duration::from_millis(15)).await;
            }

            handle.stop();
            let stop_deadline = Instant::now() + Duration::from_secs(2);
            while !matches!(handle.current_status(), DirectHostStatus::Stopped)
                && Instant::now() < stop_deadline
            {
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            assert!(matches!(handle.current_status(), DirectHostStatus::Stopped));
        });
    }

    #[test]
    fn rank_local_interfaces_prioritizes_physical_lan_over_vpn_and_vm() {
        let raw = vec![
            ("CloudflareWARP".to_string(), Ipv4Addr::new(172, 16, 0, 2)),
            (
                "VMware Network Adapter VMnet8".to_string(),
                Ipv4Addr::new(192, 168, 181, 1),
            ),
            ("vEthernet (WSL)".to_string(), Ipv4Addr::new(172, 25, 160, 1)),
            ("Wi-Fi".to_string(), Ipv4Addr::new(192, 168, 1, 167)),
        ];

        let (primary, ranked) = rank_local_interfaces(raw, Some(Ipv4Addr::new(172, 16, 0, 2)));
        assert_eq!(primary, Some(Ipv4Addr::new(192, 168, 1, 167)));
        assert_eq!(ranked[0].name, "Wi-Fi");
        assert!(ranked[0].is_primary);
        assert!(!ranked[0].is_virtual);
        assert!(ranked.iter().skip(1).all(|i| !i.is_primary && i.is_virtual));

        let multi_lan = vec![
            ("Wi-Fi".to_string(), Ipv4Addr::new(192, 168, 1, 167)),
            ("Ethernet".to_string(), Ipv4Addr::new(10, 0, 0, 15)),
            ("Tailscale".to_string(), Ipv4Addr::new(100, 88, 1, 2)),
        ];
        let (route_primary, route_ranked) =
            rank_local_interfaces(multi_lan.clone(), Some(Ipv4Addr::new(10, 0, 0, 15)));
        assert_eq!(route_primary, Some(Ipv4Addr::new(10, 0, 0, 15)));
        assert_eq!(route_ranked[0].name, "Ethernet");

        let (offline_primary, offline_ranked) = rank_local_interfaces(multi_lan, None);
        assert_eq!(offline_primary, Some(Ipv4Addr::new(192, 168, 1, 167)));
        assert_eq!(offline_ranked[0].name, "Wi-Fi");
    }

    #[test]
    fn parse_wan_control_service_handles_openwrt_miniupnpd_and_prefixes() {
        let xml = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <device>
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:Layer3Forwarding:1</serviceType>
        <controlURL>/ctl/L3F</controlURL>
      </service>
    </serviceList>
    <deviceList>
      <device>
        <serviceList>
          <service>
            <serviceType>urn:schemas-upnp-org:service:WANCommonInterfaceConfig:1</serviceType>
            <controlURL>/ctl/CmnIfCfg</controlURL>
          </service>
          <upnp:service xmlns:upnp="urn:schemas-upnp-org:device-1-0">
            <upnp:serviceType>urn:schemas-upnp-org:service:WANPPPConnection:1</upnp:serviceType>
            <upnp:controlURL/>
          </upnp:service>
          <upnp:service xmlns:upnp="urn:schemas-upnp-org:device-1-0">
            <upnp:serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</upnp:serviceType>
            <upnp:controlURL>/ctl/IPConn</upnp:controlURL>
          </upnp:service>
        </serviceList>
      </device>
    </deviceList>
  </device>
</root>"#;

        assert_eq!(
            parse_wan_control_service(xml),
            Some((
                "urn:schemas-upnp-org:service:WANIPConnection:1".to_string(),
                "/ctl/IPConn".to_string()
            ))
        );
    }

    #[test]
    fn parse_upnp_external_ip_xml_handles_namespaces_and_attributes() {
        let soap = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <u:GetExternalIPAddressResponse xmlns:u="urn:schemas-upnp-org:service:WANIPConnection:1">
      <u:NewExternalIPAddress xmlns:dt="urn:schemas-microsoft-com:datatypes" dt:dt="string">203.0.113.42</u:NewExternalIPAddress>
    </u:GetExternalIPAddressResponse>
  </s:Body>
</s:Envelope>"#;

        assert_eq!(
            parse_upnp_external_ip_xml(soap),
            Some(Ipv4Addr::new(203, 0, 113, 42))
        );

        let empty_soap = r#"<s:Envelope><s:Body><u:GetExternalIPAddressResponse><NewExternalIPAddress/></u:GetExternalIPAddressResponse></s:Body></s:Envelope>"#;
        assert_eq!(parse_upnp_external_ip_xml(empty_soap), None);
    }

    #[test]
    fn parse_netsh_rule_enabled_detects_enabled_disabled_and_blocked_rules() {
        let enabled_rule = "\
Rule Name:                            TerrariaLauncher_TCP_7777
----------------------------------------------------------------------
Enabled:                              Yes
Direction:                            In
Profiles:                             Domain,Private,Public
Protocol:                             TCP
LocalPort:                            7777
Action:                               Allow
";
        assert!(parse_netsh_rule_enabled(enabled_rule));

        let disabled_rule = "\
Rule Name:                            TerrariaLauncher_TCP_7777
Enabled:                              No
Action:                               Allow
";
        assert!(!parse_netsh_rule_enabled(disabled_rule));

        let block_rule = "\
Rule Name:                            TerrariaLauncher_TCP_7777
Enabled:                              Yes
Action:                               Block
";
        assert!(!parse_netsh_rule_enabled(block_rule));

        assert!(!parse_netsh_rule_enabled(""));
        assert!(!parse_netsh_rule_enabled(
            "No rules match the specified criteria."
        ));
    }

    #[test]
    fn extract_igd_location_from_ssdp_filters_non_igd_packets() {
        let chromecast = "\
HTTP/1.1 200 OK\r\n\
CACHE-CONTROL: max-age=1800\r\n\
EXT:\r\n\
LOCATION: http://192.168.1.55:8008/ssdp/device-desc.xml\r\n\
SERVER: Linux/3.8.13, UPnP/1.0\r\n\
ST: urn:dial-multiscreen-org:service:dial:1\r\n\
USN: uuid:1234::urn:dial-multiscreen-org:service:dial:1\r\n\r\n";
        assert_eq!(extract_igd_location_from_ssdp(chromecast), None);

        let igd = "\
HTTP/1.1 200 OK\r\n\
CACHE-CONTROL: max-age=120\r\n\
ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\
USN: uuid:router-1::urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\
LOCATION: http://192.168.1.1:5000/rootDesc.xml\r\n\r\n";
        assert_eq!(
            extract_igd_location_from_ssdp(igd),
            Some("http://192.168.1.1:5000/rootDesc.xml".to_string())
        );

        let wan_ip_v2 = "\
HTTP/1.1 200 OK\r\n\
LOCATION: http://192.168.1.1:1900/igd.xml\r\n\
ST: urn:schemas-upnp-org:service:WANIPConnection:2\r\n\r\n";
        assert_eq!(
            extract_igd_location_from_ssdp(wan_ip_v2),
            Some("http://192.168.1.1:1900/igd.xml".to_string())
        );
    }

    #[test]
    fn probe_local_tcp_listening_detects_bound_port_and_zero_port() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(!probe_local_tcp_listening(0).await);
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            assert!(probe_local_tcp_listening(port).await);
            drop(listener);
            assert!(!probe_local_tcp_listening(port).await);
        });
    }

    #[test]
    #[ignore = "requires live LAN interface"]
    fn live_end_to_end_direct_host_and_upnp_roundtrip() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (primary_ip, interfaces) = collect_local_interfaces();
            let lan_ip = primary_ip.expect("expected a physical LAN IP");
            assert!(
                interfaces
                    .iter()
                    .find(|i| i.ipv4 == lan_ip)
                    .is_some_and(|i| !i.is_virtual),
                "primary LAN IP must not be a virtual/VPN adapter: {lan_ip} ({interfaces:?})"
            );

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let local_port = listener.local_addr().unwrap().port();
            assert!(probe_local_tcp_listening(local_port).await);

            match open_upnp_port(7778).await {
                Ok(lease) => {
                    assert_eq!(lease.local_ip, lan_ip);
                    assert_eq!(lease.port, 7778);
                    println!(
                        "UPNP_OK local={} ext={:?} cgnat={}",
                        lease.local_ip, lease.external_ip, lease.is_cgnat
                    );
                    close_upnp_port(&lease)
                        .await
                        .expect("UPnP close should succeed cleanly");
                }
                Err(err) => {
                    println!("UPNP_ROUTER_RESPONSE={err}");
                }
            }

            tokio::spawn(async move {
                while let Ok((mut sock, _)) = listener.accept().await {
                    tokio::spawn(async move {
                        let mut buf = [0u8; 32];
                        if let Ok(n) = sock.read(&mut buf).await {
                            if &buf[..n] == b"TL-HOST-PING" {
                                let _ = sock.write_all(b"TL-HOST-PONG").await;
                            }
                        }
                    });
                }
            });

            let probe_free = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
            let host_port = probe_free.local_addr().unwrap().port();
            drop(probe_free);

            let handle = start_direct_port_host(
                &tokio::runtime::Handle::current(),
                DirectHostConfig {
                    local_target_port: local_port,
                    host_port,
                    bind_ipv4: None,
                    try_upnp: false,
                    cleanup_local_upnp_on_start: false,
                    initial_banned_ips: Vec::new(),
                    verify_wan_reachability: false,
                    enable_p2p: false,
                    p2p_secret_key_path: None,
                    initial_banned_p2p_peers: Vec::new(),
                },
                crate::server_manager::egui_repaint::RepaintHandle::default(),
            );

            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match handle.current_status() {
                    DirectHostStatus::Running(_) => break,
                    DirectHostStatus::Error(e) => panic!("direct host failed: {e}"),
                    _ if Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    other => panic!("timed out waiting for direct host, got {other:?}"),
                }
            }

            let mut lan_client = timeout(
                Duration::from_secs(4),
                TcpStream::connect((lan_ip, host_port)),
            )
            .await
            .expect("connect timeout")
            .expect("connect to LAN direct host");
            lan_client.write_all(b"TL-HOST-PING").await.unwrap();
            let mut reply = [0u8; 32];
            let n = timeout(Duration::from_secs(4), lan_client.read(&mut reply))
                .await
                .expect("read timeout")
                .expect("read reply");
            assert_eq!(&reply[..n], b"TL-HOST-PONG");
            drop(lan_client);

            handle.stop();
            let stop_deadline = Instant::now() + Duration::from_secs(3);
            while !matches!(handle.current_status(), DirectHostStatus::Stopped)
                && Instant::now() < stop_deadline
            {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            assert!(matches!(handle.current_status(), DirectHostStatus::Stopped));
        });
    }

    #[test]
    fn parse_upnp_soap_fault_extracts_error_code_and_description() {
        let fault_xml = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><s:Fault><detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>606</errorCode><errorDescription>Action not authorized</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>"#;
        assert_eq!(
            parse_upnp_soap_fault(fault_xml),
            Some(("606".to_string(), "Action not authorized".to_string()))
        );
        let msg = format_upnp_soap_rejection(fault_xml);
        assert!(msg.contains("606"));
        assert!(msg.contains("Radmin VPN / Tailscale / ZeroTier"));
    }

    #[test]
    fn direct_host_info_partitions_vpn_and_prefers_upnp_then_vpn() {
        let interfaces = vec![
            LocalInterfaceInfo {
                name: "Wi-Fi".to_string(),
                ipv4: Ipv4Addr::new(192, 168, 1, 10),
                is_primary: true,
                is_virtual: false,
                is_vpn: false,
                is_hypervisor: false,
            },
            LocalInterfaceInfo {
                name: "Radmin VPN".to_string(),
                ipv4: Ipv4Addr::new(26, 40, 12, 5),
                is_primary: false,
                is_virtual: true,
                is_vpn: true,
                is_hypervisor: false,
            },
            LocalInterfaceInfo {
                name: "Radmin VPN Duplicate".to_string(),
                ipv4: Ipv4Addr::new(26, 40, 12, 5),
                is_primary: false,
                is_virtual: true,
                is_vpn: true,
                is_hypervisor: false,
            },
            LocalInterfaceInfo {
                name: "vEthernet (WSL)".to_string(),
                ipv4: Ipv4Addr::new(172, 25, 160, 1),
                is_primary: false,
                is_virtual: true,
                is_vpn: false,
                is_hypervisor: true,
            },
        ];

        let partitioned = partition_host_endpoints(&interfaces, None, 7777);
        assert_eq!(
            partitioned.lan,
            vec![("Wi-Fi".to_string(), "192.168.1.10:7777".to_string())]
        );
        assert_eq!(
            partitioned.vpn,
            vec![("Radmin VPN".to_string(), "26.40.12.5:7777".to_string())]
        );

        let pinned_vpn =
            partition_host_endpoints(&interfaces, Some(Ipv4Addr::new(26, 40, 12, 5)), 7777);
        assert!(pinned_vpn.lan.is_empty());
        assert_eq!(
            pinned_vpn.vpn,
            vec![("Radmin VPN".to_string(), "26.40.12.5:7777".to_string())]
        );

        let pinned_phys =
            partition_host_endpoints(&interfaces, Some(Ipv4Addr::new(192, 168, 1, 10)), 7777);
        assert_eq!(
            pinned_phys.lan,
            vec![("Wi-Fi".to_string(), "192.168.1.10:7777".to_string())]
        );
        assert!(pinned_phys.vpn.is_empty());

        let fallback = partition_host_endpoints(&[], None, 7777);
        assert_eq!(
            fallback.lan,
            vec![("Local".to_string(), "127.0.0.1:7777".to_string())]
        );
        assert!(fallback.vpn.is_empty());

        let mut info = DirectHostActiveInfo {
            local_port: 7777,
            host_port: 7777,
            lan_endpoints: partitioned.lan,
            vpn_endpoints: partitioned.vpn,
            ipv6_endpoint: Some("[2001:ee0::1]:7777".to_string()),
            ipv6_pinhole_status: Ipv6PinholeStatus::Disabled,
            ipv6_reachability: ExternalReachability::NotChecked,
            upnp_status: UpnpPortStatus::Mapped {
                external_ip: Ipv4Addr::new(203, 0, 113, 9),
                external_port: 7777,
                gateway_label: "Router".to_string(),
            },
            ipv4_reachability: ExternalReachability::NotChecked,
            bridging_tcp: true,
            active_clients: 0,
            p2p_status: P2pHostStatus::Disabled,
        };
        assert_eq!(
            info.preferred_online_endpoint(),
            Some("203.0.113.9:7777".to_string())
        );

        info.upnp_status = UpnpPortStatus::Disabled;
        assert_eq!(
            info.preferred_online_endpoint(),
            Some("26.40.12.5:7777".to_string())
        );

        info.vpn_endpoints.clear();
        assert_eq!(info.preferred_online_endpoint(), None);
    }

    #[test]
    fn ipv4_to_ipv6_client_bridge_forwards_tcp_and_stops_cleanly() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            assert!(matches!(
                start_ipv4_to_ipv6_client_bridge(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 0, 0, 0))
                    .await,
                Err(LauncherError::InvalidInput(_))
            ));

            let free_v6 = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap();
            let closed_v6_port = free_v6.local_addr().unwrap().port();
            drop(free_v6);
            assert!(matches!(
                start_ipv4_to_ipv6_client_bridge(SocketAddrV6::new(
                    Ipv6Addr::LOCALHOST,
                    closed_v6_port,
                    0,
                    0
                ))
                .await,
                Err(LauncherError::Network(_))
            ));

            let v6_listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap();
            let v6_port = v6_listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                while let Ok((mut sock, _)) = v6_listener.accept().await {
                    tokio::spawn(async move {
                        let mut buf = [0u8; 64];
                        while let Ok(n) = sock.read(&mut buf).await {
                            if n == 0 || sock.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    });
                }
            });

            let bridge = start_ipv4_to_ipv6_client_bridge(SocketAddrV6::new(
                Ipv6Addr::LOCALHOST,
                v6_port,
                0,
                0,
            ))
            .await
            .expect("bridge to [::1] echo listener must succeed");
            let local_port = bridge.local_port();
            assert_ne!(local_port, 0);

            let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, local_port))
                .await
                .expect("IPv4 client must connect to local bridge");
            client.write_all(b"TL-V6-BRIDGE").await.unwrap();
            let mut reply = [0u8; 12];
            client.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"TL-V6-BRIDGE");
            drop(client);

            bridge.stop();
            drop(bridge);
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if TcpStream::connect((Ipv4Addr::LOCALHOST, local_port))
                    .await
                    .is_err()
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "local bridge listener did not stop after drop"
                );
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
        });
    }

    #[test]
    fn is_local_probe_host_recognizes_local_lan_vpn_and_global_ipv6() {
        let snap = NetworkSnapshot {
            primary_lan_ip: Some(Ipv4Addr::new(192, 168, 1, 50)),
            interfaces: vec![LocalInterfaceInfo {
                name: "Tailscale".to_string(),
                ipv4: Ipv4Addr::new(100, 88, 1, 2),
                is_primary: false,
                is_virtual: true,
                is_vpn: true,
                is_hypervisor: false,
            }],
            global_ipv6: Some("2001:ee0:524e::1".parse().unwrap()),
            public_ip: Some(Ipv4Addr::new(203, 0, 113, 42)),
            ..Default::default()
        };

        assert!(is_local_probe_host("localhost", &snap));
        assert!(is_local_probe_host("  LOCALHOST  ", &snap));
        assert!(is_local_probe_host("127.0.0.1", &snap));
        assert!(is_local_probe_host("127.0.1.1", &snap));
        assert!(is_local_probe_host("::1", &snap));
        assert!(is_local_probe_host("[::1]", &snap));
        assert!(is_local_probe_host("192.168.1.50", &snap));
        assert!(is_local_probe_host("100.88.1.2", &snap));
        assert!(is_local_probe_host("2001:ee0:524e::1", &snap));
        assert!(is_local_probe_host("[2001:ee0:524e::1]", &snap));

        assert!(!is_local_probe_host("", &snap));
        assert!(!is_local_probe_host("203.0.113.42", &snap));
        assert!(!is_local_probe_host("play.example.com", &snap));
        assert!(!is_local_probe_host("8.8.8.8", &snap));
        assert!(!is_local_probe_host("fe80::1", &snap));
    }

    #[cfg(windows)]
    #[test]
    fn passive_local_listener_probes_open_zero_tcp_connections() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            assert!(!probe_local_tcp_listening(0).await);
            assert!(probe_local_endpoint_listening("", 7777).await.is_err());
            assert!(probe_local_endpoint_listening("127.0.0.1", 0).await.is_err());

            let v4_loop = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let v4_loop_port = v4_loop.local_addr().unwrap().port();
            assert!(probe_local_tcp_listening(v4_loop_port).await);
            assert!(probe_local_endpoint_listening("127.0.0.1", v4_loop_port)
                .await
                .is_ok());
            assert!(probe_local_endpoint_listening("localhost", v4_loop_port)
                .await
                .is_ok());
            assert!(
                timeout(Duration::from_millis(60), v4_loop.accept())
                    .await
                    .is_err(),
                "passive IPv4 loopback probe must not open a TCP connection"
            );
            drop(v4_loop);
            assert!(!probe_local_tcp_listening(v4_loop_port).await);
            assert!(probe_local_endpoint_listening("127.0.0.1", v4_loop_port)
                .await
                .is_err());

            let v4_any = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
            let v4_any_port = v4_any.local_addr().unwrap().port();
            assert!(probe_local_tcp_listening(v4_any_port).await);
            assert!(probe_local_endpoint_listening("0.0.0.0", v4_any_port)
                .await
                .is_ok());
            assert!(probe_local_endpoint_listening("127.0.0.1", v4_any_port)
                .await
                .is_ok());
            assert!(probe_local_endpoint_listening("localhost", v4_any_port)
                .await
                .is_ok());
            assert!(
                timeout(Duration::from_millis(60), v4_any.accept())
                    .await
                    .is_err(),
                "passive IPv4 wildcard probe must not open a TCP connection"
            );
            drop(v4_any);

            if let Ok(v6_loop) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await {
                let v6_port = v6_loop.local_addr().unwrap().port();
                assert!(probe_local_endpoint_listening("[::1]", v6_port)
                    .await
                    .is_ok());
                assert!(probe_local_endpoint_listening("::1", v6_port)
                    .await
                    .is_ok());
                assert!(probe_local_endpoint_listening("localhost", v6_port)
                    .await
                    .is_ok());
                let bridge = start_ipv4_to_ipv6_client_bridge(SocketAddrV6::new(
                    Ipv6Addr::LOCALHOST,
                    v6_port,
                    0,
                    0,
                ))
                .await
                .expect("local IPv6 bridge pre-flight must succeed passively");
                assert!(
                    timeout(Duration::from_millis(60), v6_loop.accept())
                        .await
                        .is_err(),
                    "passive IPv6 loopback probe and bridge pre-flight must not open a TCP connection"
                );
                bridge.stop();
            }
        });
    }

    #[cfg(windows)]
    #[test]
    fn direct_port_host_same_port_startup_opens_zero_self_probe_connections() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
            let server_port = listener.local_addr().unwrap().port();

            let handle = start_direct_port_host(
                &tokio::runtime::Handle::current(),
                DirectHostConfig {
                    local_target_port: server_port,
                    host_port: server_port,
                    bind_ipv4: None,
                    try_upnp: false,
                    cleanup_local_upnp_on_start: false,
                    initial_banned_ips: Vec::new(),
                    verify_wan_reachability: false,
                    enable_p2p: false,
                    p2p_secret_key_path: None,
                    initial_banned_p2p_peers: Vec::new(),
                },
                crate::server_manager::egui_repaint::RepaintHandle::default(),
            );

            let start_deadline = Instant::now() + Duration::from_secs(3);
            loop {
                match handle.current_status() {
                    DirectHostStatus::Running(info) => {
                        assert_eq!(info.local_port, server_port);
                        assert_eq!(info.host_port, server_port);
                        break;
                    }
                    DirectHostStatus::Error(e) => {
                        panic!("same-port direct host failed unexpectedly: {e}");
                    }
                    _ if Instant::now() < start_deadline => {
                        tokio::time::sleep(Duration::from_millis(15)).await;
                    }
                    other => panic!("timed out waiting for Running, got {other:?}"),
                }
            }

            assert!(
                timeout(Duration::from_millis(80), listener.accept())
                    .await
                    .is_err(),
                "start_direct_port_host must not open dummy self-probe TCP connections to server"
            );

            handle.stop();
            let stop_deadline = Instant::now() + Duration::from_secs(2);
            while !matches!(handle.current_status(), DirectHostStatus::Stopped)
                && Instant::now() < stop_deadline
            {
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            assert!(matches!(handle.current_status(), DirectHostStatus::Stopped));
        });
    }

    #[test]
    fn resolve_ipv6_bridge_target_handles_ipv6_variants_and_rejects_ipv4_or_localhost() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let expected: Ipv6Addr = "2405:4802:1d52::1".parse().unwrap();
            let link_local: Ipv6Addr = "fe80::1".parse().unwrap();
            assert_eq!(
                resolve_ipv6_bridge_target("2405:4802:1d52::1", 7777).await,
                Some(SocketAddrV6::new(expected, 7777, 0, 0))
            );
            assert_eq!(
                resolve_ipv6_bridge_target("  [2405:4802:1d52::1]  ", 7777).await,
                Some(SocketAddrV6::new(expected, 7777, 0, 0))
            );
            assert_eq!(
                resolve_ipv6_bridge_target("[2405:4802:1d52::1%12]", 7777).await,
                Some(SocketAddrV6::new(expected, 7777, 0, 12))
            );
            assert_eq!(
                resolve_ipv6_bridge_target("2405:4802:1d52::1%12", 0).await,
                Some(SocketAddrV6::new(expected, 7777, 0, 12))
            );
            assert_eq!(
                resolve_ipv6_bridge_target("[fe80::1%12]", 7779).await,
                Some(SocketAddrV6::new(link_local, 7779, 0, 12))
            );
            assert_eq!(resolve_ipv6_bridge_target("[fe80::1%]", 7777).await, None);
            assert_eq!(resolve_ipv6_bridge_target("fe80::1%1%2", 7777).await, None);

            assert_eq!(resolve_ipv6_bridge_target("", 7777).await, None);
            assert_eq!(resolve_ipv6_bridge_target("[]", 7777).await, None);
            assert_eq!(resolve_ipv6_bridge_target("127.0.0.1", 7777).await, None);
            assert_eq!(resolve_ipv6_bridge_target("192.168.1.25", 7777).await, None);
            assert_eq!(resolve_ipv6_bridge_target("localhost", 7777).await, None);
            assert_eq!(resolve_ipv6_bridge_target("  LOCALHOST  ", 7777).await, None);
        });
    }

    async fn wait_for_direct_host_running(handle: &DirectPortHostHandle) -> DirectHostActiveInfo {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match handle.current_status() {
                DirectHostStatus::Running(info) => return info,
                DirectHostStatus::Error(e) => panic!("direct host failed: {e}"),
                _ if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(15)).await;
                }
                other => panic!("timed out waiting for Running, got {other:?}"),
            }
        }
    }

    async fn stop_direct_host_and_wait(handle: &DirectPortHostHandle) {
        handle.stop();
        let stop_deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(handle.current_status(), DirectHostStatus::Stopped)
            && Instant::now() < stop_deadline
        {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        assert!(matches!(handle.current_status(), DirectHostStatus::Stopped));
    }

    #[test]
    fn normalize_ban_set_canonicalizes_and_excludes_loopback_and_unspecified() {
        let banned = normalize_ban_set([
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            "::ffff:127.0.0.1".parse().unwrap(),
            "::ffff:0.0.0.0".parse().unwrap(),
            "::ffff:192.168.1.50".parse().unwrap(),
            "2001:db8::42".parse().unwrap(),
        ]);

        assert_eq!(banned.len(), 2);
        assert!(banned.contains(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50))));
        assert!(banned.contains(&"2001:db8::42".parse::<IpAddr>().unwrap()));
    }

    #[test]
    fn direct_port_host_tracks_bridged_peers_and_enforces_ip_bans() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let upstream_port = upstream.local_addr().unwrap().port();
            let (peer_port_tx, mut peer_port_rx) = tokio::sync::mpsc::unbounded_channel();

            tokio::spawn(async move {
                while let Ok((mut sock, peer_addr)) = upstream.accept().await {
                    let _ = peer_port_tx.send(peer_addr.port());
                    tokio::spawn(async move {
                        let mut req = [0u8; 4];
                        while sock.read_exact(&mut req).await.is_ok() {
                            if &req == b"PING" && sock.write_all(b"PONG").await.is_err() {
                                break;
                            }
                        }
                    });
                }
            });

            let probe_free = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
            let host_port = probe_free.local_addr().unwrap().port();
            drop(probe_free);

            let handle = start_direct_port_host(
                &tokio::runtime::Handle::current(),
                DirectHostConfig {
                    local_target_port: upstream_port,
                    host_port,
                    bind_ipv4: None,
                    try_upnp: false,
                    cleanup_local_upnp_on_start: false,
                    initial_banned_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                    verify_wan_reachability: false,
                    enable_p2p: false,
                    p2p_secret_key_path: None,
                    initial_banned_p2p_peers: Vec::new(),
                },
                crate::server_manager::egui_repaint::RepaintHandle::default(),
            );
            let _running = wait_for_direct_host_running(&handle).await;

            let mut loop_client = TcpStream::connect((Ipv4Addr::LOCALHOST, host_port))
                .await
                .expect("loopback client must connect despite LOCALHOST in initial_banned_ips");
            loop_client.write_all(b"PING").await.unwrap();
            let mut reply = [0u8; 4];
            loop_client.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"PONG");

            let loop_outbound_port = timeout(Duration::from_secs(2), peer_port_rx.recv())
                .await
                .expect("expected upstream peer port")
                .expect("channel open");
            assert_eq!(
                handle.resolve_bridged_peer(loop_outbound_port),
                Some(IpAddr::V4(Ipv4Addr::LOCALHOST))
            );
            assert_eq!(handle.resolve_bridged_peer(0), None);

            handle.set_banned_ips([
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                "::ffff:127.0.0.1".parse().unwrap(),
            ]);

            loop_client.write_all(b"PING").await.unwrap();
            loop_client.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"PONG");

            let mut loop_client_2 = TcpStream::connect((Ipv4Addr::LOCALHOST, host_port))
                .await
                .expect("new loopback connection must remain allowed");
            loop_client_2.write_all(b"PING").await.unwrap();
            loop_client_2.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"PONG");
            let loop_outbound_port_2 = timeout(Duration::from_secs(2), peer_port_rx.recv())
                .await
                .expect("expected second upstream peer port")
                .expect("channel open");

            drop(loop_client);
            drop(loop_client_2);

            let clear_deadline = Instant::now() + Duration::from_secs(2);
            while (handle.resolve_bridged_peer(loop_outbound_port).is_some()
                || handle.resolve_bridged_peer(loop_outbound_port_2).is_some())
                && Instant::now() < clear_deadline
            {
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            assert_eq!(handle.resolve_bridged_peer(loop_outbound_port), None);
            assert_eq!(handle.resolve_bridged_peer(loop_outbound_port_2), None);

            if let Some(lan_ip) =
                detect_primary_lan_ipv4().filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
            {
                if let Ok(Ok(mut lan_client)) = timeout(
                    Duration::from_secs(2),
                    TcpStream::connect((lan_ip, host_port)),
                )
                .await
                {
                    lan_client.write_all(b"PING").await.unwrap();
                    lan_client.read_exact(&mut reply).await.unwrap();
                    assert_eq!(&reply, b"PONG");

                    let lan_outbound_port = timeout(Duration::from_secs(2), peer_port_rx.recv())
                        .await
                        .expect("expected LAN upstream peer port")
                        .expect("channel open");
                    let peer_ip = lan_client.local_addr().unwrap().ip().to_canonical();
                    assert_eq!(handle.resolve_bridged_peer(lan_outbound_port), Some(peer_ip));

                    if is_bannable_peer_ip(peer_ip) {
                        handle.set_banned_ips([peer_ip]);

                        let mut drop_buf = [0u8; 4];
                        let drop_res =
                            timeout(Duration::from_secs(1), lan_client.read(&mut drop_buf))
                                .await
                                .expect("banned peer must be dropped immediately");
                        assert!(
                            matches!(drop_res, Ok(0) | Err(_)),
                            "expected closed connection after ban, got {drop_res:?}"
                        );

                        let peer_clear_deadline = Instant::now() + Duration::from_secs(2);
                        while handle.resolve_bridged_peer(lan_outbound_port).is_some()
                            && Instant::now() < peer_clear_deadline
                        {
                            tokio::time::sleep(Duration::from_millis(15)).await;
                        }
                        assert_eq!(handle.resolve_bridged_peer(lan_outbound_port), None);

                        if let Ok(Ok(mut blocked_client)) = timeout(
                            Duration::from_secs(1),
                            TcpStream::connect((lan_ip, host_port)),
                        )
                        .await
                        {
                            let _ = blocked_client.write_all(b"PING").await;
                            let blocked_read =
                                timeout(Duration::from_secs(1), blocked_client.read(&mut drop_buf))
                                    .await
                                    .expect("banned reconnect must close immediately");
                            assert!(
                                matches!(blocked_read, Ok(0) | Err(_)),
                                "banned reconnect must not succeed, got {blocked_read:?}"
                            );
                        }
                        assert!(
                            timeout(Duration::from_millis(120), peer_port_rx.recv())
                                .await
                                .is_err(),
                            "banned peer must be rejected before connecting upstream"
                        );

                        handle.set_banned_ips(std::iter::empty::<IpAddr>());
                        let mut unbanned_client = timeout(
                            Duration::from_secs(2),
                            TcpStream::connect((lan_ip, host_port)),
                        )
                        .await
                        .expect("unbanned connect timeout")
                        .expect("unbanned client must connect");
                        unbanned_client.write_all(b"PING").await.unwrap();
                        unbanned_client.read_exact(&mut reply).await.unwrap();
                        assert_eq!(&reply, b"PONG");
                        drop(unbanned_client);
                        let _ = timeout(Duration::from_secs(1), peer_port_rx.recv()).await;

                        stop_direct_host_and_wait(&handle).await;

                        let probe_free_2 =
                            TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
                        let host_port_2 = probe_free_2.local_addr().unwrap().port();
                        drop(probe_free_2);

                        let mapped_ban = match peer_ip {
                            IpAddr::V4(v4) => IpAddr::V6(v4.to_ipv6_mapped()),
                            other => other,
                        };
                        let handle_2 = start_direct_port_host(
                            &tokio::runtime::Handle::current(),
                            DirectHostConfig {
                                local_target_port: upstream_port,
                                host_port: host_port_2,
                                bind_ipv4: None,
                                try_upnp: false,
                                cleanup_local_upnp_on_start: false,
                                initial_banned_ips: vec![mapped_ban],
                                verify_wan_reachability: false,
                                enable_p2p: false,
                                p2p_secret_key_path: None,
                                initial_banned_p2p_peers: Vec::new(),
                            },
                            crate::server_manager::egui_repaint::RepaintHandle::default(),
                        );
                        let _running_2 = wait_for_direct_host_running(&handle_2).await;

                        if let Ok(Ok(mut init_blocked)) = timeout(
                            Duration::from_secs(1),
                            TcpStream::connect((lan_ip, host_port_2)),
                        )
                        .await
                        {
                            let _ = init_blocked.write_all(b"PING").await;
                            let init_read =
                                timeout(Duration::from_secs(1), init_blocked.read(&mut drop_buf))
                                    .await
                                    .expect("initial_banned_ips must drop peer immediately");
                            assert!(
                                matches!(init_read, Ok(0) | Err(_)),
                                "initial_banned_ips must reject peer, got {init_read:?}"
                            );
                        }
                        assert!(
                            timeout(Duration::from_millis(120), peer_port_rx.recv())
                                .await
                                .is_err(),
                            "initial_banned_ips must block peer before upstream connect"
                        );
                        stop_direct_host_and_wait(&handle_2).await;
                        return;
                    }
                }
            }

            stop_direct_host_and_wait(&handle).await;
        });
    }

    #[cfg(windows)]
    #[test]
    fn direct_port_host_same_port_skips_redundant_ipv4_bridge_when_already_listening() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let upstream = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
            let server_port = upstream.local_addr().unwrap().port();
            let probe_ip = detect_primary_lan_ipv4().unwrap_or(Ipv4Addr::LOCALHOST);

            let handle = start_direct_port_host(
                &tokio::runtime::Handle::current(),
                DirectHostConfig {
                    local_target_port: server_port,
                    host_port: server_port,
                    bind_ipv4: Some(probe_ip),
                    try_upnp: false,
                    cleanup_local_upnp_on_start: false,
                    initial_banned_ips: Vec::new(),
                    verify_wan_reachability: false,
                    enable_p2p: false,
                    p2p_secret_key_path: None,
                    initial_banned_p2p_peers: Vec::new(),
                },
                crate::server_manager::egui_repaint::RepaintHandle::default(),
            );

            let info = wait_for_direct_host_running(&handle).await;
            assert!(!info.bridging_tcp);

            let (client_res, accept_res) = tokio::join!(
                timeout(
                    Duration::from_secs(2),
                    TcpStream::connect((probe_ip, server_port))
                ),
                timeout(Duration::from_secs(2), upstream.accept())
            );
            let _client = client_res
                .expect("connect timeout")
                .expect("direct connect must succeed");
            let (_server_sock, peer_addr) = accept_res
                .expect("accept timeout")
                .expect("upstream accept must succeed");
            assert_eq!(peer_addr.ip(), IpAddr::V4(probe_ip));

            stop_direct_host_and_wait(&handle).await;
        });
    }

    #[test]
    fn direct_port_host_stop_and_wait_synchronizes_upnp_cleanup() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        let invalid_handle = start_direct_port_host(
            rt.handle(),
            DirectHostConfig {
                local_target_port: 0,
                host_port: 0,
                bind_ipv4: None,
                try_upnp: false,
                cleanup_local_upnp_on_start: false,
                initial_banned_ips: Vec::new(),
                verify_wan_reachability: false,
                enable_p2p: false,
                p2p_secret_key_path: None,
                initial_banned_p2p_peers: Vec::new(),
            },
            crate::server_manager::egui_repaint::RepaintHandle::default(),
        );
        let started = Instant::now();
        invalid_handle.stop_and_wait(Duration::from_secs(2));
        assert!(started.elapsed() < Duration::from_millis(250));

        let live_handle = DirectPortHostHandle {
            cancel: CancellationToken::new(),
            status: Arc::new(RwLock::new(DirectHostStatus::Idle)),
            banned_ips: Arc::new(RwLock::new(HashSet::new())),
            banned_p2p_peers: Arc::new(RwLock::new(HashSet::new())),
            active_p2p_conns: Arc::new(Mutex::new(Vec::new())),
            bridge_peers: Arc::new(RwLock::new(HashMap::new())),
            has_upnp_lease: Arc::new(AtomicBool::new(true)),
            has_p2p_endpoint: Arc::new(AtomicBool::new(false)),
            cleanup_done: Arc::new((Mutex::new(false), Condvar::new())),
            rechecking: Arc::new(AtomicBool::new(false)),
        };
        let cleanup_clone = Arc::clone(&live_handle.cleanup_done);
        let cancel_clone = live_handle.cancel.clone();
        let releaser = std::thread::spawn(move || {
            while !cancel_clone.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(40));
            let (lock, cvar) = &*cleanup_clone;
            let mut done = lock.lock().unwrap_or_else(|e| e.into_inner());
            *done = true;
            cvar.notify_all();
        });

        let wait_started = Instant::now();
        live_handle.stop_and_wait(Duration::from_secs(2));
        let elapsed = wait_started.elapsed();
        assert!(live_handle.cancel.is_cancelled());
        assert!(elapsed >= Duration::from_millis(30));
        assert!(elapsed < Duration::from_secs(1));
        releaser.join().unwrap();
    }

    #[test]
    fn upnp_add_port_mapping_payload_and_lease_fallback_work() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let service_type = "urn:schemas-upnp-org:service:WANIPConnection:1";
        let local_ip = Ipv4Addr::new(192, 168, 1, 10);
        let body_zero = build_upnp_add_port_mapping_body(service_type, 7777, 7777, local_ip, 0);
        assert!(body_zero.contains("<NewLeaseDuration>0</NewLeaseDuration>"));
        assert!(body_zero.contains("<NewExternalPort>7777</NewExternalPort>"));
        assert!(body_zero.contains("<NewInternalClient>192.168.1.10</NewInternalClient>"));
        assert!(body_zero.contains("<NewProtocol>TCP</NewProtocol>"));

        let body_timed = build_upnp_add_port_mapping_body(service_type, 7777, 7777, local_ip, 3600);
        assert!(body_timed.contains("<NewLeaseDuration>3600</NewLeaseDuration>"));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let control_url = format!("http://127.0.0.1:{port}/ctl/IPConn");

            let server_task = tokio::spawn(async move {
                let mut seen_bodies = Vec::new();
                for idx in 0..3 {
                    let (mut sock, _) = listener.accept().await.unwrap();
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 1024];
                    let header_end = loop {
                        let n = sock.read(&mut tmp).await.unwrap();
                        assert!(n > 0, "unexpected EOF reading HTTP headers");
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
                    let content_len = headers
                        .lines()
                        .find_map(|line| {
                            let (k, v) = line.split_once(':')?;
                            k.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    while buf.len() < header_end + content_len {
                        let n = sock.read(&mut tmp).await.unwrap();
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let req_body =
                        String::from_utf8_lossy(&buf[header_end..header_end + content_len])
                            .to_string();
                    seen_bodies.push(req_body);

                    let resp = if idx == 0 {
                        let err_xml = "<errorCode>725</errorCode><errorDescription>OnlyPermanentLeasesSupported</errorDescription>";
                        format!(
                            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{err_xml}",
                            err_xml.len()
                        )
                    } else {
                        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    };
                    sock.write_all(resp.as_bytes()).await.unwrap();
                    let _ = sock.shutdown().await;
                }
                seen_bodies
            });

            let client = build_lan_upnp_client().unwrap();
            let negotiated = add_port_mapping_with_lease_fallback(
                &client,
                &control_url,
                service_type,
                7777,
                7777,
                local_ip,
            )
            .await
            .unwrap();
            assert_eq!(negotiated, 3600);

            send_upnp_add_port_mapping(
                &client,
                &control_url,
                service_type,
                7777,
                7777,
                local_ip,
                1800,
            )
            .await
            .unwrap();

            let captured = server_task.await.unwrap();
            assert_eq!(captured.len(), 3);
            assert!(captured[0].contains("<NewLeaseDuration>0</NewLeaseDuration>"));
            assert!(captured[1].contains("<NewLeaseDuration>3600</NewLeaseDuration>"));
            assert!(captured[2].contains("<NewLeaseDuration>1800</NewLeaseDuration>"));
        });
    }

    #[test]
    fn probe_network_snapshot_with_client_probes_distinct_server_and_firewall_ports() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let listening_port = listener.local_addr().unwrap().port();

            let snap = probe_network_snapshot_with_client(
                listening_port,
                0,
                build_http_client(Some("://invalid-proxy")),
            )
            .await;

            assert!(snap.local_port_listening);
            assert_eq!(snap.firewall_rule_active, Some(false));
            assert!(snap.public_ip.is_none());
            assert!(snap.public_ip_error.is_some());

            drop(listener);

            let snap_closed = probe_network_snapshot_with_client(
                listening_port,
                0,
                build_http_client(Some("://invalid-proxy")),
            )
            .await;
            assert!(!snap_closed.local_port_listening);
            assert_eq!(snap_closed.firewall_rule_active, Some(false));
        });
    }

    #[test]
    fn query_public_ipv4_endpoints_enforces_overall_timeout_cap() {
        use tokio::io::AsyncReadExt;

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let stall_1 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let port_1 = stall_1.local_addr().unwrap().port();
            let stall_2 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let port_2 = stall_2.local_addr().unwrap().port();

            let hold_task = tokio::spawn(async move {
                let mut kept = Vec::new();
                for listener in [stall_1, stall_2] {
                    if let Ok(Ok((mut s, _))) =
                        tokio::time::timeout(Duration::from_millis(500), listener.accept()).await
                    {
                        let mut buf = [0u8; 256];
                        let _ = tokio::time::timeout(Duration::from_millis(50), s.read(&mut buf))
                            .await;
                        kept.push(s);
                    }
                }
                tokio::time::sleep(Duration::from_millis(600)).await;
                drop(kept);
            });

            let url_1 = format!("http://127.0.0.1:{port_1}/");
            let url_2 = format!("http://127.0.0.1:{port_2}/");
            let client = build_http_client_with_options(None, true, true).unwrap();
            let endpoints = [url_1.as_str(), url_2.as_str()];

            let started = Instant::now();
            let res = query_public_ipv4_endpoints(
                &client,
                &endpoints,
                Duration::from_secs(5),
                Duration::from_millis(350),
            )
            .await;
            assert!(res.is_none());
            assert!(started.elapsed() < Duration::from_secs(2));

            tokio::time::timeout(Duration::from_secs(3), hold_task)
                .await
                .expect("hold_task should finish cleanly")
                .unwrap();
        });
    }

    #[test]
    fn send_upnp_delete_port_mapping_posts_valid_soap_envelope() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();

            let server_task = tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await
                    .unwrap();
                req
            });

            let client = build_lan_upnp_client().unwrap();
            let control_url = format!("http://127.0.0.1:{port}/ctl/IPConn");
            let service_type = "urn:schemas-upnp-org:service:WANIPConnection:1";
            send_upnp_delete_port_mapping(&client, &control_url, service_type, 7777)
                .await
                .unwrap();

            let captured = server_task.await.unwrap();
            assert!(captured.contains("#DeletePortMapping\""));
            assert!(captured.contains("<NewExternalPort>7777</NewExternalPort>"));
            assert!(captured.contains("<NewProtocol>TCP</NewProtocol>"));
        });
    }

    async fn read_mock_http_request(sock: &mut TcpStream) -> (String, String, String) {
        use tokio::io::AsyncReadExt;

        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let header_end = loop {
            let n = sock.read(&mut tmp).await.unwrap();
            assert!(n > 0, "unexpected EOF reading HTTP headers");
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let request_line = headers.lines().next().unwrap_or_default().to_string();
        let content_len = headers
            .lines()
            .find_map(|line| {
                let (k, v) = line.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while buf.len() < header_end + content_len {
            let n = sock.read(&mut tmp).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let body = String::from_utf8_lossy(&buf[header_end..header_end + content_len]).to_string();
        (request_line, headers, body)
    }

    async fn write_mock_http_response(
        sock: &mut TcpStream,
        status_line: &str,
        content_type: &str,
        body: &str,
    ) {
        use tokio::io::AsyncWriteExt;
        let resp = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
        let _ = sock.shutdown().await;
    }

    #[test]
    fn is_wan_behind_cgnat_detects_private_rfc6598_and_double_nat_mismatch() {
        assert!(is_wan_behind_cgnat(Ipv4Addr::new(100, 64, 0, 1), None));
        assert!(is_wan_behind_cgnat(Ipv4Addr::new(10, 0, 0, 2), None));
        assert!(is_wan_behind_cgnat(Ipv4Addr::new(192, 168, 1, 2), None));
        assert!(is_wan_behind_cgnat(
            Ipv4Addr::new(203, 0, 113, 10),
            Some(Ipv4Addr::new(198, 51, 100, 77))
        ));
        assert!(!is_wan_behind_cgnat(
            Ipv4Addr::new(203, 0, 113, 10),
            Some(Ipv4Addr::new(203, 0, 113, 10))
        ));
        assert!(!is_wan_behind_cgnat(Ipv4Addr::new(203, 0, 113, 10), None));
    }

    #[test]
    fn upnp_ipv6_firewall_xml_parsers_handle_expected_variants() {
        let root_desc = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <device>
    <friendlyName>OpenWrt IGDv2</friendlyName>
    <deviceList>
      <device>
        <serviceList>
          <service>
            <serviceType>urn:schemas-upnp-org:service:WANIPConnection:2</serviceType>
            <controlURL>/ctl/IPConn</controlURL>
          </service>
          <u:service xmlns:u="urn:schemas-upnp-org:device-1-0">
            <u:serviceType>urn:schemas-upnp-org:service:WANIPv6FirewallControl:1</u:serviceType>
            <u:controlURL>/ctl/IP6FCtl</u:controlURL>
          </u:service>
        </serviceList>
      </device>
    </deviceList>
  </device>
</root>"#;
        assert_eq!(
            parse_wan_ipv6_firewall_service(root_desc),
            Some((
                "urn:schemas-upnp-org:service:WANIPv6FirewallControl:1".to_string(),
                "/ctl/IP6FCtl".to_string()
            ))
        );
        assert_eq!(
            parse_wan_ipv6_firewall_service("<root><device/></root>"),
            None
        );

        let fw_status_numeric = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:GetFirewallStatusResponse xmlns:u="urn:schemas-upnp-org:service:WANIPv6FirewallControl:1"><FirewallEnabled>1</FirewallEnabled><InboundPinholeAllowed>0</InboundPinholeAllowed></u:GetFirewallStatusResponse></s:Body></s:Envelope>"#;
        assert_eq!(
            parse_upnp_ipv6_firewall_status_xml(fw_status_numeric),
            Some((true, false))
        );

        let fw_status_bool = r#"<s:Envelope><s:Body><u:GetFirewallStatusResponse><u:FirewallEnabled>True</u:FirewallEnabled><u:InboundPinholeAllowed>FALSE</u:InboundPinholeAllowed></u:GetFirewallStatusResponse></s:Body></s:Envelope>"#;
        assert_eq!(
            parse_upnp_ipv6_firewall_status_xml(fw_status_bool),
            Some((true, false))
        );

        let fw_disabled = r#"<s:Envelope><s:Body><GetFirewallStatusResponse><FirewallEnabled>0</FirewallEnabled><InboundPinholeAllowed>0</InboundPinholeAllowed></GetFirewallStatusResponse></s:Body></s:Envelope>"#;
        assert_eq!(
            parse_upnp_ipv6_firewall_status_xml(fw_disabled),
            Some((false, false))
        );
        assert_eq!(
            parse_upnp_ipv6_firewall_status_xml("<FirewallEnabled>1</FirewallEnabled>"),
            None
        );

        let add_pinhole_resp = r#"<s:Envelope><s:Body><u:AddPinholeResponse xmlns:u="urn:schemas-upnp-org:service:WANIPv6FirewallControl:1"><UniqueID>42</UniqueID></u:AddPinholeResponse></s:Body></s:Envelope>"#;
        assert_eq!(parse_upnp_pinhole_unique_id_xml(add_pinhole_resp), Some(42));
        assert_eq!(
            parse_upnp_pinhole_unique_id_xml("<UniqueID>abc</UniqueID>"),
            None
        );
    }

    #[test]
    fn ipv6_url_rewriting_and_ssdp_candidate_collection_work() {
        let gua: Ipv6Addr = "2001:ee0::1".parse().unwrap();
        let ula: Ipv6Addr = "fd00::1".parse().unwrap();
        let ll: Ipv6Addr = "fe80::1".parse().unwrap();

        assert!(is_routable_non_link_local_ipv6(gua));
        assert!(is_routable_non_link_local_ipv6(ula));
        assert!(!is_routable_non_link_local_ipv6(ll));
        assert!(!is_routable_non_link_local_ipv6(Ipv6Addr::LOCALHOST));
        assert!(!is_routable_non_link_local_ipv6(Ipv6Addr::UNSPECIFIED));
        assert!(!is_routable_non_link_local_ipv6(
            "ff02::c".parse::<Ipv6Addr>().unwrap()
        ));

        assert_eq!(
            rewrite_http_url_ipv6_host("http://[fe80::1%25eth0]:5000/rootDesc.xml", gua),
            Some("http://[2001:ee0::1]:5000/rootDesc.xml".to_string())
        );
        assert_eq!(
            rewrite_http_url_ipv6_host("http://192.168.1.1:49152/ctl", ula),
            Some("http://[fd00::1]:49152/ctl".to_string())
        );
        assert_eq!(
            rewrite_http_url_ipv6_host("https://router.local/igd.xml?x=1", gua),
            Some("https://[2001:ee0::1]/igd.xml?x=1".to_string())
        );
        assert_eq!(rewrite_http_url_ipv6_host("ftp://192.168.1.1/a", gua), None);
        assert_eq!(rewrite_http_url_ipv6_host("http://[broken/a", gua), None);

        let fw_ssdp = "\
HTTP/1.1 200 OK\r\n\
LOCATION: http://[fe80::1]:5000/rootDesc.xml\r\n\
ST: urn:schemas-upnp-org:service:WANIPv6FirewallControl:1\r\n\r\n";
        assert_eq!(
            extract_igd_location_from_ssdp(fw_ssdp),
            Some("http://[fe80::1]:5000/rootDesc.xml".to_string())
        );

        let mut candidates = Vec::new();
        collect_ipv6_ssdp_candidate_locations(
            fw_ssdp,
            SocketAddr::V6(SocketAddrV6::new(gua, 1900, 0, 0)),
            &mut candidates,
        );
        assert_eq!(
            candidates,
            vec!["http://[2001:ee0::1]:5000/rootDesc.xml".to_string()]
        );

        let v4_loc_ssdp = "\
HTTP/1.1 200 OK\r\n\
LOCATION: http://192.168.1.1:5000/rootDesc.xml\r\n\
ST: urn:schemas-upnp-org:device:InternetGatewayDevice:2\r\n\r\n";
        let mut mixed_candidates = Vec::new();
        collect_ipv6_ssdp_candidate_locations(
            v4_loc_ssdp,
            SocketAddr::V6(SocketAddrV6::new(ula, 1900, 0, 0)),
            &mut mixed_candidates,
        );
        assert_eq!(
            mixed_candidates,
            vec![
                "http://[fd00::1]:5000/rootDesc.xml".to_string(),
                "http://192.168.1.1:5000/rootDesc.xml".to_string(),
            ]
        );

        let mut ll_sender_candidates = Vec::new();
        collect_ipv6_ssdp_candidate_locations(
            fw_ssdp,
            SocketAddr::V6(SocketAddrV6::new(ll, 1900, 0, 3)),
            &mut ll_sender_candidates,
        );
        assert!(ll_sender_candidates.is_empty());
    }

    #[test]
    fn ipv6_pinhole_soap_lifecycle_works_over_ipv6_and_ipv4_with_fallback() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            if let Ok(v6_listener) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await {
                let v6_port = v6_listener.local_addr().unwrap().port();
                let desc_xml_no_service = r#"<?xml version="1.0"?><root><device><friendlyName>Fallback IPv6 Gateway</friendlyName></device></root>"#;

                let server_task = tokio::spawn(async move {
                    let mut actions = Vec::new();
                    for _ in 0..5 {
                        let (mut sock, _) = v6_listener.accept().await.unwrap();
                        let (req_line, headers, body) = read_mock_http_request(&mut sock).await;
                        if req_line.starts_with("GET /rootDesc.xml") {
                            write_mock_http_response(&mut sock, "200 OK", "text/xml", desc_xml_no_service).await;
                            continue;
                        }
                        assert!(
                            req_line.starts_with("POST /ctl/IP6FCtl"),
                            "expected fallback path /ctl/IP6FCtl, got {req_line}"
                        );
                        if headers.contains("#GetFirewallStatus\"") {
                            actions.push("GetFirewallStatus".to_string());
                            let resp = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:GetFirewallStatusResponse xmlns:u="urn:schemas-upnp-org:service:WANIPv6FirewallControl:1"><FirewallEnabled>1</FirewallEnabled><InboundPinholeAllowed>1</InboundPinholeAllowed></u:GetFirewallStatusResponse></s:Body></s:Envelope>"#;
                            write_mock_http_response(&mut sock, "200 OK", "text/xml", resp).await;
                        } else if headers.contains("#AddPinhole\"") {
                            actions.push("AddPinhole".to_string());
                            assert!(body.contains("<InternalClient>::1</InternalClient>"));
                            assert!(body.contains("<InternalPort>7777</InternalPort>"));
                            assert!(body.contains("<Protocol>6</Protocol>"));
                            assert!(body.contains("<LeaseTime>3600</LeaseTime>"));
                            let resp = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:AddPinholeResponse xmlns:u="urn:schemas-upnp-org:service:WANIPv6FirewallControl:1"><UniqueID>88</UniqueID></u:AddPinholeResponse></s:Body></s:Envelope>"#;
                            write_mock_http_response(&mut sock, "200 OK", "text/xml", resp).await;
                        } else if headers.contains("#UpdatePinhole\"") {
                            actions.push("UpdatePinhole".to_string());
                            assert!(body.contains("<UniqueID>88</UniqueID>"));
                            assert!(body.contains("<NewLeaseTime>3600</NewLeaseTime>"));
                            write_mock_http_response(&mut sock, "200 OK", "text/xml", "<s:Envelope/>").await;
                        } else if headers.contains("#DeletePinhole\"") {
                            actions.push("DeletePinhole".to_string());
                            assert!(body.contains("<UniqueID>88</UniqueID>"));
                            write_mock_http_response(&mut sock, "200 OK", "text/xml", "<s:Envelope/>").await;
                        } else {
                            panic!("unexpected SOAP headers: {headers}");
                        }
                    }
                    actions
                });

                let v6_client = build_ipv6_upnp_client(Ipv6Addr::LOCALHOST).unwrap();
                let cancel = CancellationToken::new();
                let has_lease = AtomicBool::new(false);
                let desc_url = format!("http://[::1]:{v6_port}/rootDesc.xml");

                let (lease_opt, client_opt, status_opt) = try_ipv6_pinhole_candidates(
                    &[(v6_client, desc_url)],
                    Ipv6Addr::LOCALHOST,
                    7777,
                    &cancel,
                    &has_lease,
                )
                .await;

                assert!(has_lease.load(Ordering::SeqCst));
                assert_eq!(
                    status_opt,
                    Some(Ipv6PinholeStatus::Opened {
                        unique_id: 88,
                        gateway_label: "Fallback IPv6 Gateway".to_string(),
                    })
                );
                let lease = lease_opt.expect("expected active IPv6 pinhole lease");
                assert_eq!(lease.unique_id, 88);
                let working_client = client_opt.expect("expected working HTTP client");

                send_upnp_update_ipv6_pinhole(
                    &working_client,
                    &lease.control_url,
                    &lease.service_type,
                    lease.unique_id,
                    lease.lease_seconds,
                )
                .await
                .unwrap();

                close_ipv6_pinhole(&lease).await.unwrap();

                let actions = server_task.await.unwrap();
                assert_eq!(
                    actions,
                    vec![
                        "GetFirewallStatus",
                        "AddPinhole",
                        "UpdatePinhole",
                        "DeletePinhole"
                    ]
                );
            }

            // Test IPv4 control endpoint with explicit WANIPv6FirewallControl:1 + InboundPinholeAllowed=0 / FirewallDisabled
            let v4_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let v4_port = v4_listener.local_addr().unwrap().port();
            let v4_server = tokio::spawn(async move {
                let explicit_desc = r#"<?xml version="1.0"?><root><device><friendlyName>DualStack Router</friendlyName><serviceList><service><serviceType>urn:schemas-upnp-org:service:WANIPv6FirewallControl:1</serviceType><controlURL>/custom/v6fw</controlURL></service></serviceList></device></root>"#;
                // Case 1: FirewallDisabled (GET + GetFirewallStatus)
                for step in 0..2 {
                    let (mut sock, _) = v4_listener.accept().await.unwrap();
                    let (req_line, _, _) = read_mock_http_request(&mut sock).await;
                    if step == 0 {
                        assert!(req_line.starts_with("GET /desc.xml"));
                        write_mock_http_response(&mut sock, "200 OK", "text/xml", explicit_desc).await;
                    } else {
                        assert!(req_line.starts_with("POST /custom/v6fw"));
                        write_mock_http_response(
                            &mut sock,
                            "200 OK",
                            "text/xml",
                            r#"<GetFirewallStatusResponse><FirewallEnabled>0</FirewallEnabled><InboundPinholeAllowed>0</InboundPinholeAllowed></GetFirewallStatusResponse>"#,
                        )
                        .await;
                    }
                }
                // Case 2: InboundPinholeAllowed = 0 -> Rejected
                for step in 0..2 {
                    let (mut sock, _) = v4_listener.accept().await.unwrap();
                    let _ = read_mock_http_request(&mut sock).await;
                    if step == 0 {
                        write_mock_http_response(&mut sock, "200 OK", "text/xml", explicit_desc).await;
                    } else {
                        write_mock_http_response(
                            &mut sock,
                            "200 OK",
                            "text/xml",
                            r#"<GetFirewallStatusResponse><FirewallEnabled>1</FirewallEnabled><InboundPinholeAllowed>0</InboundPinholeAllowed></GetFirewallStatusResponse>"#,
                        )
                        .await;
                    }
                }
                // Case 3: Unsupported (no service, 404 on /ctl/IP6FCtl)
                for step in 0..2 {
                    let (mut sock, _) = v4_listener.accept().await.unwrap();
                    let _ = read_mock_http_request(&mut sock).await;
                    if step == 0 {
                        write_mock_http_response(&mut sock, "200 OK", "text/xml", "<root><device/></root>").await;
                    } else {
                        write_mock_http_response(&mut sock, "404 Not Found", "text/plain", "Not Found").await;
                    }
                }
            });

            let lan_client = build_lan_upnp_client().unwrap();
            let cancel = CancellationToken::new();
            let has_lease = AtomicBool::new(false);
            let target_v6: Ipv6Addr = "2001:ee0::42".parse().unwrap();

            let (l1, _, s1) = try_ipv6_pinhole_candidates(
                &[(lan_client.clone(), format!("http://127.0.0.1:{v4_port}/desc.xml"))],
                target_v6,
                7777,
                &cancel,
                &has_lease,
            )
            .await;
            assert!(l1.is_none());
            assert_eq!(s1, Some(Ipv6PinholeStatus::FirewallDisabled));

            let (l2, _, s2) = try_ipv6_pinhole_candidates(
                &[(lan_client.clone(), format!("http://127.0.0.1:{v4_port}/desc.xml"))],
                target_v6,
                7777,
                &cancel,
                &has_lease,
            )
            .await;
            assert!(l2.is_none());
            assert!(matches!(s2, Some(Ipv6PinholeStatus::Rejected(_))));

            let (l3, _, s3) = try_ipv6_pinhole_candidates(
                &[(lan_client, format!("http://127.0.0.1:{v4_port}/desc.xml"))],
                target_v6,
                7777,
                &cancel,
                &has_lease,
            )
            .await;
            assert!(l3.is_none());
            assert_eq!(s3, Some(Ipv6PinholeStatus::Unsupported));
            assert!(!has_lease.load(Ordering::SeqCst));

            v4_server.await.unwrap();
        });
    }

    #[test]
    fn double_nat_upnp_check_skips_add_port_mapping_and_detects_cgnat() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let upnp_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let upnp_port = upnp_listener.local_addr().unwrap().port();
            let pub_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let pub_port = pub_listener.local_addr().unwrap().port();

            let upnp_server = tokio::spawn(async move {
                let (mut sock, _) = upnp_listener.accept().await.unwrap();
                let (_, headers, _) = read_mock_http_request(&mut sock).await;
                assert!(
                    headers.contains("#GetExternalIPAddress\""),
                    "expected GetExternalIPAddress before AddPortMapping, got: {headers}"
                );
                let resp = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:GetExternalIPAddressResponse><NewExternalIPAddress>100.64.10.5</NewExternalIPAddress></u:GetExternalIPAddressResponse></s:Body></s:Envelope>"#;
                write_mock_http_response(&mut sock, "200 OK", "text/xml", resp).await;
                tokio::time::timeout(Duration::from_millis(120), upnp_listener.accept())
                    .await
                    .is_err()
            });

            let pub_server = tokio::spawn(async move {
                let (mut sock, _) = pub_listener.accept().await.unwrap();
                let _ = read_mock_http_request(&mut sock).await;
                write_mock_http_response(&mut sock, "200 OK", "text/plain", "203.0.113.88").await;
            });

            let endpoint = UpnpServiceEndpoint {
                client: build_lan_upnp_client().unwrap(),
                control_url: format!("http://127.0.0.1:{upnp_port}/ctl/IPConn"),
                service_type: "urn:schemas-upnp-org:service:WANIPConnection:1".to_string(),
                local_ip: Ipv4Addr::new(192, 168, 1, 10),
                gateway_label: "ISP Router".to_string(),
            };
            let pub_url = format!("http://127.0.0.1:{pub_port}/");
            let cancel = CancellationToken::new();
            let has_lease = AtomicBool::new(false);

            let (lease, _, status) = run_direct_host_upnp_mapping(
                endpoint,
                7777,
                &[pub_url.as_str()],
                &cancel,
                &has_lease,
            )
            .await;

            assert!(lease.is_none());
            assert!(!has_lease.load(Ordering::SeqCst));
            assert_eq!(
                status,
                Some(UpnpPortStatus::CarrierGradeNat {
                    wan_ip: Ipv4Addr::new(100, 64, 10, 5),
                    public_ip: Some(Ipv4Addr::new(203, 0, 113, 88)),
                    external_port: 7777,
                })
            );
            assert!(
                upnp_server.await.unwrap(),
                "AddPortMapping must NOT be called when CGNAT is detected"
            );
            pub_server.await.unwrap();

            // Verify fallback re-query when pre-mapping GetExternalIPAddress returns None
            let upnp_listener_2 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let upnp_port_2 = upnp_listener_2.local_addr().unwrap().port();
            let pub_listener_2 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let pub_port_2 = pub_listener_2.local_addr().unwrap().port();

            let upnp_server_2 = tokio::spawn(async move {
                // 1: initial GetExternalIPAddress -> empty
                let (mut s1, _) = upnp_listener_2.accept().await.unwrap();
                let (_, h1, _) = read_mock_http_request(&mut s1).await;
                assert!(h1.contains("#GetExternalIPAddress\""));
                write_mock_http_response(
                    &mut s1,
                    "200 OK",
                    "text/xml",
                    "<NewExternalIPAddress/>",
                )
                .await;

                // 2: AddPortMapping -> 200 OK
                let (mut s2, _) = upnp_listener_2.accept().await.unwrap();
                let (_, h2, _) = read_mock_http_request(&mut s2).await;
                assert!(h2.contains("#AddPortMapping\""));
                write_mock_http_response(&mut s2, "200 OK", "text/xml", "").await;

                // 3: post-mapping GetExternalIPAddress -> public WAN IP different from public_ip
                let (mut s3, _) = upnp_listener_2.accept().await.unwrap();
                let (_, h3, _) = read_mock_http_request(&mut s3).await;
                assert!(h3.contains("#GetExternalIPAddress\""));
                write_mock_http_response(
                    &mut s3,
                    "200 OK",
                    "text/xml",
                    "<NewExternalIPAddress>203.0.113.10</NewExternalIPAddress>",
                )
                .await;
            });

            let pub_server_2 = tokio::spawn(async move {
                let (mut sock, _) = pub_listener_2.accept().await.unwrap();
                let _ = read_mock_http_request(&mut sock).await;
                write_mock_http_response(&mut sock, "200 OK", "text/plain", "198.51.100.77").await;
            });

            let endpoint_2 = UpnpServiceEndpoint {
                client: build_lan_upnp_client().unwrap(),
                control_url: format!("http://127.0.0.1:{upnp_port_2}/ctl/IPConn"),
                service_type: "urn:schemas-upnp-org:service:WANIPConnection:1".to_string(),
                local_ip: Ipv4Addr::new(192, 168, 1, 10),
                gateway_label: "DoubleNAT Router".to_string(),
            };
            let pub_url_2 = format!("http://127.0.0.1:{pub_port_2}/");
            let has_lease_2 = AtomicBool::new(false);
            let (lease_2, _, status_2) = run_direct_host_upnp_mapping(
                endpoint_2,
                7777,
                &[pub_url_2.as_str()],
                &cancel,
                &has_lease_2,
            )
            .await;

            let lease_2 = lease_2.expect("expected lease after AddPortMapping");
            assert!(lease_2.is_cgnat);
            assert!(has_lease_2.load(Ordering::SeqCst));
            assert_eq!(
                status_2,
                Some(UpnpPortStatus::CarrierGradeNat {
                    wan_ip: Ipv4Addr::new(203, 0, 113, 10),
                    public_ip: Some(Ipv4Addr::new(198, 51, 100, 77)),
                    external_port: 7777,
                })
            );
            upnp_server_2.await.unwrap();
            pub_server_2.await.unwrap();
        });
    }

    #[test]
    fn reachability_parsers_and_mock_http_probes_work() {
        let v4_ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));
        let v6_ip = IpAddr::V6("2001:ee0::1".parse().unwrap());

        assert_eq!(
            parse_ifconfig_port_probe_response(
                r#"{"ip":"203.0.113.9","port":7777,"reachable":true}"#,
                v4_ip,
                7777
            ),
            ExternalReachability::Reachable
        );
        assert_eq!(
            parse_ifconfig_port_probe_response(
                r#"{"ip":"2001:ee0::1","port":7777,"reachable":false}"#,
                v6_ip,
                7777
            ),
            ExternalReachability::Blocked
        );
        assert!(matches!(
            parse_ifconfig_port_probe_response(
                r#"{"ip":"198.51.100.1","port":7777,"reachable":true}"#,
                v4_ip,
                7777
            ),
            ExternalReachability::CheckUnavailable(_)
        ));
        assert!(matches!(
            parse_ifconfig_port_probe_response(
                r#"{"ip":"203.0.113.9","port":8888,"reachable":true}"#,
                v4_ip,
                7777
            ),
            ExternalReachability::CheckUnavailable(_)
        ));
        assert!(matches!(
            parse_ifconfig_port_probe_response("not-json", v4_ip, 7777),
            ExternalReachability::CheckUnavailable(_)
        ));

        assert_eq!(
            parse_portchecker_io_response("True\n"),
            ExternalReachability::Reachable
        );
        assert_eq!(
            parse_portchecker_io_response("false"),
            ExternalReachability::Blocked
        );
        assert!(matches!(
            parse_portchecker_io_response("error"),
            ExternalReachability::CheckUnavailable(_)
        ));

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            // Test IPv4 primary + fallback to portchecker.io + recheck_reachability
            let ifconfig_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let ifconfig_port = ifconfig_listener.local_addr().unwrap().port();
            let pc_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let pc_port = pc_listener.local_addr().unwrap().port();

            let ifconfig_server = tokio::spawn(async move {
                // 1st call: HTTP 429 triggers portchecker fallback
                let (mut s1, _) = ifconfig_listener.accept().await.unwrap();
                let (req1, _, _) = read_mock_http_request(&mut s1).await;
                assert!(req1.starts_with("GET /port/7777"));
                write_mock_http_response(&mut s1, "429 Too Many Requests", "text/plain", "rate limit")
                    .await;

                // 2nd call (from recheck_reachability): returns reachable=true
                let (mut s2, _) = ifconfig_listener.accept().await.unwrap();
                let (req2, _, _) = read_mock_http_request(&mut s2).await;
                assert!(req2.starts_with("GET /port/7777"));
                write_mock_http_response(
                    &mut s2,
                    "200 OK",
                    "application/json",
                    r#"{"ip":"203.0.113.9","port":7777,"reachable":true}"#,
                )
                .await;
            });

            let pc_server = tokio::spawn(async move {
                let (mut s, _) = pc_listener.accept().await.unwrap();
                let (req, _, _) = read_mock_http_request(&mut s).await;
                assert!(req.starts_with("GET /api/203.0.113.9/7777"));
                write_mock_http_response(&mut s, "200 OK", "text/plain", "False").await;
            });

            let ifconfig_base = format!("http://127.0.0.1:{ifconfig_port}");
            let pc_base = format!("http://127.0.0.1:{pc_port}");
            let v4_probe_res = probe_external_ipv4_reachability_at(
                &ifconfig_base,
                &pc_base,
                Ipv4Addr::new(203, 0, 113, 9),
                7777,
            )
            .await;
            assert_eq!(v4_probe_res, ExternalReachability::Blocked);

            // Test IPv6 local mock probe on [::1]
            if let Ok(v6_probe_listener) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await {
                let v6_probe_port = v6_probe_listener.local_addr().unwrap().port();
                let v6_server = tokio::spawn(async move {
                    let (mut s, _) = v6_probe_listener.accept().await.unwrap();
                    let (req, headers, _) = read_mock_http_request(&mut s).await;
                    assert!(req.starts_with("GET /port/7777"));
                    assert!(headers.to_ascii_lowercase().contains("accept: application/json"));
                    write_mock_http_response(
                        &mut s,
                        "200 OK",
                        "application/json",
                        r#"{"ip":"::1","port":7777,"reachable":true}"#,
                    )
                    .await;
                });
                let v6_res = probe_external_ipv6_reachability_at(
                    &format!("http://[::1]:{v6_probe_port}"),
                    Ipv6Addr::LOCALHOST,
                    7777,
                )
                .await;
                assert_eq!(v6_res, ExternalReachability::Reachable);
                v6_server.await.unwrap();
            }

            // Test recheck_reachability updates status, calls wake_ui, and resets rechecking guard
            let wakes = Arc::new(AtomicUsize::new(0));
            let wakes_clone = Arc::clone(&wakes);
            let wake_ui = crate::server_manager::egui_repaint::RepaintHandle::new(Arc::new(
                move || {
                    wakes_clone.fetch_add(1, Ordering::SeqCst);
                },
            ));
            let handle = DirectPortHostHandle {
                cancel: CancellationToken::new(),
                status: Arc::new(RwLock::new(DirectHostStatus::Running(DirectHostActiveInfo {
                    local_port: 7777,
                    host_port: 7777,
                    lan_endpoints: vec![("Wi-Fi".to_string(), "192.168.1.10:7777".to_string())],
                    vpn_endpoints: Vec::new(),
                    ipv6_endpoint: None,
                    ipv6_pinhole_status: Ipv6PinholeStatus::Disabled,
                    ipv6_reachability: ExternalReachability::NotChecked,
                    upnp_status: UpnpPortStatus::Mapped {
                        external_ip: Ipv4Addr::new(203, 0, 113, 9),
                        external_port: 7777,
                        gateway_label: "Router".to_string(),
                    },
                    ipv4_reachability: ExternalReachability::Blocked,
                    bridging_tcp: false,
                    active_clients: 0,
                    p2p_status: P2pHostStatus::Disabled,
                }))),
                banned_ips: Arc::new(RwLock::new(HashSet::new())),
                banned_p2p_peers: Arc::new(RwLock::new(HashSet::new())),
                active_p2p_conns: Arc::new(Mutex::new(Vec::new())),
                bridge_peers: Arc::new(RwLock::new(HashMap::new())),
                has_upnp_lease: Arc::new(AtomicBool::new(false)),
                has_p2p_endpoint: Arc::new(AtomicBool::new(false)),
                cleanup_done: Arc::new((Mutex::new(true), Condvar::new())),
                rechecking: Arc::new(AtomicBool::new(false)),
            };

            handle.recheck_reachability_with_bases(
                &tokio::runtime::Handle::current(),
                wake_ui.clone(),
                &ifconfig_base,
                &pc_base,
            );
            // Concurrent duplicate call must be ignored while rechecking is true
            handle.recheck_reachability_with_bases(
                &tokio::runtime::Handle::current(),
                wake_ui,
                &ifconfig_base,
                &pc_base,
            );

            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let DirectHostStatus::Running(info) = handle.current_status() {
                    if info.ipv4_reachability == ExternalReachability::Reachable
                        && !handle.rechecking.load(Ordering::SeqCst)
                    {
                        break;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "recheck_reachability did not complete"
                );
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            assert!(wakes.load(Ordering::SeqCst) >= 2);

            ifconfig_server.await.unwrap();
            pc_server.await.unwrap();
        });
    }

    #[test]
    fn direct_host_active_info_endpoint_and_progress_matrix() {
        let mut info = DirectHostActiveInfo {
            local_port: 7777,
            host_port: 7777,
            lan_endpoints: vec![("Ethernet".to_string(), "192.168.1.50:7777".to_string())],
            vpn_endpoints: vec![("Tailscale".to_string(), "100.64.1.2:7777".to_string())],
            ipv6_endpoint: Some("[2001:ee0::1]:7777".to_string()),
            ipv6_pinhole_status: Ipv6PinholeStatus::Opened {
                unique_id: 7,
                gateway_label: "Router".to_string(),
            },
            ipv6_reachability: ExternalReachability::Reachable,
            upnp_status: UpnpPortStatus::Mapped {
                external_ip: Ipv4Addr::new(203, 0, 113, 9),
                external_port: 7777,
                gateway_label: "Router".to_string(),
            },
            ipv4_reachability: ExternalReachability::Reachable,
            bridging_tcp: false,
            active_clients: 0,
            p2p_status: P2pHostStatus::Disabled,
        };

        assert!(!info.is_online_setup_in_progress());
        assert_eq!(
            info.preferred_online_endpoint(),
            Some("203.0.113.9:7777".to_string())
        );

        info.p2p_status = P2pHostStatus::Starting;
        assert!(info.is_online_setup_in_progress());
        info.p2p_status = P2pHostStatus::Running {
            ticket: "TLPORT:TESTTICKET".to_string(),
            endpoint_id: "0123".repeat(16),
            relay_url: Some("https://aps1-1.relay.n0.iroh.link/".to_string()),
        };
        assert!(!info.is_online_setup_in_progress());
        assert_eq!(
            info.preferred_online_endpoint(),
            Some("TLPORT:TESTTICKET".to_string())
        );
        info.p2p_status = P2pHostStatus::Disabled;
        assert_eq!(
            info.candidate_wan_endpoint(),
            Some((
                "203.0.113.9:7777".to_string(),
                &ExternalReachability::Reachable
            ))
        );

        info.ipv4_reachability = ExternalReachability::Blocked;
        assert_eq!(
            info.preferred_online_endpoint(),
            Some("[2001:ee0::1]:7777".to_string())
        );

        info.ipv6_reachability = ExternalReachability::Blocked;
        assert_eq!(
            info.preferred_online_endpoint(),
            Some("100.64.1.2:7777".to_string())
        );

        info.upnp_status = UpnpPortStatus::CarrierGradeNat {
            wan_ip: Ipv4Addr::new(100, 64, 0, 1),
            public_ip: Some(Ipv4Addr::new(203, 0, 113, 9)),
            external_port: 7777,
        };
        assert_eq!(
            info.candidate_wan_endpoint(),
            Some((
                "[2001:ee0::1]:7777".to_string(),
                &ExternalReachability::Blocked
            ))
        );

        info.ipv6_reachability = ExternalReachability::Reachable;
        assert_eq!(
            info.preferred_online_endpoint(),
            Some("[2001:ee0::1]:7777".to_string())
        );

        info.ipv6_reachability = ExternalReachability::Checking;
        assert!(info.is_online_setup_in_progress());
        info.ipv6_reachability = ExternalReachability::NotChecked;
        info.ipv6_pinhole_status = Ipv6PinholeStatus::Opening;
        assert!(info.is_online_setup_in_progress());
    }
}
