use crate::http_client::ensure_rustls_provider;
use crate::network_host::{
    clean_launcher_error_msg, configure_bridge_tcp_stream, new_local_client_bridge_handle,
    probe_local_tcp_listening, update_active_client_count, BridgePeerMap, ClientGuard,
    DirectHostStatus, LocalClientBridgeHandle,
};
use crate::server_manager::egui_repaint::RepaintHandle;
use iroh::address_lookup::{DnsAddressLookup, PkarrResolver};
use iroh::endpoint::{presets, PortmapperConfig};
use iroh::Watcher;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tl_core::config::write_atomic;
use tl_core::{LauncherError, LauncherResult};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

pub const P2P_TICKET_PREFIX: &str = "TLPORT:";
pub const P2P_BAN_PREFIX: &str = "p2p:";

const P2P_ALPN: &[u8] = b"trihienkun-tl-p2p/1";
const STREAM_KIND_TCP_BRIDGE: u8 = 0x01;
const STREAM_KIND_PROBE: u8 = 0x02;
const STATUS_OK: u8 = 0x00;
const STATUS_UPSTREAM_UNAVAILABLE: u8 = 0x01;
const STATUS_BANNED: u8 = 0xFF;

const TICKET_VERSION_V1: u8 = 0x01;
const RELAY_TAG_NONE: u8 = 0;
const RELAY_TAG_CUSTOM: u8 = 255;
const DEFAULT_FALLBACK_RELAY_URL: &str = "https://aps1-1.relay.n0.iroh.link./";
const RELAY_TAG_MAP: &[(u8, &str)] = &[
    (1, "https://aps1-1.relay.n0.iroh.link./"),
    (2, "https://euc1-1.relay.n0.iroh.link./"),
    (3, "https://use1-1.relay.n0.iroh.link./"),
    (4, "https://usw1-1.relay.n0.iroh.link./"),
];

pub(crate) type ActiveP2pConns = Mutex<Vec<(String, iroh::endpoint::Connection, CancellationToken)>>;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum P2pHostStatus {
    #[default]
    Disabled,
    Starting,
    Running {
        ticket: String,
        endpoint_id: String,
        relay_url: Option<String>,
    },
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgedPeerTarget {
    Ip(IpAddr),
    P2p(String),
}

pub fn is_p2p_ticket(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.len() <= P2P_TICKET_PREFIX.len() {
        return false;
    }
    let (prefix, body) = trimmed.split_at(P2P_TICKET_PREFIX.len());
    prefix.eq_ignore_ascii_case(P2P_TICKET_PREFIX)
        && !body.is_empty()
        && !body.chars().any(char::is_whitespace)
}

pub fn format_p2p_ticket_short(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() < P2P_TICKET_PREFIX.len()
        || !trimmed[..P2P_TICKET_PREFIX.len()].eq_ignore_ascii_case(P2P_TICKET_PREFIX)
    {
        return trimmed.to_string();
    }
    let body = &trimmed[P2P_TICKET_PREFIX.len()..];
    if body.is_ascii() {
        if body.len() > 14 {
            format!(
                "{P2P_TICKET_PREFIX}{}…{}",
                &body[..6],
                &body[body.len() - 6..]
            )
        } else {
            format!("{P2P_TICKET_PREFIX}{body}")
        }
    } else {
        let chars: Vec<char> = body.chars().collect();
        if chars.len() > 14 {
            let head: String = chars[..6].iter().collect();
            let tail: String = chars[chars.len() - 6..].iter().collect();
            format!("{P2P_TICKET_PREFIX}{head}…{tail}")
        } else {
            format!("{P2P_TICKET_PREFIX}{body}")
        }
    }
}

fn canonicalize_relay_for_match(url: &str) -> String {
    let without_slash = url.trim().trim_end_matches('/');
    let without_dot = without_slash.strip_suffix('.').unwrap_or(without_slash);
    without_dot.to_ascii_lowercase()
}

fn relay_url_to_tag(url: &str) -> Option<u8> {
    let normalized = canonicalize_relay_for_match(url);
    RELAY_TAG_MAP.iter().find_map(|(tag, known)| {
        (canonicalize_relay_for_match(known) == normalized).then_some(*tag)
    })
}

fn relay_tag_to_url(tag: u8) -> Option<&'static str> {
    RELAY_TAG_MAP
        .iter()
        .find_map(|(known_tag, url)| (*known_tag == tag).then_some(*url))
}

fn parse_valid_relay_url(raw: &str) -> LauncherResult<iroh::RelayUrl> {
    let trimmed = raw.trim();
    if let Some(canon) = relay_url_to_tag(trimmed).and_then(relay_tag_to_url) {
        return canon.parse().map_err(|e| {
            LauncherError::InvalidFormat(format!("URL relay P2P chuẩn hóa không hợp lệ: {e}"))
        });
    }
    let parsed: iroh::RelayUrl = trimmed
        .parse()
        .map_err(|e| LauncherError::InvalidFormat(format!("URL relay P2P không hợp lệ: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") || !parsed.has_host() {
        return Err(LauncherError::InvalidFormat(
            "URL relay P2P phải dùng HTTP/HTTPS và có hostname hợp lệ".to_string(),
        ));
    }
    Ok(parsed)
}

fn base64url_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        match chunk {
            [b0, b1, b2] => {
                let n = ((u32::from(*b0)) << 16) | ((u32::from(*b1)) << 8) | u32::from(*b2);
                out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
                out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
                out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
                out.push(TABLE[(n & 0x3F) as usize] as char);
            }
            [b0, b1] => {
                let n = ((u32::from(*b0)) << 16) | ((u32::from(*b1)) << 8);
                out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
                out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
                out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
            }
            [b0] => {
                let n = (u32::from(*b0)) << 16;
                out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
                out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
            }
            _ => {}
        }
    }
    out
}

fn base64url_decode_char(b: u8) -> LauncherResult<u8> {
    match b {
        b'A'..=b'Z' => Ok(b - b'A'),
        b'a'..=b'z' => Ok(b - b'a' + 26),
        b'0'..=b'9' => Ok(b - b'0' + 52),
        b'-' => Ok(62),
        b'_' => Ok(63),
        _ => Err(LauncherError::InvalidFormat(
            "Mã P2P chứa ký tự Base64URL không hợp lệ".to_string(),
        )),
    }
}

fn base64url_decode(input: &str) -> LauncherResult<Vec<u8>> {
    let bytes = input.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 == 1 {
        return Err(LauncherError::InvalidFormat(
            "Độ dài mã P2P Base64URL không hợp lệ".to_string(),
        ));
    }
    let mut out = Vec::with_capacity((bytes.len() * 3) / 4);
    for chunk in bytes.chunks(4) {
        match chunk {
            [c0, c1, c2, c3] => {
                let v0 = base64url_decode_char(*c0)?;
                let v1 = base64url_decode_char(*c1)?;
                let v2 = base64url_decode_char(*c2)?;
                let v3 = base64url_decode_char(*c3)?;
                out.push((v0 << 2) | (v1 >> 4));
                out.push(((v1 & 0x0F) << 4) | (v2 >> 2));
                out.push(((v2 & 0x03) << 6) | v3);
            }
            [c0, c1, c2] => {
                let v0 = base64url_decode_char(*c0)?;
                let v1 = base64url_decode_char(*c1)?;
                let v2 = base64url_decode_char(*c2)?;
                if (v2 & 0x03) != 0 {
                    return Err(LauncherError::InvalidFormat(
                        "Mã P2P Base64URL có bit đệm không hợp lệ".to_string(),
                    ));
                }
                out.push((v0 << 2) | (v1 >> 4));
                out.push(((v1 & 0x0F) << 4) | (v2 >> 2));
            }
            [c0, c1] => {
                let v0 = base64url_decode_char(*c0)?;
                let v1 = base64url_decode_char(*c1)?;
                if (v1 & 0x0F) != 0 {
                    return Err(LauncherError::InvalidFormat(
                        "Mã P2P Base64URL có bit đệm không hợp lệ".to_string(),
                    ));
                }
                out.push((v0 << 2) | (v1 >> 4));
            }
            _ => unreachable!("chunk length constrained by len % 4 != 1"),
        }
    }
    Ok(out)
}

pub fn encode_p2p_ticket(id: &iroh::PublicKey, relay_url: Option<&str>) -> String {
    let clean_relay = relay_url.map(str::trim).filter(|s| !s.is_empty());
    let mut payload = Vec::with_capacity(34);
    payload.push(TICKET_VERSION_V1);
    payload.extend_from_slice(id.as_bytes());
    match clean_relay {
        None => payload.push(RELAY_TAG_NONE),
        Some(url) => {
            if let Some(tag) = relay_url_to_tag(url) {
                payload.push(tag);
            } else {
                payload.push(RELAY_TAG_CUSTOM);
                payload.extend_from_slice(url.as_bytes());
            }
        }
    }
    format!("{P2P_TICKET_PREFIX}{}", base64url_encode(&payload))
}

