use reqwest::Client;
use std::sync::Once;
use std::time::Duration;
use tl_core::{LauncherError, LauncherResult};

static RUSTLS_INIT: Once = Once::new();
pub const LAUNCHER_USER_AGENT: &str = "TerrariaLauncher/1.0";

pub fn ensure_rustls_provider() {
    RUSTLS_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub fn normalize_proxy_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains("://") {
        Some(trimmed.to_string())
    } else {
        Some(format!("http://{trimmed}"))
    }
}

pub fn build_http_client(proxy_url: Option<&str>) -> LauncherResult<Client> {
    ensure_rustls_provider();

    let mut builder = Client::builder()
        .user_agent(LAUNCHER_USER_AGENT)
        .connect_timeout(Duration::from_secs(12))
        .timeout(Duration::from_secs(60));

    if let Some(proxy_str) = proxy_url.and_then(normalize_proxy_url) {
        let proxy = reqwest::Proxy::all(&proxy_str)
            .map_err(|e| LauncherError::InvalidInput(format!("Proxy không hợp lệ: {e}")))?;
        builder = builder.proxy(proxy);
    }

    builder
        .build()
        .map_err(|e| LauncherError::Network(format!("Không thể khởi tạo HTTP client: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_proxy_url_handles_empty_bare_and_schemed_values() {
        assert_eq!(normalize_proxy_url(""), None);
        assert_eq!(normalize_proxy_url("   \t\n "), None);
        assert_eq!(
            normalize_proxy_url("127.0.0.1:7890"),
            Some("http://127.0.0.1:7890".to_string())
        );
        assert_eq!(
            normalize_proxy_url("  127.0.0.1:7890  "),
            Some("http://127.0.0.1:7890".to_string())
        );
        assert_eq!(
            normalize_proxy_url("http://127.0.0.1:7890"),
            Some("http://127.0.0.1:7890".to_string())
        );
        assert_eq!(
            normalize_proxy_url("socks5://127.0.0.1:10808"),
            Some("socks5://127.0.0.1:10808".to_string())
        );
    }

    #[test]
    fn build_http_client_accepts_normalized_proxies_and_rejects_invalid() {
        assert!(build_http_client(None).is_ok());
        assert!(build_http_client(Some("   ")).is_ok());
        assert!(build_http_client(Some("127.0.0.1:7890")).is_ok());
        assert!(build_http_client(Some("socks5://127.0.0.1:10808")).is_ok());
        assert!(matches!(
            build_http_client(Some("://invalid-proxy")),
            Err(LauncherError::InvalidInput(_))
        ));
    }
}
