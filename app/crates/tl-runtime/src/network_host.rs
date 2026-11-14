use crate::game_runner::run_elevated_windows;
use crate::http_client::{build_http_client, ensure_rustls_provider, LAUNCHER_USER_AGENT};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, UdpSocket as StdUdpSocket,
};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
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
    pub upnp_status: UpnpPortStatus,
    pub bridging_tcp: bool,
    pub active_clients: usize,
}

impl DirectHostActiveInfo {
    pub fn primary_lan_endpoint(&self) -> Option<&str> {
        self.lan_endpoints.first().map(|(_, ep)| ep.as_str())
    }

    pub fn preferred_online_endpoint(&self) -> Option<String> {
        if let UpnpPortStatus::Mapped {
            external_ip,
            external_port,
            ..
        } = &self.upnp_status
        {
            return Some(format!("{external_ip}:{external_port}"));
        }
        self.vpn_endpoints.first().map(|(_, ep)| ep.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirectHostStatus {
    Idle,
    Starting { local_port: u16, host_port: u16 },
    Running(DirectHostActiveInfo),
    Stopped,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct DirectHostConfig {
    pub local_target_port: u16,
    pub host_port: u16,
    pub bind_ipv4: Option<Ipv4Addr>,
    pub try_upnp: bool,
}

#[derive(Clone)]
pub struct DirectPortHostHandle {
    cancel: CancellationToken,
    status: Arc<RwLock<DirectHostStatus>>,
}

impl DirectPortHostHandle {
    pub fn stop(&self) {
        self.cancel.cancel();
    }

    pub fn current_status(&self) -> DirectHostStatus {
        self.status
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[derive(Debug)]
pub struct LocalClientBridgeHandle {
    cancel: CancellationToken,
    local_port: u16,
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
        if host.is_empty() || host.parse::<Ipv6Addr>().is_err() {
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

async fn query_public_ipv4_with_client(
    client: &reqwest::Client,
    per_req_timeout: Duration,
) -> Option<Ipv4Addr> {
    for endpoint in [
        "https://api.ipify.org",
        "https://ipv4.icanhazip.com",
        "https://checkip.amazonaws.com",
    ] {
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
}

pub async fn probe_network_snapshot(port: u16, proxy_url: Option<&str>) -> NetworkSnapshot {
    let (primary_lan_ip, interfaces) = collect_local_interfaces();
    let global_ipv6 = detect_primary_global_ipv6();
    let has_vpn_or_virtual = interfaces.iter().any(|i| i.is_vpn);
    let firewall_rule_active = Some(check_firewall_rule_exists(port));
    let local_port_listening = probe_local_tcp_listening(port).await;

    let mut public_ip = None;
    let mut public_ip_error = None;

    match build_http_client(proxy_url) {
        Ok(client) => {
            public_ip = query_public_ipv4_with_client(&client, Duration::from_secs(6)).await;
            if public_ip.is_none() {
                public_ip_error = Some("Không lấy được IP Public từ internet".to_string());
            }
        }
        Err(e) => {
            public_ip_error = Some(e.to_string());
        }
    }

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

fn push_unique_ipv4(target: &mut Vec<Ipv4Addr>, ip: Ipv4Addr) {
    if !target.contains(&ip) {
        target.push(ip);
    }
}

async fn open_upnp_port_mapping_on(
    internal_port: u16,
    external_port: u16,
    preferred_local_ip: Option<Ipv4Addr>,
) -> LauncherResult<(UpnpLeaseInfo, String)> {
    if internal_port == 0 || external_port == 0 {
        return Err(LauncherError::InvalidInput(
            "Cổng server không hợp lệ (port = 0)".to_string(),
        ));
    }

    let mut non_virtual_candidates = Vec::new();
    let mut fallback_candidates = Vec::new();

    if let Some(pinned) = preferred_local_ip {
        push_unique_ipv4(&mut non_virtual_candidates, pinned);
    } else {
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
    }

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

    let add_body = format!(
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
      <NewLeaseDuration>0</NewLeaseDuration>
    </u:AddPortMapping>
  </s:Body>
</s:Envelope>"#
    );

    let soap_resp = client
        .post(&control_url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header("SOAPAction", format!("\"{service_type}#AddPortMapping\""))
        .body(add_body)
        .send()
        .await
        .map_err(|e| LauncherError::Network(format!("Lỗi gửi UPnP AddPortMapping: {e}")))?;

    if !soap_resp.status().is_success() {
        let err_body = soap_resp.text().await.unwrap_or_default();
        return Err(LauncherError::Network(format_upnp_soap_rejection(&err_body)));
    }

    let external_ip = query_upnp_external_ip(&client, &control_url, &service_type).await;
    let is_cgnat = external_ip.map(is_cgnat_or_private_ipv4).unwrap_or(false);

    Ok((
        UpnpLeaseInfo {
            control_url,
            service_type,
            external_ip,
            local_ip,
            port: external_port,
            is_cgnat,
        },
        gateway_label,
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

pub async fn close_upnp_port(lease: &UpnpLeaseInfo) -> LauncherResult<()> {
    if lease.port == 0 {
        return Err(LauncherError::InvalidInput(
            "Cổng UPnP không hợp lệ (port = 0)".to_string(),
        ));
    }
    let client = build_lan_upnp_client()?;
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
        lease.service_type, lease.port
    );

    let resp = client
        .post(&lease.control_url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header(
            "SOAPAction",
            format!("\"{}#DeletePortMapping\"", lease.service_type),
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

pub(crate) fn parse_wan_control_service(xml: &str) -> Option<(String, String)> {
    let mut reader = Reader::from_str(xml);
    let mut in_service = false;
    let mut cur_tag = ServiceTag::None;
    let mut cur_type = String::new();
    let mut cur_ctrl = String::new();
    let mut best_ip_conn: Option<(String, String)> = None;
    let mut best_ppp_conn: Option<(String, String)> = None;

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
                        if st.contains("WANIPConnection") && best_ip_conn.is_none() {
                            best_ip_conn = Some((st.to_string(), ctrl.to_string()));
                        } else if st.contains("WANPPPConnection") && best_ppp_conn.is_none() {
                            best_ppp_conn = Some((st.to_string(), ctrl.to_string()));
                        }
                    }
                }
                cur_tag = ServiceTag::None;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    best_ip_conn.or(best_ppp_conn)
}

pub(crate) fn parse_igd_friendly_name(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    let mut in_friendly = false;
    let mut buf = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if e.local_name()
                    .as_ref()
                    .eq_ignore_ascii_case("friendlyName")
                {
                    in_friendly = true;
                    buf.clear();
                }
            }
            Ok(Event::Empty(e)) => {
                if e.local_name()
                    .as_ref()
                    .eq_ignore_ascii_case("friendlyName")
                {
                    in_friendly = false;
                    buf.clear();
                }
            }
            Ok(Event::Text(t)) if in_friendly => {
                buf.push_str(t.as_ref());
            }
            Ok(Event::End(e)) => {
                if e.local_name()
                    .as_ref()
                    .eq_ignore_ascii_case("friendlyName")
                    && in_friendly
                {
                    in_friendly = false;
                    let trimmed = buf.trim();
                    if !trimmed.is_empty() {
                        return trimmed.to_string();
                    }
                    buf.clear();
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    "IGD Router".to_string()
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
    let mut reader = Reader::from_str(xml);
    let mut in_ext_ip = false;
    let mut buf = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if e.local_name()
                    .as_ref()
                    .eq_ignore_ascii_case("NewExternalIPAddress")
                {
                    in_ext_ip = true;
                    buf.clear();
                }
            }
            Ok(Event::Empty(e)) => {
                if e.local_name()
                    .as_ref()
                    .eq_ignore_ascii_case("NewExternalIPAddress")
                {
                    in_ext_ip = false;
                    buf.clear();
                }
            }
            Ok(Event::Text(t)) if in_ext_ip => {
                buf.push_str(t.as_ref());
            }
            Ok(Event::End(e)) => {
                if e.local_name()
                    .as_ref()
                    .eq_ignore_ascii_case("NewExternalIPAddress")
                    && in_ext_ip
                {
                    in_ext_ip = false;
                    if let Ok(ip) = buf.trim().parse::<Ipv4Addr>() {
                        return Some(ip);
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
    let resp = client
        .post(control_url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header(
            "SOAPAction",
            format!("\"{service_type}#GetExternalIPAddress\""),
        )
        .body(body)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let xml = resp.text().await.ok()?;
    parse_upnp_external_ip_xml(&xml)
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

struct ClientGuard {
    counter: Arc<AtomicUsize>,
    status: Arc<RwLock<DirectHostStatus>>,
    wake_ui: crate::server_manager::egui_repaint::RepaintHandle,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        let next = self
            .counter
            .fetch_sub(1, Ordering::SeqCst)
            .saturating_sub(1);
        update_active_client_count(&self.status, &self.wake_ui, next);
    }
}

fn update_active_client_count(
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

fn spawn_tcp_bridge_worker(
    listener: TcpListener,
    local_target_port: u16,
    cancel: CancellationToken,
    active_clients: Arc<AtomicUsize>,
    status: Arc<RwLock<DirectHostStatus>>,
    wake_ui: crate::server_manager::egui_repaint::RepaintHandle,
) {
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                _ = cancel.cancelled() => break,
                res = listener.accept() => res,
            };
            let Ok((mut inbound, _)) = accepted else {
                continue;
            };
            let _ = inbound.set_nodelay(true);
            let child_cancel = cancel.clone();
            let counter = Arc::clone(&active_clients);
            let st_ref = Arc::clone(&status);
            let wake_ref = wake_ui.clone();

            tokio::spawn(async move {
                let target_v4 =
                    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, local_target_port));
                let Ok(Ok(mut outbound)) =
                    timeout(Duration::from_secs(3), TcpStream::connect(target_v4)).await
                else {
                    return;
                };
                let _ = outbound.set_nodelay(true);

                let now_count = counter.fetch_add(1, Ordering::SeqCst) + 1;
                update_active_client_count(&st_ref, &wake_ref, now_count);
                let _guard = ClientGuard {
                    counter,
                    status: st_ref,
                    wake_ui: wake_ref,
                };

                tokio::select! {
                    _ = child_cancel.cancelled() => {}
                    _ = copy_bidirectional(&mut inbound, &mut outbound) => {}
                }
            });
        }
    });
}

fn clean_launcher_error_msg(err: LauncherError) -> String {
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
    if config.local_target_port == 0 || config.host_port == 0 {
        let status = Arc::new(RwLock::new(DirectHostStatus::Error(
            "Cổng server hoặc cổng host không hợp lệ (port = 0)".to_string(),
        )));
        wake_ui.wake();
        return DirectPortHostHandle { cancel, status };
    }

    let status = Arc::new(RwLock::new(DirectHostStatus::Starting {
        local_port: config.local_target_port,
        host_port: config.host_port,
    }));
    let handle = DirectPortHostHandle {
        cancel: cancel.clone(),
        status: Arc::clone(&status),
    };

    rt.spawn(async move {
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
            spawn_tcp_bridge_worker(
                v4_listener,
                config.local_target_port,
                cancel.clone(),
                Arc::clone(&active_clients),
                Arc::clone(&status),
                wake_ui.clone(),
            );

            if config.bind_ipv4.is_none() {
                if let Ok(v6_listener) = bind_tcp_v6_only_listener(config.host_port) {
                    ipv6_ready = true;
                    spawn_tcp_bridge_worker(
                        v6_listener,
                        config.local_target_port,
                        cancel.clone(),
                        Arc::clone(&active_clients),
                        Arc::clone(&status),
                        wake_ui.clone(),
                    );
                }
            }
        } else {
            if let Ok(v4_listener) = bind_tcp_v4_listener(bind_ip, config.host_port) {
                bridging_tcp = true;
                spawn_tcp_bridge_worker(
                    v4_listener,
                    config.local_target_port,
                    cancel.clone(),
                    Arc::clone(&active_clients),
                    Arc::clone(&status),
                    wake_ui.clone(),
                );
            } else if let Some(check_ip) = config.bind_ipv4.or(primary_lan_ip) {
                let lan_sock = SocketAddr::V4(SocketAddrV4::new(check_ip, config.host_port));
                if !is_local_tcp_socket_listening(lan_sock, Duration::from_millis(500)).await {
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
                            spawn_tcp_bridge_worker(
                                nic_listener,
                                config.local_target_port,
                                cancel.clone(),
                                Arc::clone(&active_clients),
                                Arc::clone(&status),
                                wake_ui.clone(),
                            );
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
                        spawn_tcp_bridge_worker(
                            v6_listener,
                            config.local_target_port,
                            cancel.clone(),
                            Arc::clone(&active_clients),
                            Arc::clone(&status),
                            wake_ui.clone(),
                        );
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

        let initial_upnp = if config.try_upnp {
            UpnpPortStatus::Mapping
        } else {
            UpnpPortStatus::Disabled
        };

        let running_info = DirectHostActiveInfo {
            local_port: config.local_target_port,
            host_port: config.host_port,
            lan_endpoints,
            vpn_endpoints,
            ipv6_endpoint,
            upnp_status: initial_upnp,
            bridging_tcp,
            active_clients: active_clients.load(Ordering::SeqCst),
        };

        {
            let mut guard = status.write().unwrap_or_else(|e| e.into_inner());
            *guard = DirectHostStatus::Running(running_info);
        }
        wake_ui.wake();

        let mut active_lease: Option<UpnpLeaseInfo> = None;

        if config.try_upnp {
            let upnp_fut = async {
                match config.bind_ipv4 {
                    Some(pinned) => {
                        open_upnp_port_mapping_on(config.host_port, config.host_port, Some(pinned))
                            .await
                    }
                    None => open_upnp_port_mapping(config.host_port, config.host_port).await,
                }
            };

            let upnp_outcome = tokio::select! {
                _ = cancel.cancelled() => None,
                res = upnp_fut => Some(res),
            };

            if let Some(res) = upnp_outcome {
                let resolved_upnp = match res {
                    Ok((lease, gateway_label)) => {
                        let ext_port = lease.port;
                        let is_cgnat = lease.is_cgnat;
                        let ext_ip = lease.external_ip;
                        active_lease = Some(lease);

                        if is_cgnat {
                            let wan_ip = ext_ip.unwrap_or(Ipv4Addr::UNSPECIFIED);
                            UpnpPortStatus::CarrierGradeNat {
                                wan_ip,
                                external_port: ext_port,
                            }
                        } else if let Some(ip) = ext_ip {
                            UpnpPortStatus::Mapped {
                                external_ip: ip,
                                external_port: ext_port,
                                gateway_label,
                            }
                        } else {
                            let fallback_pub = match build_http_client(None) {
                                Ok(client) => {
                                    query_public_ipv4_with_client(
                                        &client,
                                        Duration::from_millis(2500),
                                    )
                                    .await
                                }
                                Err(_) => None,
                            };
                            if let Some(pub_ip) = fallback_pub {
                                if is_cgnat_or_private_ipv4(pub_ip) {
                                    UpnpPortStatus::CarrierGradeNat {
                                        wan_ip: pub_ip,
                                        external_port: ext_port,
                                    }
                                } else {
                                    UpnpPortStatus::Mapped {
                                        external_ip: pub_ip,
                                        external_port: ext_port,
                                        gateway_label,
                                    }
                                }
                            } else {
                                UpnpPortStatus::Unavailable(
                                    "Đã mở cổng UPnP nhưng không xác định được IP WAN công cộng"
                                        .to_string(),
                                )
                            }
                        }
                    }
                    Err(err) => UpnpPortStatus::Unavailable(clean_launcher_error_msg(err)),
                };

                let mut updated = false;
                {
                    let mut guard = status.write().unwrap_or_else(|e| e.into_inner());
                    if let DirectHostStatus::Running(ref mut info) = *guard {
                        info.upnp_status = resolved_upnp;
                        updated = true;
                    }
                }
                if updated {
                    wake_ui.wake();
                }
            }
        }

        cancel.cancelled().await;

        if let Some(lease) = active_lease {
            let _ = close_upnp_port(&lease).await;
        }

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

pub async fn resolve_ipv6_bridge_target(host: &str, port: u16) -> Option<Ipv6Addr> {
    let trimmed = host.trim();
    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed)
        .trim();
    let before_zone = unbracketed.split('%').next().unwrap_or("").trim();
    let clean = before_zone
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(before_zone)
        .trim();
    if clean.is_empty() {
        return None;
    }
    if let Ok(v6) = clean.parse::<Ipv6Addr>() {
        return Some(v6);
    }
    if clean.parse::<Ipv4Addr>().is_ok() || clean.eq_ignore_ascii_case("localhost") {
        return None;
    }

    let lookup_port = if port > 0 { port } else { 7777 };
    let Ok(Ok(addrs)) = timeout(
        Duration::from_millis(1500),
        tokio::net::lookup_host((clean, lookup_port)),
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
        SocketAddr::V6(v6) => Some(*v6.ip()),
        SocketAddr::V4(_) => None,
    })
}

pub async fn start_ipv4_to_ipv6_client_bridge(
    target_ip: Ipv6Addr,
    target_port: u16,
) -> LauncherResult<LocalClientBridgeHandle> {
    if target_port == 0 {
        return Err(LauncherError::InvalidInput(
            "Cổng đích IPv6 không hợp lệ (port = 0)".to_string(),
        ));
    }

    let remote = SocketAddr::V6(SocketAddrV6::new(target_ip, target_port, 0, 0));
    if target_ip.is_loopback() || detect_primary_global_ipv6() == Some(target_ip) {
        if !is_local_tcp_socket_listening(remote, Duration::from_millis(400)).await {
            return Err(LauncherError::Network(format!(
                "Không kết nối được tới [{target_ip}]:{target_port} — cổng IPv6 trên máy này chưa mở hoặc Server chưa chạy."
            )));
        }
    } else {
        match timeout(Duration::from_millis(2500), TcpStream::connect(remote)).await {
            Ok(Ok(probe_stream)) => {
                drop(probe_stream);
            }
            Ok(Err(e)) => {
                return Err(LauncherError::Network(format!(
                    "Không kết nối được tới [{target_ip}]:{target_port} — cổng phía host đang đóng hoặc bị tường lửa IPv6 của modem/router chặn ({e})."
                )));
            }
            Err(_) => {
                return Err(LauncherError::Network(format!(
                    "Hết thời gian chờ kết nối tới [{target_ip}]:{target_port} — tường lửa IPv6 trên modem/router có thể đang chặn hoặc host chưa mở cổng."
                )));
            }
        }
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
                continue;
            };
            let _ = inbound.set_nodelay(true);
            let child_cancel = task_cancel.clone();
            tokio::spawn(async move {
                let Ok(Ok(mut outbound)) =
                    timeout(Duration::from_millis(2500), TcpStream::connect(remote)).await
                else {
                    return;
                };
                let _ = outbound.set_nodelay(true);
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
                },
                crate::server_manager::egui_repaint::RepaintHandle::default(),
            );
            assert!(matches!(
                invalid.current_status(),
                DirectHostStatus::Error(_)
            ));

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
            upnp_status: UpnpPortStatus::Mapped {
                external_ip: Ipv4Addr::new(203, 0, 113, 9),
                external_port: 7777,
                gateway_label: "Router".to_string(),
            },
            bridging_tcp: true,
            active_clients: 0,
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
                start_ipv4_to_ipv6_client_bridge(Ipv6Addr::LOCALHOST, 0).await,
                Err(LauncherError::InvalidInput(_))
            ));

            let free_v6 = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap();
            let closed_v6_port = free_v6.local_addr().unwrap().port();
            drop(free_v6);
            assert!(matches!(
                start_ipv4_to_ipv6_client_bridge(Ipv6Addr::LOCALHOST, closed_v6_port).await,
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

            let bridge = start_ipv4_to_ipv6_client_bridge(Ipv6Addr::LOCALHOST, v6_port)
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
                let bridge = start_ipv4_to_ipv6_client_bridge(Ipv6Addr::LOCALHOST, v6_port)
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
            assert_eq!(
                resolve_ipv6_bridge_target("2405:4802:1d52::1", 7777).await,
                Some(expected)
            );
            assert_eq!(
                resolve_ipv6_bridge_target("  [2405:4802:1d52::1]  ", 7777).await,
                Some(expected)
            );
            assert_eq!(
                resolve_ipv6_bridge_target("[2405:4802:1d52::1%12]", 7777).await,
                Some(expected)
            );
            assert_eq!(
                resolve_ipv6_bridge_target("2405:4802:1d52::1%12", 0).await,
                Some(expected)
            );

            assert_eq!(resolve_ipv6_bridge_target("", 7777).await, None);
            assert_eq!(resolve_ipv6_bridge_target("[]", 7777).await, None);
            assert_eq!(resolve_ipv6_bridge_target("127.0.0.1", 7777).await, None);
            assert_eq!(resolve_ipv6_bridge_target("192.168.1.25", 7777).await, None);
            assert_eq!(resolve_ipv6_bridge_target("localhost", 7777).await, None);
            assert_eq!(resolve_ipv6_bridge_target("  LOCALHOST  ", 7777).await, None);
        });
    }
}