fn decode_v1_ticket_body(body: &str) -> LauncherResult<iroh::EndpointAddr> {
    let bytes = base64url_decode(body)?;
    if bytes.len() < 34 || bytes[0] != TICKET_VERSION_V1 {
        return Err(LauncherError::InvalidFormat(
            "Định dạng mã P2P v1 không hợp lệ".to_string(),
        ));
    }
    let key_bytes: [u8; 32] = bytes[1..33]
        .try_into()
        .map_err(|_| LauncherError::InvalidFormat("Khóa P2P không đủ 32 byte".to_string()))?;
    let pubkey = iroh::PublicKey::from_bytes(&key_bytes)
        .map_err(|e| LauncherError::InvalidFormat(format!("PublicKey P2P không hợp lệ: {e}")))?;

    let relay_tag = bytes[33];
    let relay_url = match relay_tag {
        RELAY_TAG_NONE => {
            if bytes.len() != 34 {
                return Err(LauncherError::InvalidFormat(
                    "Độ dài payload mã P2P không khớp".to_string(),
                ));
            }
            parse_valid_relay_url(DEFAULT_FALLBACK_RELAY_URL)?
        }
        tag @ 1..=4 => {
            if bytes.len() != 34 {
                return Err(LauncherError::InvalidFormat(
                    "Độ dài payload mã P2P không khớp".to_string(),
                ));
            }
            let url_str = relay_tag_to_url(tag).ok_or_else(|| {
                LauncherError::InvalidFormat("Mã vùng relay P2P không hợp lệ".to_string())
            })?;
            parse_valid_relay_url(url_str)?
        }
        RELAY_TAG_CUSTOM => {
            if bytes.len() <= 34 {
                return Err(LauncherError::InvalidFormat(
                    "Thiếu URL relay tùy chỉnh trong mã P2P".to_string(),
                ));
            }
            let raw_url = std::str::from_utf8(&bytes[34..]).map_err(|_| {
                LauncherError::InvalidFormat("URL relay tùy chỉnh không phải UTF-8".to_string())
            })?;
            parse_valid_relay_url(raw_url)?
        }
        other => {
            return Err(LauncherError::InvalidFormat(format!(
                "Mã relay P2P không được hỗ trợ: {other}"
            )));
        }
    };

    Ok(iroh::EndpointAddr::new(pubkey).with_relay_url(relay_url))
}

pub fn parse_p2p_ticket(raw: &str) -> LauncherResult<iroh::EndpointAddr> {
    let trimmed = raw.trim();
    if trimmed.len() <= P2P_TICKET_PREFIX.len()
        || !trimmed[..P2P_TICKET_PREFIX.len()].eq_ignore_ascii_case(P2P_TICKET_PREFIX)
    {
        return Err(LauncherError::InvalidFormat(
            "Mã Cổng Ảo P2P phải bắt đầu bằng TLPORT:".to_string(),
        ));
    }
    let body = &trimmed[P2P_TICKET_PREFIX.len()..];
    if body.is_empty() || body.chars().any(char::is_whitespace) {
        return Err(LauncherError::InvalidFormat(
            "Mã Cổng Ảo P2P trống hoặc chứa khoảng trắng".to_string(),
        ));
    }

    if let Ok(addr) = decode_v1_ticket_body(body) {
        return Ok(addr);
    }

    let fallback_key = body
        .parse::<iroh::PublicKey>()
        .or_else(|err| {
            if body.len() == 64 && body.bytes().all(|b| b.is_ascii_hexdigit()) {
                body.to_ascii_lowercase().parse::<iroh::PublicKey>()
            } else {
                Err(err)
            }
        })
        .map_err(|e| LauncherError::InvalidFormat(format!("Mã Cổng Ảo P2P không hợp lệ: {e}")))?;

    Ok(iroh::EndpointAddr::new(fallback_key))
}

fn parse_secret_key_bytes(raw: &[u8]) -> Option<iroh::SecretKey> {
    if let Ok(exact) = <[u8; 32]>::try_from(raw) {
        return Some(iroh::SecretKey::from_bytes(&exact));
    }
    let text = std::str::from_utf8(raw).ok()?.trim();
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let decoded = hex::decode(text).ok()?;
    let exact: [u8; 32] = decoded.try_into().ok()?;
    Some(iroh::SecretKey::from_bytes(&exact))
}

pub(crate) fn load_or_create_p2p_secret_key(path: Option<&Path>) -> iroh::SecretKey {
    if let Some(key_path) = path {
        if let Ok(existing) = std::fs::read(key_path) {
            if let Some(key) = parse_secret_key_bytes(&existing) {
                return key;
            }
        }
    }

    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(&uuid::Uuid::new_v4().into_bytes());
    bytes[16..].copy_from_slice(&uuid::Uuid::new_v4().into_bytes());

    if let Some(key_path) = path {
        if let Some(parent) = key_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            if let Err(err) = std::fs::create_dir_all(parent) {
                tracing::warn!(path = %parent.display(), error = %err, "Failed to create P2P key directory");
            }
        }
        if let Err(err) = write_atomic(key_path, &bytes) {
            tracing::warn!(path = %key_path.display(), error = %err, "Failed to persist P2P secret key");
        }
    }

    iroh::SecretKey::from_bytes(&bytes)
}

pub(crate) fn normalize_p2p_peer_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let body = if trimmed.len() > P2P_BAN_PREFIX.len()
        && trimmed[..P2P_BAN_PREFIX.len()].eq_ignore_ascii_case(P2P_BAN_PREFIX)
    {
        &trimmed[P2P_BAN_PREFIX.len()..]
    } else {
        trimmed
    };
    if body.len() < 32 || !body.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some(body.to_ascii_lowercase())
}

pub(crate) fn normalize_p2p_ban_set(peers: impl IntoIterator<Item = String>) -> HashSet<String> {
    peers
        .into_iter()
        .filter_map(|raw| normalize_p2p_peer_id(&raw))
        .collect()
}

pub(crate) fn apply_p2p_peer_ban(
    banned_p2p_peers: &RwLock<HashSet<String>>,
    bridge_peers: &RwLock<BridgePeerMap>,
    active_p2p_conns: &ActiveP2pConns,
    peers: impl IntoIterator<Item = String>,
) {
    let ban_set = normalize_p2p_ban_set(peers);
    *banned_p2p_peers.write().unwrap_or_else(|e| e.into_inner()) = ban_set.clone();

    let stream_tokens: Vec<CancellationToken> = {
        let map = bridge_peers.read().unwrap_or_else(|e| e.into_inner());
        map.values()
            .filter_map(|(target, tok)| match target {
                BridgedPeerTarget::P2p(peer_id) if ban_set.contains(peer_id) => Some(tok.clone()),
                _ => None,
            })
            .collect()
    };
    for tok in stream_tokens {
        tok.cancel();
    }

    let conns_to_close: Vec<(iroh::endpoint::Connection, CancellationToken)> = {
        let mut guard = active_p2p_conns.lock().unwrap_or_else(|e| e.into_inner());
        let mut to_close = Vec::new();
        let existing = std::mem::take(&mut *guard);
        for (peer_id, conn, tok) in existing {
            if tok.is_cancelled() || conn.close_reason().is_some() {
                continue;
            }
            if ban_set.contains(&peer_id) {
                to_close.push((conn, tok));
            } else {
                guard.push((peer_id, conn, tok));
            }
        }
        to_close
    };

    for (conn, tok) in conns_to_close {
        tok.cancel();
        conn.close(1u32.into(), b"banned");
    }
}

fn is_p2p_peer_banned(banned_p2p_peers: &RwLock<HashSet<String>>, peer_id: &str) -> bool {
    banned_p2p_peers
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .contains(peer_id)
}

