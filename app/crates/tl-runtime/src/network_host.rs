use crate::game_runner::run_elevated_windows;
use crate::http_client::build_http_client;
use bore_cli::client::Client as BoreClient;
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4, UdpSocket as StdUdpSocket};
use std::process::Command;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use sysinfo::Networks;
use tl_core::{LauncherError, LauncherResult};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SSDP_MULTICAST: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalInterfaceInfo {
    pub name: String,
    pub ipv4: Ipv4Addr,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSnapshot {
    pub primary_lan_ip: Option<Ipv4Addr>,
    pub interfaces: Vec<LocalInterfaceInfo>,
    pub public_ip: Option<Ipv4Addr>,
    pub public_ip_error: Option<String>,
    pub firewall_rule_active: Option<bool>,
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
pub enum TunnelStatus {
    Idle,
    Connecting,
    Running {
        remote_host: String,
        remote_port: u16,
    },
    Stopped,
    Error(String),
}

impl TunnelStatus {
    pub fn endpoint(&self) -> Option<String> {
        match self {
            Self::Running {
                remote_host,
                remote_port,
            } => Some(format!("{remote_host}:{remote_port}")),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct BoreTunnelHandle {
    cancel: CancellationToken,
    status: Arc<RwLock<TunnelStatus>>,
}

impl BoreTunnelHandle {
    pub fn stop(&self) {
        self.cancel.cancel();
    }

    pub fn current_status(&self) -> TunnelStatus {
        self.status
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

pub fn is_cgnat_or_private_ipv4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || (o[0] == 100 && (o[1] & 0b1100_0000) == 64)
}

pub fn detect_primary_lan_ipv4() -> Option<Ipv4Addr> {
    let sock = StdUdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
        _ => None,
    }
}

pub fn collect_local_interfaces() -> (Option<Ipv4Addr>, Vec<LocalInterfaceInfo>) {
    let primary = detect_primary_lan_ipv4();
    let Ok(networks) = Networks::new_with_refreshed_list() else {
        return (primary, Vec::new());
    };
    let mut list = Vec::new();

    for (iface_name, data) in &networks {
        for ip_net in data.ip_networks() {
            if let IpAddr::V4(v4) = ip_net.addr {
                if v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() {
                    continue;
                }
                list.push(LocalInterfaceInfo {
                    name: iface_name.to_string_lossy().into_owned(),
                    ipv4: v4,
                    is_primary: Some(v4) == primary,
                });
            }
        }
    }

    list.sort_by(|a, b| b.is_primary.cmp(&a.is_primary).then(a.name.cmp(&b.name)));
    (primary, list)
}

pub async fn probe_network_snapshot(port: u16) -> NetworkSnapshot {
    let (primary_lan_ip, interfaces) = collect_local_interfaces();
    let firewall_rule_active = Some(check_firewall_rule_exists(port));

    let mut public_ip = None;
    let mut public_ip_error = None;

    match build_http_client(None) {
        Ok(client) => {
            for endpoint in ["https://api.ipify.org", "https://ipv4.icanhazip.com"] {
                if let Ok(resp) = client.get(endpoint).send().await {
                    if resp.status().is_success() {
                        if let Ok(text) = resp.text().await {
                            if let Ok(parsed) = text.trim().parse::<Ipv4Addr>() {
                                public_ip = Some(parsed);
                                break;
                            }
                        }
                    }
                }
            }
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
        public_ip,
        public_ip_error,
        firewall_rule_active,
    }
}

pub fn firewall_rule_name(port: u16) -> String {
    format!("TerrariaLauncher_TCP_{port}")
}

pub fn check_firewall_rule_exists(port: u16) -> bool {
    let name = firewall_rule_name(port);
    let mut cmd = Command::new("netsh");
    cmd.args([
        "advfirewall",
        "firewall",
        "show",
        "rule",
        &format!("name={name}"),
    ]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

pub fn ensure_firewall_rule(port: u16) -> LauncherResult<String> {
    let name = firewall_rule_name(port);
    if check_firewall_rule_exists(port) {
        return Ok(format!("Quy tắc Firewall {name} đã tồn tại"));
    }

    let args = [
        "advfirewall",
        "firewall",
        "add",
        "rule",
        &format!("name={name}"),
        "dir=in",
        "action=allow",
        "protocol=TCP",
        &format!("localport={port}"),
    ];

    let mut cmd = Command::new("netsh");
    cmd.args(args);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let out = cmd
        .output()
        .map_err(|e| LauncherError::Process(format!("Không chạy được netsh: {e}")))?;

    if out.status.success() {
        return Ok(format!("Đã mở Firewall TCP cổng {port}"));
    }

    let joined_args = args.join(" ");
    run_elevated_windows("netsh.exe", &joined_args)?;
    Ok(format!("Đã gửi yêu cầu mở Firewall TCP cổng {port} (UAC)"))
}

pub fn remove_firewall_rule(port: u16) -> LauncherResult<String> {
    let name = firewall_rule_name(port);
    let args = [
        "advfirewall",
        "firewall",
        "delete",
        "rule",
        &format!("name={name}"),
    ];
    let mut cmd = Command::new("netsh");
    cmd.args(args);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let out = cmd
        .output()
        .map_err(|e| LauncherError::Process(format!("Không chạy được netsh: {e}")))?;
    if out.status.success() {
        return Ok(format!("Đã xóa quy tắc Firewall {name}"));
    }

    run_elevated_windows("netsh.exe", &args.join(" "))?;
    Ok(format!("Đã gửi yêu cầu xóa quy tắc Firewall {name}"))
}

pub async fn open_upnp_port(port: u16) -> LauncherResult<UpnpLeaseInfo> {
    let local_ip = detect_primary_lan_ipv4().ok_or_else(|| {
        LauncherError::Network("Không xác định được địa chỉ LAN IPv4 để mở UPnP".to_string())
    })?;

    let location_url = discover_igd_location(local_ip).await?;
    let client = build_http_client(None)?;
    let desc_xml = client
        .get(&location_url)
        .send()
        .await
        .map_err(|e| LauncherError::Network(format!("Lỗi tải mô tả UPnP router: {e}")))?
        .text()
        .await
        .map_err(|e| LauncherError::Network(format!("Lỗi đọc XML router: {e}")))?;

    let (service_type, control_path) = parse_wan_control_service(&desc_xml).ok_or_else(|| {
        LauncherError::NotFound("Router không cung cấp dịch vụ WANIPConnection/WANPPPConnection".to_string())
    })?;

    let control_url = resolve_upnp_url(&location_url, &control_path);

    let add_body = format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:AddPortMapping xmlns:u="{service_type}">
      <NewRemoteHost></NewRemoteHost>
      <NewExternalPort>{port}</NewExternalPort>
      <NewProtocol>TCP</NewProtocol>
      <NewInternalPort>{port}</NewInternalPort>
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
        return Err(LauncherError::Network(format!(
            "Router từ chối UPnP AddPortMapping: {err_body}"
        )));
    }

    let external_ip = query_upnp_external_ip(&client, &control_url, &service_type).await;
    let is_cgnat = external_ip.map(is_cgnat_or_private_ipv4).unwrap_or(false);

    Ok(UpnpLeaseInfo {
        control_url,
        service_type,
        external_ip,
        local_ip,
        port,
        is_cgnat,
    })
}

pub async fn close_upnp_port(lease: &UpnpLeaseInfo) -> LauncherResult<()> {
    let client = build_http_client(None)?;
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

    let _ = client
        .post(&lease.control_url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header(
            "SOAPAction",
            format!("\"{}#DeletePortMapping\"", lease.service_type),
        )
        .body(del_body)
        .send()
        .await;
    Ok(())
}

async fn discover_igd_location(local_ip: Ipv4Addr) -> LauncherResult<String> {
    let bind_addr = SocketAddrV4::new(local_ip, 0);
    let socket = UdpSocket::bind(bind_addr)
        .await
        .map_err(|e| LauncherError::Network(format!("Không thể tạo SSDP socket: {e}")))?;

    for target in [
        "urn:schemas-upnp-org:device:InternetGatewayDevice:1",
        "urn:schemas-upnp-org:service:WANIPConnection:1",
        "urn:schemas-upnp-org:service:WANPPPConnection:1",
    ] {
        let req = format!(
            "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {target}\r\n\r\n"
        );
        let _ = socket.send_to(req.as_bytes(), SSDP_MULTICAST).await;
    }

    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let rem = deadline.saturating_duration_since(Instant::now());
        match timeout(rem, socket.recv_from(&mut buf)).await {
            Ok(Ok((len, _))) => {
                let text = String::from_utf8_lossy(&buf[..len]);
                for line in text.lines() {
                    if let Some((k, v)) = line.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("LOCATION") {
                            return Ok(v.trim().to_string());
                        }
                    }
                }
            }
            _ => break,
        }
    }

    Err(LauncherError::NotFound(
        "Không tìm thấy Router hỗ trợ UPnP trong mạng LAN".to_string(),
    ))
}

fn parse_wan_control_service(xml: &str) -> Option<(String, String)> {
    let mut reader = Reader::from_str(xml);
    let mut in_service = false;
    let mut cur_tag = String::new();
    let mut cur_type = String::new();
    let mut cur_ctrl = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_string();
                if name.ends_with("service") {
                    in_service = true;
                    cur_type.clear();
                    cur_ctrl.clear();
                } else if in_service {
                    cur_tag = name;
                }
            }
            Ok(Event::Text(t)) if in_service => {
                let txt = t.as_ref().trim().to_string();
                if cur_tag.ends_with("serviceType") {
                    cur_type = txt;
                } else if cur_tag.ends_with("controlURL") {
                    cur_ctrl = txt;
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name().as_ref().to_string();
                if name.ends_with("service") && in_service {
                    in_service = false;
                    if (cur_type.contains("WANIPConnection") || cur_type.contains("WANPPPConnection"))
                        && !cur_ctrl.is_empty()
                    {
                        return Some((cur_type, cur_ctrl));
                    }
                }
                cur_tag.clear();
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    None
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
        return format!("{origin}/{control_path}");
    }
    control_path.to_string()
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
    let xml = resp.text().await.ok()?;
    let start = xml.find("<NewExternalIPAddress>")? + "<NewExternalIPAddress>".len();
    let end = xml[start..].find("</NewExternalIPAddress>")? + start;
    xml[start..end].trim().parse::<Ipv4Addr>().ok()
}

pub fn start_bore_tunnel(
    rt: &tokio::runtime::Handle,
    local_port: u16,
    bore_server: String,
    secret: Option<String>,
    wake_ui: crate::server_manager::egui_repaint::RepaintHandle,
) -> BoreTunnelHandle {
    let cancel = CancellationToken::new();
    let status = Arc::new(RwLock::new(TunnelStatus::Connecting));
    let handle = BoreTunnelHandle {
        cancel: cancel.clone(),
        status: Arc::clone(&status),
    };

    let host = if bore_server.trim().is_empty() {
        "bore.pub".to_string()
    } else {
        bore_server.trim().to_string()
    };

    rt.spawn(async move {
        let secret_opt = secret.as_deref().map(str::trim).filter(|s| !s.is_empty());
        match BoreClient::new("127.0.0.1", local_port, &host, 0, secret_opt).await {
            Ok(client) => {
                let remote_port = client.remote_port();
                *status.write().unwrap_or_else(|e| e.into_inner()) = TunnelStatus::Running {
                    remote_host: host,
                    remote_port,
                };
                wake_ui.wake();

                tokio::select! {
                    _ = cancel.cancelled() => {
                        *status.write().unwrap_or_else(|e| e.into_inner()) = TunnelStatus::Stopped;
                    }
                    res = client.listen() => {
                        *status.write().unwrap_or_else(|e| e.into_inner()) = match res {
                            Ok(()) => TunnelStatus::Stopped,
                            Err(e) => TunnelStatus::Error(format!("Tunnel ngắt kết nối: {e}")),
                        };
                    }
                }
            }
            Err(e) => {
                *status.write().unwrap_or_else(|e| e.into_inner()) =
                    TunnelStatus::Error(format!("Không mở được Bore Tunnel: {e}"));
            }
        }
        wake_ui.wake();
    });

    handle
}

pub async fn probe_tcp_endpoint(host: &str, port: u16) -> Result<u128, String> {
    let trimmed = host.trim();
    if trimmed.is_empty() || port == 0 {
        return Err("Host hoặc port không hợp lệ".to_string());
    }
    let started = Instant::now();
    match timeout(Duration::from_secs(4), TcpStream::connect((trimmed, port))).await {
        Ok(Ok(_stream)) => Ok(started.elapsed().as_millis()),
        Ok(Err(e)) => Err(format!("Kết nối thất bại: {e}")),
        Err(_) => Err("Hết thời gian chờ (4s) — cổng chưa phản hồi".to_string()),
    }
}