async fn bind_p2p_endpoint(
    secret_key: Option<iroh::SecretKey>,
    alpns: Vec<Vec<u8>>,
    use_relay_preset: bool,
    enable_portmapper: bool,
) -> LauncherResult<iroh::Endpoint> {
    ensure_rustls_provider();
    let mut builder = if use_relay_preset {
        let mut b = iroh::Endpoint::builder(presets::N0);
        if alpns.is_empty() {
            b = b
                .clear_address_lookup()
                .address_lookup(PkarrResolver::n0_dns())
                .address_lookup(DnsAddressLookup::n0_dns());
        }
        if !enable_portmapper {
            b = b.portmapper_config(PortmapperConfig::Disabled);
        }
        b
    } else {
        iroh::Endpoint::builder(presets::Minimal)
            .portmapper_config(PortmapperConfig::Disabled)
            .clear_ip_transports()
            .bind_addr("127.0.0.1:0")
            .map_err(|e| LauncherError::Network(format!("Lỗi cấu hình bind P2P loopback: {e}")))?
    };

    if let Some(key) = secret_key {
        builder = builder.secret_key(key);
    }
    if !alpns.is_empty() {
        builder = builder.alpns(alpns);
    }

    builder
        .bind()
        .await
        .map_err(|e| LauncherError::Network(format!("Không thể khởi tạo điểm cuối P2P: {e}")))
}

fn build_direct_endpoint_addr(endpoint: &iroh::Endpoint) -> iroh::EndpointAddr {
    let mut addr = iroh::EndpointAddr::new(endpoint.id());
    for sock in endpoint.bound_sockets() {
        let dial_sock = match sock {
            SocketAddr::V4(v4) if v4.ip().is_unspecified() => {
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), v4.port())
            }
            SocketAddr::V6(v6) if v6.ip().is_unspecified() => {
                SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), v6.port())
            }
            other => other,
        };
        addr = addr.with_ip_addr(dial_sock);
    }
    addr
}

async fn pump_tcp_and_iroh_streams(
    tcp: TcpStream,
    mut quic_send: iroh::endpoint::SendStream,
    mut quic_recv: iroh::endpoint::RecvStream,
    cancel: CancellationToken,
    peer_cancel: Option<CancellationToken>,
) {
    let (mut tcp_read, mut tcp_write) = tcp.into_split();

    let tcp_to_quic = async {
        let _ = tokio::io::copy(&mut tcp_read, &mut quic_send).await;
        let _ = quic_send.finish();
        let _ = timeout(Duration::from_secs(2), quic_send.stopped()).await;
    };

    let quic_to_tcp = async {
        let _ = tokio::io::copy(&mut quic_recv, &mut tcp_write).await;
        let _ = AsyncWriteExt::shutdown(&mut tcp_write).await;
    };

    let peer_cancelled = async {
        match peer_cancel {
            Some(tok) => tok.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };

    tokio::select! {
        _ = cancel.cancelled() => {}
        _ = peer_cancelled => {}
        _ = async { tokio::join!(tcp_to_quic, quic_to_tcp); } => {}
    }
}

fn update_p2p_status(
    status: &RwLock<DirectHostStatus>,
    wake_ui: &RepaintHandle,
    next: P2pHostStatus,
) {
    let mut changed = false;
    {
        let mut guard = status.write().unwrap_or_else(|e| e.into_inner());
        if let DirectHostStatus::Running(ref mut info) = *guard {
            if info.p2p_status != next {
                info.p2p_status = next;
                changed = true;
            }
        }
    }
    if changed {
        wake_ui.wake();
    }
}

async fn write_status_and_finish(mut send: iroh::endpoint::SendStream, status_byte: u8) {
    if send.write_all(&[status_byte]).await.is_ok() {
        let _ = send.finish();
        let _ = timeout(Duration::from_secs(2), send.stopped()).await;
    }
}

struct ActiveConnCleanupGuard {
    conn_cancel: CancellationToken,
    active_p2p_conns: Arc<ActiveP2pConns>,
}

impl Drop for ActiveConnCleanupGuard {
    fn drop(&mut self) {
        self.conn_cancel.cancel();
        let mut guard = self
            .active_p2p_conns
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.retain(|(_, c, tok)| !tok.is_cancelled() && c.close_reason().is_none());
    }
}

#[derive(Clone)]
struct P2pHostContext {
    local_target_port: u16,
    cancel: CancellationToken,
    active_clients: Arc<AtomicUsize>,
    status: Arc<RwLock<DirectHostStatus>>,
    wake_ui: RepaintHandle,
    banned_p2p_peers: Arc<RwLock<HashSet<String>>>,
    active_p2p_conns: Arc<ActiveP2pConns>,
    bridge_peers: Arc<RwLock<BridgePeerMap>>,
}

async fn handle_p2p_bi_stream(
    ctx: P2pHostContext,
    peer_id: String,
    conn: iroh::endpoint::Connection,
    conn_cancel: CancellationToken,
    send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
) {
    let mut kind_buf = [0u8; 1];
    let Ok(Ok(())) = timeout(Duration::from_secs(5), recv.read_exact(&mut kind_buf)).await else {
        return;
    };

    match kind_buf[0] {
        STREAM_KIND_PROBE => {
            let status_byte = if is_p2p_peer_banned(&ctx.banned_p2p_peers, &peer_id) {
                STATUS_BANNED
            } else if probe_local_tcp_listening(ctx.local_target_port).await {
                STATUS_OK
            } else {
                STATUS_UPSTREAM_UNAVAILABLE
            };
            write_status_and_finish(send, status_byte).await;
            if status_byte == STATUS_BANNED {
                conn_cancel.cancel();
                conn.close(1u32.into(), b"banned");
            }
        }
        STREAM_KIND_TCP_BRIDGE => {
            if is_p2p_peer_banned(&ctx.banned_p2p_peers, &peer_id) {
                write_status_and_finish(send, STATUS_BANNED).await;
                conn_cancel.cancel();
                conn.close(1u32.into(), b"banned");
                return;
            }

            let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), ctx.local_target_port);
            let Ok(Ok(tcp_stream)) = timeout(Duration::from_secs(3), TcpStream::connect(target)).await
            else {
                write_status_and_finish(send, STATUS_UPSTREAM_UNAVAILABLE).await;
                return;
            };

            if is_p2p_peer_banned(&ctx.banned_p2p_peers, &peer_id) {
                drop(tcp_stream);
                write_status_and_finish(send, STATUS_BANNED).await;
                conn_cancel.cancel();
                conn.close(1u32.into(), b"banned");
                return;
            }

            configure_bridge_tcp_stream(&tcp_stream);
            let outbound_port = tcp_stream.local_addr().ok().map(|a| a.port());
            let stream_cancel = conn_cancel.child_token();
            if let Some(port) = outbound_port {
                ctx.bridge_peers
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(port, (BridgedPeerTarget::P2p(peer_id), stream_cancel.clone()));
            }

            let now_count = ctx.active_clients.fetch_add(1, Ordering::SeqCst) + 1;
            let _guard = ClientGuard {
                counter: Arc::clone(&ctx.active_clients),
                status: Arc::clone(&ctx.status),
                wake_ui: ctx.wake_ui.clone(),
                outbound_port,
                bridge_peers: Arc::clone(&ctx.bridge_peers),
            };
            update_active_client_count(&ctx.status, &ctx.wake_ui, now_count);

            let mut send = send;
            if send.write_all(&[STATUS_OK]).await.is_err() {
                return;
            }

            pump_tcp_and_iroh_streams(tcp_stream, send, recv, ctx.cancel, Some(stream_cancel))
                .await;
        }
        _ => {}
    }
}

async fn handle_p2p_connection(ctx: P2pHostContext, incoming: iroh::endpoint::Incoming) {
    let Ok(accepting) = incoming.accept() else {
        return;
    };
    let conn = tokio::select! {
        _ = ctx.cancel.cancelled() => return,
        res = timeout(Duration::from_secs(10), accepting) => match res {
            Ok(Ok(conn)) => conn,
            _ => return,
        },
    };

    let peer_id = conn.remote_id().to_string().to_ascii_lowercase();
    let conn_cancel = CancellationToken::new();

    if is_p2p_peer_banned(&ctx.banned_p2p_peers, &peer_id) {
        let accepted = tokio::select! {
            _ = ctx.cancel.cancelled() => None,
            res = timeout(Duration::from_secs(3), conn.accept_bi()) => res.ok().and_then(Result::ok),
        };
        if let Some((send, mut recv)) = accepted {
            let mut kind = [0u8; 1];
            let _ = timeout(Duration::from_secs(2), recv.read_exact(&mut kind)).await;
            write_status_and_finish(send, STATUS_BANNED).await;
        }
        conn.close(1u32.into(), b"banned");
        return;
    }

    {
        let mut guard = ctx
            .active_p2p_conns
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.retain(|(_, c, tok)| !tok.is_cancelled() && c.close_reason().is_none());
        guard.push((peer_id.clone(), conn.clone(), conn_cancel.clone()));
    }
    let _conn_cleanup = ActiveConnCleanupGuard {
        conn_cancel: conn_cancel.clone(),
        active_p2p_conns: Arc::clone(&ctx.active_p2p_conns),
    };

    if is_p2p_peer_banned(&ctx.banned_p2p_peers, &peer_id) {
        conn.close(1u32.into(), b"banned");
        return;
    }

    loop {
        let accepted = tokio::select! {
            _ = ctx.cancel.cancelled() => break,
            _ = conn_cancel.cancelled() => break,
            res = conn.accept_bi() => match res {
                Ok(pair) => pair,
                Err(_) => break,
            },
        };
        let (send, recv) = accepted;
        let stream_ctx = ctx.clone();
        let stream_peer = peer_id.clone();
        let stream_conn = conn.clone();
        let stream_conn_cancel = conn_cancel.clone();
        tokio::spawn(async move {
            handle_p2p_bi_stream(
                stream_ctx,
                stream_peer,
                stream_conn,
                stream_conn_cancel,
                send,
                recv,
            )
            .await;
        });
    }
}

fn publish_online_relay_status(
    endpoint_id: &iroh::PublicKey,
    stable_ticket: &str,
    addr: &iroh::EndpointAddr,
    status: &RwLock<DirectHostStatus>,
    wake_ui: &RepaintHandle,
) {
    if let Some(url_str) = addr.relay_urls().next().map(|u| u.to_string()) {
        update_p2p_status(
            status,
            wake_ui,
            P2pHostStatus::Running {
                ticket: stable_ticket.to_string(),
                endpoint_id: endpoint_id.to_string(),
                relay_url: Some(url_str),
            },
        );
    }
}

fn spawn_online_relay_watcher(
    endpoint: &iroh::Endpoint,
    stable_ticket: String,
    status: Arc<RwLock<DirectHostStatus>>,
    wake_ui: RepaintHandle,
    cancel: CancellationToken,
) {
    let endpoint_id = endpoint.id();
    let mut watcher = endpoint.watch_addr();
    tokio::spawn(async move {
        publish_online_relay_status(&endpoint_id, &stable_ticket, &watcher.get(), &status, &wake_ui);
        loop {
            let addr = tokio::select! {
                _ = cancel.cancelled() => break,
                updated = watcher.updated() => match updated {
                    Ok(addr) => addr,
                    Err(_) => break,
                },
            };
            publish_online_relay_status(&endpoint_id, &stable_ticket, &addr, &status, &wake_ui);
        }
    });
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_p2p_host_worker(
    local_target_port: u16,
    secret_key: iroh::SecretKey,
    cancel: CancellationToken,
    active_clients: Arc<AtomicUsize>,
    status: Arc<RwLock<DirectHostStatus>>,
    wake_ui: RepaintHandle,
    banned_p2p_peers: Arc<RwLock<HashSet<String>>>,
    active_p2p_conns: Arc<ActiveP2pConns>,
    bridge_peers: Arc<RwLock<BridgePeerMap>>,
) -> tokio::task::JoinHandle<()> {
    spawn_p2p_host_worker_inner(
        local_target_port,
        secret_key,
        cancel,
        active_clients,
        status,
        wake_ui,
        banned_p2p_peers,
        active_p2p_conns,
        bridge_peers,
        true,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_p2p_host_worker_inner(
    local_target_port: u16,
    secret_key: iroh::SecretKey,
    cancel: CancellationToken,
    active_clients: Arc<AtomicUsize>,
    status: Arc<RwLock<DirectHostStatus>>,
    wake_ui: RepaintHandle,
    banned_p2p_peers: Arc<RwLock<HashSet<String>>>,
    active_p2p_conns: Arc<ActiveP2pConns>,
    bridge_peers: Arc<RwLock<BridgePeerMap>>,
    use_relay_preset: bool,
    bound_addr_tx: Option<oneshot::Sender<iroh::EndpointAddr>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let bind_fut = bind_p2p_endpoint(
            Some(secret_key),
            vec![P2P_ALPN.to_vec()],
            use_relay_preset,
            true,
        );
        let endpoint = tokio::select! {
            _ = cancel.cancelled() => return,
            res = bind_fut => match res {
                Ok(ep) => ep,
                Err(err) => {
                    update_p2p_status(
                        &status,
                        &wake_ui,
                        P2pHostStatus::Error(clean_launcher_error_msg(err)),
                    );
                    return;
                }
            },
        };

        if cancel.is_cancelled() {
            endpoint.close().await;
            return;
        }

        if use_relay_preset {
            tokio::select! {
                _ = cancel.cancelled() => {
                    endpoint.close().await;
                    return;
                }
                _ = timeout(Duration::from_millis(2500), endpoint.online()) => {}
            }
        }
        if cancel.is_cancelled() {
            endpoint.close().await;
            return;
        }

        if let Some(tx) = bound_addr_tx {
            let _ = tx.send(build_direct_endpoint_addr(&endpoint));
        }

        let endpoint_id = endpoint.id();
        let initial_relay = endpoint
            .addr()
            .relay_urls()
            .next()
            .map(|u| u.to_string())
            .or_else(|| Some(DEFAULT_FALLBACK_RELAY_URL.to_string()));
        let stable_ticket = encode_p2p_ticket(&endpoint_id, None);
        update_p2p_status(
            &status,
            &wake_ui,
            P2pHostStatus::Running {
                ticket: stable_ticket.clone(),
                endpoint_id: endpoint_id.to_string(),
                relay_url: initial_relay,
            },
        );

        if use_relay_preset {
            spawn_online_relay_watcher(
                &endpoint,
                stable_ticket,
                Arc::clone(&status),
                wake_ui.clone(),
                cancel.clone(),
            );
        }

        let ctx = P2pHostContext {
            local_target_port,
            cancel: cancel.clone(),
            active_clients,
            status,
            wake_ui,
            banned_p2p_peers,
            active_p2p_conns: Arc::clone(&active_p2p_conns),
            bridge_peers,
        };

        loop {
            let incoming = tokio::select! {
                _ = cancel.cancelled() => break,
                accepted = endpoint.accept() => match accepted {
                    Some(inc) => inc,
                    None => break,
                },
            };
            let conn_ctx = ctx.clone();
            tokio::spawn(async move {
                handle_p2p_connection(conn_ctx, incoming).await;
            });
        }

        active_p2p_conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        endpoint.close().await;
    })
}

enum BridgeOpenError {
    Retryable(LauncherError),
    Fatal(LauncherError),
}

async fn probe_connection(conn: &iroh::endpoint::Connection) -> Result<(), BridgeOpenError> {
    let (mut send, mut recv) = timeout(Duration::from_secs(8), conn.open_bi())
        .await
        .map_err(|_| {
            BridgeOpenError::Retryable(LauncherError::Network(
                "Hết thời gian mở luồng kiểm tra P2P (8s)".to_string(),
            ))
        })?
        .map_err(|e| {
            BridgeOpenError::Retryable(LauncherError::Network(format!(
                "Không mở được luồng kiểm tra P2P: {e}"
            )))
        })?;

    timeout(Duration::from_secs(8), send.write_all(&[STREAM_KIND_PROBE]))
        .await
        .map_err(|_| {
            BridgeOpenError::Retryable(LauncherError::Network(
                "Hết thời gian gửi yêu cầu kiểm tra P2P".to_string(),
            ))
        })?
        .map_err(|e| {
            BridgeOpenError::Retryable(LauncherError::Network(format!(
                "Lỗi gửi yêu cầu kiểm tra P2P: {e}"
            )))
        })?;
    let _ = send.finish();

    let mut status_buf = [0u8; 1];
    timeout(Duration::from_secs(8), recv.read_exact(&mut status_buf))
        .await
        .map_err(|_| {
            BridgeOpenError::Retryable(LauncherError::Network(
                "Hết thời gian chờ phản hồi kiểm tra P2P (8s)".to_string(),
            ))
        })?
        .map_err(|e| {
            BridgeOpenError::Retryable(LauncherError::Network(format!(
                "Không đọc được phản hồi kiểm tra P2P: {e}"
            )))
        })?;

    match status_buf[0] {
        STATUS_OK => Ok(()),
        STATUS_BANNED => Err(BridgeOpenError::Fatal(LauncherError::Network(
            "Bạn đã bị cấm (Ban) khỏi Server này".to_string(),
        ))),
        STATUS_UPSTREAM_UNAVAILABLE => Err(BridgeOpenError::Fatal(LauncherError::Network(
            "Server đích hiện đang tắt hoặc chưa sẵn sàng".to_string(),
        ))),
        other => Err(BridgeOpenError::Fatal(LauncherError::Network(format!(
            "Server trả về mã trạng thái P2P không hợp lệ: 0x{other:02X}"
        )))),
    }
}

async fn try_open_tcp_bridge_on_conn(
    conn: &iroh::endpoint::Connection,
) -> Result<(iroh::endpoint::SendStream, iroh::endpoint::RecvStream), BridgeOpenError> {
    let (mut send, mut recv) = timeout(Duration::from_secs(8), conn.open_bi())
        .await
        .map_err(|_| {
            BridgeOpenError::Retryable(LauncherError::Network(
                "Hết thời gian mở luồng P2P tới Server".to_string(),
            ))
        })?
        .map_err(|e| {
            BridgeOpenError::Retryable(LauncherError::Network(format!(
                "Không mở được luồng P2P: {e}"
            )))
        })?;

    timeout(
        Duration::from_secs(8),
        send.write_all(&[STREAM_KIND_TCP_BRIDGE]),
    )
    .await
    .map_err(|_| {
        BridgeOpenError::Retryable(LauncherError::Network(
            "Hết thời gian khởi tạo luồng cầu nối P2P".to_string(),
        ))
    })?
    .map_err(|e| {
        BridgeOpenError::Retryable(LauncherError::Network(format!(
            "Lỗi khởi tạo luồng cầu nối P2P: {e}"
        )))
    })?;

    let mut status_buf = [0u8; 1];
    timeout(Duration::from_secs(8), recv.read_exact(&mut status_buf))
        .await
        .map_err(|_| {
            BridgeOpenError::Retryable(LauncherError::Network(
                "Hết thời gian chờ xác nhận cầu nối P2P".to_string(),
            ))
        })?
        .map_err(|e| {
            BridgeOpenError::Retryable(LauncherError::Network(format!(
                "Mất kết nối khi mở cầu nối P2P: {e}"
            )))
        })?;

    match status_buf[0] {
        STATUS_OK => Ok((send, recv)),
        STATUS_BANNED => Err(BridgeOpenError::Fatal(LauncherError::Network(
            "Bạn đã bị cấm (Ban) khỏi Server này".to_string(),
        ))),
        STATUS_UPSTREAM_UNAVAILABLE => Err(BridgeOpenError::Fatal(LauncherError::Network(
            "Server đích hiện đang tắt hoặc chưa sẵn sàng".to_string(),
        ))),
        other => Err(BridgeOpenError::Fatal(LauncherError::Network(format!(
            "Server trả về trạng thái cầu nối P2P không hợp lệ: 0x{other:02X}"
        )))),
    }
}

async fn open_tcp_bridge_stream(
    endpoint: &iroh::Endpoint,
    addr: &iroh::EndpointAddr,
    cached_conn: &tokio::sync::Mutex<Option<iroh::endpoint::Connection>>,
) -> LauncherResult<(iroh::endpoint::SendStream, iroh::endpoint::RecvStream)> {
    let existing = {
        let guard = cached_conn.lock().await;
        guard.clone().filter(|c| c.close_reason().is_none())
    };
    if let Some(conn) = existing {
        match try_open_tcp_bridge_on_conn(&conn).await {
            Ok(pair) => return Ok(pair),
            Err(BridgeOpenError::Fatal(err)) => return Err(err),
            Err(BridgeOpenError::Retryable(_)) => {}
        }
    }

    let mut guard = cached_conn.lock().await;
    if let Some(conn) = guard.as_ref().filter(|c| c.close_reason().is_none()) {
        match try_open_tcp_bridge_on_conn(conn).await {
            Ok(pair) => return Ok(pair),
            Err(BridgeOpenError::Fatal(err)) => return Err(err),
            Err(BridgeOpenError::Retryable(_)) => {}
        }
    }

    let redialed = timeout(Duration::from_secs(10), endpoint.connect(addr.clone(), P2P_ALPN))
        .await
        .map_err(|_| LauncherError::Network("Hết thời gian kết nối lại P2P (10s)".to_string()))?
        .map_err(|e| LauncherError::Network(format!("Không thể kết nối lại P2P: {e}")))?;
    *guard = Some(redialed.clone());
    drop(guard);

    match try_open_tcp_bridge_on_conn(&redialed).await {
        Ok(pair) => Ok(pair),
        Err(BridgeOpenError::Fatal(err)) | Err(BridgeOpenError::Retryable(err)) => Err(err),
    }
}

async fn connect_and_probe_endpoint(
    addr: iroh::EndpointAddr,
    client_key_path: Option<&Path>,
    use_relay_preset: bool,
    enable_portmapper: bool,
) -> LauncherResult<(iroh::Endpoint, iroh::endpoint::Connection)> {
    let secret_key = load_or_create_p2p_secret_key(client_key_path);
    if addr.id == secret_key.public() {
        return Err(LauncherError::InvalidInput(
            "Không thể tạo cầu nối P2P tới chính Endpoint của mình".to_string(),
        ));
    }

    let endpoint = bind_p2p_endpoint(
        Some(secret_key),
        Vec::new(),
        use_relay_preset,
        enable_portmapper,
    )
    .await?;
    if use_relay_preset {
        let _ = timeout(Duration::from_millis(1500), endpoint.online()).await;
    }

    let mut attempt = 0u8;
    loop {
        let conn = match timeout(Duration::from_secs(10), endpoint.connect(addr.clone(), P2P_ALPN))
            .await
        {
            Ok(Ok(conn)) => conn,
            Ok(Err(_)) if attempt == 0 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(150)).await;
                continue;
            }
            Ok(Err(err)) => {
                endpoint.close().await;
                return Err(LauncherError::Network(format!(
                    "Không thể kết nối P2P tới Server: {err}"
                )));
            }
            Err(_) if attempt == 0 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(150)).await;
                continue;
            }
            Err(_) => {
                endpoint.close().await;
                return Err(LauncherError::Network(
                    "Hết thời gian kết nối P2P tới Server (10s)".to_string(),
                ));
            }
        };

        match probe_connection(&conn).await {
            Ok(()) => return Ok((endpoint, conn)),
            Err(BridgeOpenError::Fatal(err)) => {
                conn.close(0u32.into(), b"probe_rejected");
                endpoint.close().await;
                return Err(err);
            }
            Err(BridgeOpenError::Retryable(_)) if attempt == 0 => {
                conn.close(0u32.into(), b"probe_retry");
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            Err(BridgeOpenError::Retryable(err)) => {
                conn.close(0u32.into(), b"probe_retry");
                endpoint.close().await;
                return Err(err);
            }
        }
    }
}

async fn start_p2p_client_bridge_inner(
    addr: iroh::EndpointAddr,
    client_key_path: Option<&Path>,
    use_relay_preset: bool,
) -> LauncherResult<LocalClientBridgeHandle> {
    let (endpoint, conn) =
        connect_and_probe_endpoint(addr.clone(), client_key_path, use_relay_preset, true).await?;

    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await {
        Ok(listener) => listener,
        Err(err) => {
            conn.close(0u32.into(), b"bind_failed");
            endpoint.close().await;
            return Err(LauncherError::Network(format!(
                "Không mở được cầu nối cục bộ 127.0.0.1 cho P2P: {err}"
            )));
        }
    };
    let local_port = match listener.local_addr() {
        Ok(sock) => sock.port(),
        Err(err) => {
            drop(listener);
            conn.close(0u32.into(), b"addr_failed");
            endpoint.close().await;
            return Err(LauncherError::Network(format!(
                "Không lấy được cổng cầu nối cục bộ cho P2P: {err}"
            )));
        }
    };

    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let cached_conn = Arc::new(tokio::sync::Mutex::new(Some(conn)));

    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                _ = task_cancel.cancelled() => break,
                res = listener.accept() => res,
            };
            let Ok((tcp_stream, _)) = accepted else {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            configure_bridge_tcp_stream(&tcp_stream);
            let ep = endpoint.clone();
            let target_addr = addr.clone();
            let conn_cache = Arc::clone(&cached_conn);
            let child_cancel = task_cancel.clone();
            tokio::spawn(async move {
                let opened = tokio::select! {
                    _ = child_cancel.cancelled() => return,
                    res = open_tcp_bridge_stream(&ep, &target_addr, &conn_cache) => res,
                };
                let Ok((send, recv)) = opened else {
                    return;
                };
                pump_tcp_and_iroh_streams(tcp_stream, send, recv, child_cancel, None).await;
            });
        }

        drop(listener);
        {
            let mut guard = cached_conn.lock().await;
            if let Some(active) = guard.take() {
                active.close(0u32.into(), b"bridge_stopped");
            }
        }
        endpoint.close().await;
    });

    Ok(new_local_client_bridge_handle(cancel, local_port))
}

pub async fn start_p2p_client_bridge(
    ticket: &str,
    client_key_path: Option<&Path>,
) -> LauncherResult<LocalClientBridgeHandle> {
    let addr = parse_p2p_ticket(ticket)?;
    start_p2p_client_bridge_inner(addr, client_key_path, true).await
}

#[cfg(test)]
pub(crate) async fn start_p2p_client_bridge_addr(
    addr: iroh::EndpointAddr,
    client_key_path: Option<&Path>,
) -> LauncherResult<LocalClientBridgeHandle> {
    start_p2p_client_bridge_inner(addr, client_key_path, false).await
}

async fn probe_p2p_addr_reachability_inner(
    addr: iroh::EndpointAddr,
    client_key_path: Option<&Path>,
    use_relay_preset: bool,
) -> Result<u128, String> {
    let started = Instant::now();
    let (endpoint, conn) = connect_and_probe_endpoint(addr, client_key_path, use_relay_preset, false)
        .await
        .map_err(clean_launcher_error_msg)?;
    let elapsed_ms = started.elapsed().as_millis();
    conn.close(0u32.into(), b"probe_done");
    endpoint.close().await;
    Ok(elapsed_ms)
}

pub async fn probe_p2p_ticket_reachability(
    ticket: &str,
    client_key_path: Option<&Path>,
) -> Result<u128, String> {
    let addr = parse_p2p_ticket(ticket).map_err(clean_launcher_error_msg)?;
    probe_p2p_addr_reachability_inner(addr, client_key_path, true).await
}

#[cfg(test)]
pub(crate) async fn probe_p2p_addr_reachability(
    addr: iroh::EndpointAddr,
    client_key_path: Option<&Path>,
) -> Result<u128, String> {
    probe_p2p_addr_reachability_inner(addr, client_key_path, false).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_host::{
        DirectHostActiveInfo, DirectPortHostHandle, ExternalReachability, Ipv6PinholeStatus,
        UpnpPortStatus,
    };
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::Condvar;
    use tokio::io::AsyncReadExt;

    #[test]
    fn p2p_ticket_encode_parse_and_formatting_roundtrip() {
        let key = iroh::SecretKey::from_bytes(&[0x42u8; 32]).public();
        let default_relays = iroh::defaults::prod::default_relay_map();
        let ap_relay = iroh::defaults::prod::default_ap_relay().url;
        let eu_relay = iroh::defaults::prod::default_eu_relay().url;
        let na_east_relay = iroh::defaults::prod::default_na_east_relay().url;
        let na_west_relay = iroh::defaults::prod::default_na_west_relay().url;

        for (tag_url, expected_url, expected_relay) in [
            (
                "https://aps1-1.relay.n0.iroh.link/",
                "https://aps1-1.relay.n0.iroh.link./",
                &ap_relay,
            ),
            (
                "https://aps1-1.relay.n0.iroh.link",
                "https://aps1-1.relay.n0.iroh.link./",
                &ap_relay,
            ),
            (
                "https://aps1-1.relay.n0.iroh.link./",
                "https://aps1-1.relay.n0.iroh.link./",
                &ap_relay,
            ),
            (
                "https://euc1-1.relay.n0.iroh.link/",
                "https://euc1-1.relay.n0.iroh.link./",
                &eu_relay,
            ),
            (
                "https://use1-1.relay.n0.iroh.link",
                "https://use1-1.relay.n0.iroh.link./",
                &na_east_relay,
            ),
            (
                "https://usw1-1.relay.n0.iroh.link/",
                "https://usw1-1.relay.n0.iroh.link./",
                &na_west_relay,
            ),
        ] {
            let ticket = encode_p2p_ticket(&key, Some(tag_url));
            assert!(is_p2p_ticket(&ticket));
            assert!(!ticket.contains('='));
            let lower_prefix_ticket = format!("tlport:{}", &ticket[P2P_TICKET_PREFIX.len()..]);
            let parsed = parse_p2p_ticket(&lower_prefix_ticket).unwrap();
            assert_eq!(parsed.id, key);
            let relay = parsed.relay_urls().next().unwrap();
            assert_eq!(relay.as_str(), expected_url);
            assert_eq!(relay, expected_relay);
            assert!(default_relays.contains(relay));
        }

        let none_ticket = encode_p2p_ticket(&key, None);
        let parsed_none = parse_p2p_ticket(&format!("  {none_ticket}  ")).unwrap();
        assert_eq!(parsed_none.id, key);
        assert_eq!(
            parsed_none.relay_urls().next().map(|u| u.as_str()),
            Some("https://aps1-1.relay.n0.iroh.link./")
        );
        assert_eq!(parsed_none.relay_urls().next(), Some(&ap_relay));

        let parsed_eu_ticket =
            parse_p2p_ticket("TLPORT:AZfB0UwRIBW1RfblGrsj9Kl_Q0UdfeHZtj9qycqWTIuCAg").unwrap();
        assert_eq!(parsed_eu_ticket.relay_urls().next(), Some(&eu_relay));

        let custom_relay = "https://relay.custom-example.net:8443/";
        let custom_ticket = encode_p2p_ticket(&key, Some(custom_relay));
        let parsed_custom = parse_p2p_ticket(&custom_ticket).unwrap();
        assert_eq!(parsed_custom.id, key);
        assert_eq!(
            parsed_custom.relay_urls().next().map(|u| u.as_str()),
            Some(custom_relay)
        );

        let raw_id_ticket = format!("tlport:{key}");
        assert!(is_p2p_ticket(&raw_id_ticket));
        let parsed_raw = parse_p2p_ticket(&raw_id_ticket).unwrap();
        assert_eq!(parsed_raw.id, key);
        assert!(parsed_raw.relay_urls().next().is_none());

        assert!(!is_p2p_ticket(""));
        assert!(!is_p2p_ticket("TLPORT:"));
        assert!(!is_p2p_ticket("TLPORT:   "));
        assert!(!is_p2p_ticket("tlport:abc def"));
        assert!(!is_p2p_ticket("127.0.0.1:7777"));

        assert_eq!(format_p2p_ticket_short("tlport:short123"), "TLPORT:short123");
        assert_eq!(
            format_p2p_ticket_short("tlport:1234567890ABCDEF"),
            "TLPORT:123456…ABCDEF"
        );
        assert!(parse_p2p_ticket("TLPORT:not-valid-key").is_err());
    }

    #[test]
    fn load_or_create_p2p_secret_key_persists_raw_and_reads_hex() {
        let dir = std::env::temp_dir().join(format!("tl_p2p_key_{}", uuid::Uuid::new_v4()));
        let key_path = dir.join("nested").join("p2p.key");

        let created = load_or_create_p2p_secret_key(Some(&key_path));
        assert!(key_path.is_file());
        assert_eq!(std::fs::read(&key_path).unwrap().len(), 32);

        let reloaded = load_or_create_p2p_secret_key(Some(&key_path));
        assert_eq!(created.public(), reloaded.public());
        assert_eq!(created.to_bytes(), reloaded.to_bytes());

        let hex_path = dir.join("hex.key");
        let expected_bytes = [0xABu8; 32];
        std::fs::write(&hex_path, format!("  {}\r\n", hex::encode(expected_bytes))).unwrap();
        let hex_loaded = load_or_create_p2p_secret_key(Some(&hex_path));
        assert_eq!(hex_loaded.to_bytes(), expected_bytes);

        let mem_key_a = load_or_create_p2p_secret_key(None);
        let mem_key_b = load_or_create_p2p_secret_key(None);
        assert_ne!(mem_key_a.public(), mem_key_b.public());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offline_p2p_tunnel_bridges_tcp_tracks_peer_and_enforces_live_ban() {
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
                        let mut buf = [0u8; 64];
                        while let Ok(n) = sock.read(&mut buf).await {
                            if n == 0 || sock.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    });
                }
            });

            let host_key = iroh::SecretKey::from_bytes(&[0x11u8; 32]);
            let dir =
                std::env::temp_dir().join(format!("tl_p2p_e2e_{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let client_key_path = dir.join("client.key");
            let client_key = load_or_create_p2p_secret_key(Some(&client_key_path));
            let client_id = client_key.public().to_string();

            let cancel = CancellationToken::new();
            let active_clients = Arc::new(AtomicUsize::new(0));
            let status = Arc::new(RwLock::new(DirectHostStatus::Running(
                DirectHostActiveInfo {
                    local_port: upstream_port,
                    host_port: upstream_port,
                    lan_endpoints: Vec::new(),
                    vpn_endpoints: Vec::new(),
                    ipv6_endpoint: None,
                    ipv6_pinhole_status: Ipv6PinholeStatus::Disabled,
                    ipv6_reachability: ExternalReachability::NotChecked,
                    upnp_status: UpnpPortStatus::Disabled,
                    ipv4_reachability: ExternalReachability::NotChecked,
                    bridging_tcp: false,
                    active_clients: 0,
                    p2p_status: P2pHostStatus::Starting,
                },
            )));
            let banned_p2p_peers = Arc::new(RwLock::new(HashSet::new()));
            let active_p2p_conns = Arc::new(Mutex::new(Vec::new()));
            let bridge_peers = Arc::new(RwLock::new(HashMap::new()));
            let cleanup_done = Arc::new((Mutex::new(false), Condvar::new()));

            let handle = DirectPortHostHandle::new_for_test(
                cancel.clone(),
                Arc::clone(&status),
                Arc::new(RwLock::new(HashSet::new())),
                Arc::clone(&banned_p2p_peers),
                Arc::clone(&active_p2p_conns),
                Arc::clone(&bridge_peers),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(true)),
                Arc::clone(&cleanup_done),
                Arc::new(AtomicBool::new(false)),
            );

            let (addr_tx, addr_rx) = oneshot::channel();
            let host_join = spawn_p2p_host_worker_inner(
                upstream_port,
                host_key.clone(),
                cancel.clone(),
                Arc::clone(&active_clients),
                Arc::clone(&status),
                RepaintHandle::default(),
                Arc::clone(&banned_p2p_peers),
                Arc::clone(&active_p2p_conns),
                Arc::clone(&bridge_peers),
                false,
                Some(addr_tx),
            );

            let host_addr = timeout(Duration::from_secs(5), addr_rx)
                .await
                .expect("bound addr timeout")
                .expect("host addr channel");

            let wait_running = Instant::now() + Duration::from_secs(3);
            loop {
                if let DirectHostStatus::Running(info) = handle.current_status() {
                    if let P2pHostStatus::Running {
                        ref ticket,
                        ref endpoint_id,
                        ..
                    } = info.p2p_status
                    {
                        assert!(is_p2p_ticket(ticket));
                        assert_eq!(ticket, &encode_p2p_ticket(&host_key.public(), None));
                        assert_eq!(endpoint_id, &host_key.public().to_string());
                        assert_eq!(info.preferred_online_endpoint(), Some(ticket.clone()));
                        break;
                    }
                }
                assert!(
                    Instant::now() < wait_running,
                    "P2pHostStatus did not become Running"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            let probe_ms = probe_p2p_addr_reachability(host_addr.clone(), Some(&client_key_path))
                .await
                .expect("offline probe must succeed");
            assert!(probe_ms < 5000);

            let bridge = start_p2p_client_bridge_addr(host_addr.clone(), Some(&client_key_path))
                .await
                .expect("client bridge must start");

            let mut tcp_client = TcpStream::connect((Ipv4Addr::LOCALHOST, bridge.local_port()))
                .await
                .expect("must connect to local P2P client bridge");
            tcp_client.write_all(b"TL-P2P-PING").await.unwrap();
            let mut reply = [0u8; 11];
            timeout(Duration::from_secs(3), tcp_client.read_exact(&mut reply))
                .await
                .expect("read timeout")
                .expect("read pong");
            assert_eq!(&reply, b"TL-P2P-PING");

            let outbound_port = timeout(Duration::from_secs(2), peer_port_rx.recv())
                .await
                .expect("outbound port timeout")
                .expect("outbound port");
            assert_eq!(handle.resolve_bridged_peer(outbound_port), None);
            assert_eq!(
                handle.resolve_bridged_peer_target(outbound_port),
                Some(BridgedPeerTarget::P2p(client_id.clone()))
            );
            assert_eq!(active_clients.load(Ordering::SeqCst), 1);
            match handle.current_status() {
                DirectHostStatus::Running(info) => assert_eq!(info.active_clients, 1),
                other => panic!("expected Running while P2P client connected, got {other:?}"),
            }

            handle.set_banned_p2p_peers([format!("p2p:{client_id}")]);

            let mut drop_buf = [0u8; 8];
            let drop_res = timeout(Duration::from_secs(2), tcp_client.read(&mut drop_buf))
                .await
                .expect("banned P2P connection must drop immediately");
            assert!(
                matches!(drop_res, Ok(0) | Err(_)),
                "expected EOF or error after P2P ban, got {drop_res:?}"
            );

            let wait_clear = Instant::now() + Duration::from_secs(3);
            loop {
                let info_clients = match handle.current_status() {
                    DirectHostStatus::Running(info) => info.active_clients,
                    _ => usize::MAX,
                };
                if active_clients.load(Ordering::SeqCst) == 0
                    && info_clients == 0
                    && handle.resolve_bridged_peer_target(outbound_port).is_none()
                {
                    break;
                }
                assert!(
                    Instant::now() < wait_clear,
                    "active_clients / bridge_peers did not clear after ban"
                );
                tokio::time::sleep(Duration::from_millis(15)).await;
            }

            drop(tcp_client);
            bridge.stop();
            drop(bridge);

            let banned_probe =
                probe_p2p_addr_reachability(host_addr.clone(), Some(&client_key_path))
                    .await
                    .expect_err("banned peer probe must be rejected");
            assert!(
                banned_probe.contains("cấm (Ban)"),
                "unexpected ban probe error: {banned_probe}"
            );

            let banned_bridge =
                start_p2p_client_bridge_addr(host_addr.clone(), Some(&client_key_path))
                    .await
                    .expect_err("banned peer bridge start must be rejected");
            assert!(
                banned_bridge.to_string().contains("cấm (Ban)"),
                "unexpected ban bridge error: {banned_bridge}"
            );

            handle.set_banned_p2p_peers(std::iter::empty::<String>());
            probe_p2p_addr_reachability(host_addr, Some(&client_key_path))
                .await
                .expect("unbanned peer probe must succeed again");

            cancel.cancel();
            timeout(Duration::from_secs(5), host_join)
                .await
                .expect("host join timeout")
                .expect("host task clean exit");

            let closed_tcp = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let closed_port = closed_tcp.local_addr().unwrap().port();
            drop(closed_tcp);

            let down_cancel = CancellationToken::new();
            let down_status = Arc::new(RwLock::new(DirectHostStatus::Running(
                DirectHostActiveInfo {
                    local_port: closed_port,
                    host_port: closed_port,
                    lan_endpoints: Vec::new(),
                    vpn_endpoints: Vec::new(),
                    ipv6_endpoint: None,
                    ipv6_pinhole_status: Ipv6PinholeStatus::Disabled,
                    ipv6_reachability: ExternalReachability::NotChecked,
                    upnp_status: UpnpPortStatus::Disabled,
                    ipv4_reachability: ExternalReachability::NotChecked,
                    bridging_tcp: false,
                    active_clients: 0,
                    p2p_status: P2pHostStatus::Starting,
                },
            )));
            let (down_tx, down_rx) = oneshot::channel();
            let down_join = spawn_p2p_host_worker_inner(
                closed_port,
                host_key,
                down_cancel.clone(),
                Arc::new(AtomicUsize::new(0)),
                down_status,
                RepaintHandle::default(),
                Arc::new(RwLock::new(HashSet::new())),
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(RwLock::new(HashMap::new())),
                false,
                Some(down_tx),
            );
            let down_addr = timeout(Duration::from_secs(5), down_rx)
                .await
                .expect("down addr timeout")
                .expect("down addr channel");
            let down_err = start_p2p_client_bridge_addr(down_addr, Some(&client_key_path))
                .await
                .expect_err("probe against closed upstream must fail");
            assert!(
                down_err.to_string().contains("đang tắt hoặc chưa sẵn sàng"),
                "unexpected upstream-unavailable error: {down_err}"
            );
            down_cancel.cancel();
            timeout(Duration::from_secs(5), down_join)
                .await
                .expect("down host join timeout")
                .expect("down host task clean exit");

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    #[ignore = "requires live internet access to n0 relays"]
    fn live_n0_p2p_relay_tunnel_bridges_tcp_and_probes_cleanly() {
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
                        let mut buf = [0u8; 64];
                        while let Ok(n) = sock.read(&mut buf).await {
                            if n == 0 || sock.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    });
                }
            });

            let host_key = iroh::SecretKey::generate();
            let dir =
                std::env::temp_dir().join(format!("tl_p2p_live_{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let client_key_path = dir.join("client.key");
            let client_key = load_or_create_p2p_secret_key(Some(&client_key_path));
            let client_id = client_key.public().to_string();

            let cancel = CancellationToken::new();
            let active_clients = Arc::new(AtomicUsize::new(0));
            let status = Arc::new(RwLock::new(DirectHostStatus::Running(
                DirectHostActiveInfo {
                    local_port: upstream_port,
                    host_port: upstream_port,
                    lan_endpoints: Vec::new(),
                    vpn_endpoints: Vec::new(),
                    ipv6_endpoint: None,
                    ipv6_pinhole_status: Ipv6PinholeStatus::Disabled,
                    ipv6_reachability: ExternalReachability::NotChecked,
                    upnp_status: UpnpPortStatus::Disabled,
                    ipv4_reachability: ExternalReachability::NotChecked,
                    bridging_tcp: false,
                    active_clients: 0,
                    p2p_status: P2pHostStatus::Starting,
                },
            )));
            let banned_p2p_peers = Arc::new(RwLock::new(HashSet::new()));
            let active_p2p_conns = Arc::new(Mutex::new(Vec::new()));
            let bridge_peers = Arc::new(RwLock::new(HashMap::new()));
            let cleanup_done = Arc::new((Mutex::new(false), Condvar::new()));

            let handle = DirectPortHostHandle::new_for_test(
                cancel.clone(),
                Arc::clone(&status),
                Arc::new(RwLock::new(HashSet::new())),
                Arc::clone(&banned_p2p_peers),
                Arc::clone(&active_p2p_conns),
                Arc::clone(&bridge_peers),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(true)),
                Arc::clone(&cleanup_done),
                Arc::new(AtomicBool::new(false)),
            );

            let host_join = spawn_p2p_host_worker(
                upstream_port,
                host_key.clone(),
                cancel.clone(),
                Arc::clone(&active_clients),
                Arc::clone(&status),
                RepaintHandle::default(),
                Arc::clone(&banned_p2p_peers),
                Arc::clone(&active_p2p_conns),
                Arc::clone(&bridge_peers),
            );

            let wait_running = Instant::now() + Duration::from_secs(15);
            let ticket = loop {
                if let DirectHostStatus::Running(info) = handle.current_status() {
                    if let P2pHostStatus::Running {
                        ref ticket,
                        ref endpoint_id,
                        relay_url: Some(_),
                    } = info.p2p_status
                    {
                        assert!(is_p2p_ticket(ticket));
                        assert_eq!(ticket, &encode_p2p_ticket(&host_key.public(), None));
                        assert_eq!(endpoint_id, &host_key.public().to_string());
                        break ticket.clone();
                    }
                }
                assert!(
                    Instant::now() < wait_running,
                    "live P2pHostStatus did not become Running with relay within 15s"
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            };

            probe_p2p_ticket_reachability(&ticket, Some(&client_key_path))
                .await
                .expect("live n0 probe must succeed");

            let bridge = start_p2p_client_bridge(&ticket, Some(&client_key_path))
                .await
                .expect("live n0 bridge must start");

            const PAYLOAD: &[u8] = b"TL-LIVE-N0-PING";
            let mut tcp_client = TcpStream::connect((Ipv4Addr::LOCALHOST, bridge.local_port()))
                .await
                .expect("must connect to local live P2P client bridge");
            tcp_client.write_all(PAYLOAD).await.unwrap();
            let mut reply = [0u8; PAYLOAD.len()];
            timeout(Duration::from_secs(8), tcp_client.read_exact(&mut reply))
                .await
                .expect("read timeout")
                .expect("read pong");
            assert_eq!(&reply, PAYLOAD);

            let outbound_port = timeout(Duration::from_secs(5), peer_port_rx.recv())
                .await
                .expect("outbound port timeout")
                .expect("outbound port");
            assert_eq!(
                handle.resolve_bridged_peer_target(outbound_port),
                Some(BridgedPeerTarget::P2p(client_id))
            );
            assert_eq!(active_clients.load(Ordering::SeqCst), 1);

            drop(tcp_client);
            bridge.stop();
            drop(bridge);
            cancel.cancel();
            timeout(Duration::from_secs(5), host_join)
                .await
                .expect("host join timeout")
                .expect("host task clean exit");

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn stable_host_ticket_persists_across_restarts_and_relay_changes() {
        let key = iroh::SecretKey::from_bytes(&[0x5Au8; 32]);
        let endpoint_id = key.public();
        let expected_ticket = encode_p2p_ticket(&endpoint_id, None);
        let expected_short = format_p2p_ticket_short(&expected_ticket);
        assert!(expected_ticket.ends_with("AA"));

        let status = Arc::new(RwLock::new(DirectHostStatus::Running(
            DirectHostActiveInfo {
                local_port: 7777,
                host_port: 7777,
                lan_endpoints: Vec::new(),
                vpn_endpoints: Vec::new(),
                ipv6_endpoint: None,
                ipv6_pinhole_status: Ipv6PinholeStatus::Disabled,
                ipv6_reachability: ExternalReachability::NotChecked,
                upnp_status: UpnpPortStatus::Disabled,
                ipv4_reachability: ExternalReachability::NotChecked,
                bridging_tcp: false,
                active_clients: 0,
                p2p_status: P2pHostStatus::Starting,
            },
        )));
        let wake_ui = RepaintHandle::default();

        for relay_str in [
            "https://aps1-1.relay.n0.iroh.link./",
            "https://usw1-1.relay.n0.iroh.link./",
            "https://euc1-1.relay.n0.iroh.link./",
        ] {
            let relay: iroh::RelayUrl = relay_str.parse().unwrap();
            let addr = iroh::EndpointAddr::new(endpoint_id).with_relay_url(relay.clone());
            publish_online_relay_status(&endpoint_id, &expected_ticket, &addr, &status, &wake_ui);

            let guard = status.read().unwrap();
            let DirectHostStatus::Running(ref info) = *guard else {
                panic!("expected DirectHostStatus::Running");
            };
            let P2pHostStatus::Running {
                ref ticket,
                endpoint_id: ref reported_id,
                ref relay_url,
            } = info.p2p_status
            else {
                panic!("expected P2pHostStatus::Running");
            };
            assert_eq!(ticket, &expected_ticket);
            assert_eq!(format_p2p_ticket_short(ticket), expected_short);
            assert_eq!(reported_id, &endpoint_id.to_string());
            assert_eq!(relay_url.as_deref(), Some(relay.as_str()));
        }

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let dir = std::env::temp_dir().join(format!("tl_p2p_stable_{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let key_path = dir.join("p2p_secret.key");

            let mut observed_tickets = Vec::with_capacity(2);
            let mut expected_from_disk = None;
            for _ in 0..2 {
                let loaded_key = load_or_create_p2p_secret_key(Some(&key_path));
                let expected_disk_ticket = encode_p2p_ticket(&loaded_key.public(), None);
                if let Some(ref prev) = expected_from_disk {
                    assert_eq!(prev, &expected_disk_ticket);
                } else {
                    expected_from_disk = Some(expected_disk_ticket.clone());
                }

                let cancel = CancellationToken::new();
                let run_status = Arc::new(RwLock::new(DirectHostStatus::Running(
                    DirectHostActiveInfo {
                        local_port: 7777,
                        host_port: 7777,
                        lan_endpoints: Vec::new(),
                        vpn_endpoints: Vec::new(),
                        ipv6_endpoint: None,
                        ipv6_pinhole_status: Ipv6PinholeStatus::Disabled,
                        ipv6_reachability: ExternalReachability::NotChecked,
                        upnp_status: UpnpPortStatus::Disabled,
                        ipv4_reachability: ExternalReachability::NotChecked,
                        bridging_tcp: false,
                        active_clients: 0,
                        p2p_status: P2pHostStatus::Starting,
                    },
                )));
                let host_join = spawn_p2p_host_worker_inner(
                    7777,
                    loaded_key,
                    cancel.clone(),
                    Arc::new(AtomicUsize::new(0)),
                    Arc::clone(&run_status),
                    RepaintHandle::default(),
                    Arc::new(RwLock::new(HashSet::new())),
                    Arc::new(Mutex::new(Vec::new())),
                    Arc::new(RwLock::new(HashMap::new())),
                    false,
                    None,
                );

                let deadline = Instant::now() + Duration::from_secs(3);
                let ticket = loop {
                    if let DirectHostStatus::Running(ref info) = *run_status.read().unwrap() {
                        if let P2pHostStatus::Running { ref ticket, .. } = info.p2p_status {
                            break ticket.clone();
                        }
                    }
                    assert!(
                        Instant::now() < deadline,
                        "P2pHostStatus did not become Running"
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                };

                assert_eq!(ticket, expected_disk_ticket);
                observed_tickets.push(ticket);

                cancel.cancel();
                timeout(Duration::from_secs(5), host_join)
                    .await
                    .expect("host join timeout")
                    .expect("host task clean exit");
            }

            assert_eq!(observed_tickets[0], observed_tickets[1]);
            let _ = std::fs::remove_dir_all(&dir);
        });
    }
}
