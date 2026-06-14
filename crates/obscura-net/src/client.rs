use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use std::net::{IpAddr, SocketAddr};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use reqwest::redirect::Policy;
use reqwest::{Client, Method};
use tokio::sync::RwLock;
use url::Url;

use crate::cookies::CookieJar;
use crate::interceptor::{InterceptAction, RequestInterceptor};

#[derive(Debug, Clone)]
pub struct Response {
    pub url: Url,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub redirected_from: Vec<Url>,
}

impl Response {
    /// Decode the body as text, honoring the response charset.
    ///
    /// Uses the HTTP `Content-Type` header's `charset=` parameter, then for
    /// HTML responses falls back to sniffing `<meta charset>` in the first
    /// 1KB, then UTF-8. Mirrors browser behaviour per the HTML5 spec.
    pub fn text(&self) -> String {
        if self.is_html() {
            crate::encoding::decode_response(&self.body, self.content_type())
        } else {
            crate::encoding::decode_non_html(&self.body, self.content_type())
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_lowercase()).map(|s| s.as_str())
    }

    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
    }

    pub fn is_html(&self) -> bool {
        self.content_type()
            .map(|ct| ct.contains("text/html"))
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub struct RequestInfo {
    pub url: Url,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub resource_type: ResourceType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceType {
    Document,
    Script,
    Stylesheet,
    Image,
    Font,
    Xhr,
    Fetch,
    Other,
}

pub type RequestCallback = Arc<dyn Fn(&RequestInfo) + Send + Sync>;
pub type ResponseCallback = Arc<dyn Fn(&RequestInfo, &Response) + Send + Sync>;

/// Process-wide opt-in via env var. Older flow that issue #4 introduced. The
/// new `--allow-private-network` CLI flag (issue #33) sets a per-client field
/// that is OR'd with this so existing scripts and Docker setups that pin the
/// env var keep working unchanged.
pub fn env_allows_private_network() -> bool {
    matches!(
        std::env::var("OBSCURA_ALLOW_PRIVATE_NETWORK")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// True when `ip` must never be the target of an outbound request from the
/// engine: loopback, RFC1918 private, link-local (incl. the 169.254.169.254
/// cloud-metadata endpoint), broadcast, documentation, the unspecified address
/// (0.0.0.0 / ::, which the OS routes to localhost), IPv6 unique-local
/// (fc00::/7), and any IPv4-mapped/compatible IPv6 form of the above.
/// Centralizes the SSRF deny-set so the literal-host check and the
/// DNS-resolution check (`SsrfGuardResolver`) can never disagree.
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
            {
                return true;
            }
            // Unwrap IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d)
            // forms and re-check the embedded v4 so e.g. [::ffff:127.0.0.1] or
            // [::ffff:169.254.169.254] cannot slip past the v6 arm.
            if let Some(v4) = v6.to_ipv4_mapped().or_else(|| v6.to_ipv4()) {
                return is_forbidden_ip(IpAddr::V4(v4));
            }
            false
        }
    }
}

/// reqwest DNS resolver that performs the lookup and then rejects the whole
/// request if ANY resolved address is in the SSRF deny-set. This closes the
/// DNS-rebinding bypass a host-string check alone cannot: a public name that
/// resolves to 127.0.0.1 / 169.254.169.254 / an RFC1918 address is blocked at
/// connect time, using the very addresses reqwest will dial. When private
/// access is permitted (`--allow-private-network` or
/// `OBSCURA_ALLOW_PRIVATE_NETWORK`) the lookup passes through unfiltered.
pub struct SsrfGuardResolver {
    allow_private: bool,
}

impl SsrfGuardResolver {
    pub fn new(allow_private: bool) -> Self {
        Self { allow_private }
    }
}

impl Resolve for SsrfGuardResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow = self.allow_private || env_allows_private_network();
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .collect();
            if !allow {
                if let Some(bad) = addrs.iter().find(|sa| is_forbidden_ip(sa.ip())) {
                    return Err(format!(
                        "SSRF blocked: '{}' resolves to forbidden address {}",
                        host,
                        bad.ip()
                    )
                    .into());
                }
            }
            let iter: Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

fn validate_url(url: &Url, allow_private_network: bool) -> Result<(), ObscuraNetError> {
    let allow_private_network = allow_private_network || env_allows_private_network();
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" && scheme != "file" {
        return Err(ObscuraNetError::Network(format!(
            "Forbidden URL scheme '{}' - only http, https, and file are allowed",
            scheme
        )));
    }

    if scheme == "file" || allow_private_network {
        return Ok(());
    }

    if let Some(host) = url.host() {
        match host {
            url::Host::Ipv4(ip) => {
                if is_forbidden_ip(IpAddr::V4(ip)) {
                    return Err(ObscuraNetError::Network(format!(
                        "Access to private/internal IP address {} is not allowed",
                        ip
                    )));
                }
            }
            url::Host::Ipv6(ip) => {
                if is_forbidden_ip(IpAddr::V6(ip)) {
                    return Err(ObscuraNetError::Network(format!(
                        "Access to private/internal IPv6 address {} is not allowed",
                        ip
                    )));
                }
            }
            url::Host::Domain(domain) => {
                let lower_domain = domain.to_lowercase();
                if lower_domain == "localhost"
                    || lower_domain.ends_with(".localhost")
                    || lower_domain == "127.0.0.1"
                    || lower_domain == "::1"
                {
                    return Err(ObscuraNetError::Network(format!(
                        "Access to localhost domain '{}' is not allowed",
                        domain
                    )));
                }
            }
        }
    }

    Ok(())
}

async fn fetch_file_url(url: &Url) -> Result<Response, ObscuraNetError> {
    let path = url
        .to_file_path()
        .map_err(|_| ObscuraNetError::Network("Invalid file URL".to_string()))?;
    let body = tokio::fs::read(&path)
        .await
        .map_err(|e| ObscuraNetError::Network(format!("Failed to read file: {}", e)))?;

    let mut headers = HashMap::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ct = match ext.to_lowercase().as_str() {
            "html" | "htm" => "text/html",
            "css" => "text/css",
            "js" | "mjs" => "application/javascript",
            "json" => "application/json",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "webp" => "image/webp",
            "ico" => "image/x-icon",
            _ => "application/octet-stream",
        };
        headers.insert("content-type".to_string(), ct.to_string());
    }

    Ok(Response {
        url: url.clone(),
        status: 200,
        headers,
        body,
        redirected_from: Vec::new(),
    })
}

/// Load every certificate from a PEM CA bundle at `path` into reqwest
/// `Certificate`s, ready to be registered as extra trusted roots.
///
/// Fail-closed: a missing file, an unreadable file, or a file with no valid
/// PEM certificate all return an `Err`. This is intentional — a deployment
/// that configured a CA but cannot load it must surface the error, never
/// silently proceed with the default root store only (the lane governed-egress
/// seam relies on the custom root actually being trusted).
///
/// Validation is NOT weakened: these certs are *added* to the trust anchor
/// set. `danger_accept_invalid_certs` stays `false` on the client builder.
pub fn load_ca_certs(path: &str) -> Result<Vec<reqwest::Certificate>, ObscuraNetError> {
    let pem = std::fs::read(path)
        .map_err(|e| ObscuraNetError::Network(format!("failed to read CA bundle '{path}': {e}")))?;

    // reqwest 0.12 parses a whole bundle (one or many concatenated PEM blocks)
    // in one call. Each `-----BEGIN CERTIFICATE-----` block becomes one cert.
    let certs = reqwest::Certificate::from_pem_bundle(&pem)
        .map_err(|e| ObscuraNetError::Network(format!("invalid CA bundle '{path}': {e}")))?;

    if certs.is_empty() {
        return Err(ObscuraNetError::Network(format!(
            "CA bundle '{path}' contained no certificates"
        )));
    }

    Ok(certs)
}

pub struct ObscuraHttpClient {
    client: tokio::sync::OnceCell<Client>,
    proxy_url: Option<String>,
    /// Path to a PEM CA bundle whose certificates are ADDED to the client's
    /// trust store (issue: lane governed-egress seam). When set, every cert in
    /// the file is loaded via `reqwest::Certificate` and registered as an extra
    /// trusted root. This never weakens validation: `danger_accept_invalid_certs`
    /// stays `false`; we are widening the trust anchor set, not disabling checks.
    /// Lets obscura run behind a TLS-terminating governed proxy (e.g. `lane`)
    /// that re-signs upstream certs with its own local CA.
    ca_path: Option<String>,
    pub cookie_jar: Arc<CookieJar>,
    pub user_agent: RwLock<String>,
    pub extra_headers: RwLock<HashMap<String, String>>,
    pub interceptor: RwLock<Option<Box<dyn RequestInterceptor + Send + Sync>>>,
    pub on_request: RwLock<Vec<RequestCallback>>,
    pub on_response: RwLock<Vec<ResponseCallback>>,
    pub timeout: Duration,
    pub in_flight: Arc<std::sync::atomic::AtomicU32>,
    pub block_trackers: bool,
    /// When true, `validate_url` lets localhost / RFC1918 / link-local addresses
    /// through in addition to the `OBSCURA_ALLOW_PRIVATE_NETWORK` env var.
    /// Set via `--allow-private-network` on the CLI (issue #33).
    pub allow_private_network: bool,
}

impl ObscuraHttpClient {
    pub fn new() -> Self {
        Self::with_cookie_jar(Arc::new(CookieJar::new()))
    }

    pub fn with_cookie_jar(cookie_jar: Arc<CookieJar>) -> Self {
        Self::with_options(cookie_jar, None)
    }

    pub fn with_options(cookie_jar: Arc<CookieJar>, proxy_url: Option<&str>) -> Self {
        Self::with_full_options(cookie_jar, proxy_url, false)
    }

    pub fn with_full_options(
        cookie_jar: Arc<CookieJar>,
        proxy_url: Option<&str>,
        allow_private_network: bool,
    ) -> Self {
        Self::with_options_ca(cookie_jar, proxy_url, allow_private_network, None)
    }

    /// Kitchen-sink constructor that also accepts a custom CA bundle path
    /// (the lane governed-egress seam). Older constructors delegate here with
    /// `ca_path = None`, so existing callers are unaffected. When `ca_path` is
    /// `Some`, the certs in that PEM file are added to the trust store the first
    /// time the underlying reqwest client is built (see `get_client`).
    pub fn with_options_ca(
        cookie_jar: Arc<CookieJar>,
        proxy_url: Option<&str>,
        allow_private_network: bool,
        ca_path: Option<&str>,
    ) -> Self {
        ObscuraHttpClient {
            client: tokio::sync::OnceCell::new(),
            proxy_url: proxy_url.map(|s| s.to_string()),
            ca_path: ca_path.map(|s| s.to_string()),
            cookie_jar,
            user_agent: RwLock::new(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36".to_string(),
            ),
            extra_headers: RwLock::new(HashMap::new()),
            interceptor: RwLock::new(None),
            on_request: RwLock::new(Vec::new()),
            on_response: RwLock::new(Vec::new()),
            in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            timeout: Duration::from_secs(30),
            block_trackers: false,
            allow_private_network,
        }
    }

    async fn get_client(&self) -> Result<&Client, ObscuraNetError> {
        self.client
            .get_or_try_init(|| async {
                let mut builder = Client::builder()
                    .redirect(Policy::none())
                    .timeout(Duration::from_secs(30))
                    .danger_accept_invalid_certs(false)
                    // SSRF guard: reject hostnames that resolve to a private/loopback IP.
                    .dns_resolver(Arc::new(SsrfGuardResolver::new(self.allow_private_network)));

                if let Some(ref proxy) = self.proxy_url {
                    if let Ok(p) = reqwest::Proxy::all(proxy.as_str()) {
                        builder = builder.proxy(p);
                    }
                }

                // Custom CA trust: load the PEM bundle and register each cert as
                // an extra trusted root. Fail-closed on a missing/invalid file —
                // a governed-egress deployment that asked for a CA but cannot load
                // it must NOT silently fall back to the default roots only.
                if let Some(ref path) = self.ca_path {
                    for cert in load_ca_certs(path)? {
                        builder = builder.add_root_certificate(cert);
                    }
                }

                builder.build().map_err(|e| {
                    ObscuraNetError::Network(format!("failed to build HTTP client: {e}"))
                })
            })
            .await
    }

    /// Read-only accessor for the proxy URL the client was configured with
    /// (if any). Exposed so callers outside the `obscura-net` crate — notably
    /// `op_fetch_url` in `obscura-js` (#139) — can route their own reqwest
    /// requests through the same upstream proxy.
    pub fn proxy_url(&self) -> Option<&str> {
        self.proxy_url.as_deref()
    }

    /// Read-only accessor for the custom CA bundle path the client was
    /// configured with (if any). Mirrors [`proxy_url`](Self::proxy_url) so
    /// callers that build their OWN reqwest client off this client's config —
    /// notably `op_fetch_url` in `obscura-js` — route requests through the same
    /// trusted-root set, not just the same proxy.
    pub fn ca_path(&self) -> Option<&str> {
        self.ca_path.as_deref()
    }

    pub async fn fetch(&self, url: &Url) -> Result<Response, ObscuraNetError> {
        self.fetch_with_method(Method::GET, url, None).await
    }

    pub async fn post_form(&self, url: &Url, body: &str) -> Result<Response, ObscuraNetError> {
        self.fetch_with_method(Method::POST, url, Some(body.as_bytes().to_vec()))
            .await
    }

    pub async fn fetch_with_method(
        &self,
        initial_method: Method,
        url: &Url,
        initial_body: Option<Vec<u8>>,
    ) -> Result<Response, ObscuraNetError> {
        validate_url(url, self.allow_private_network)?;

        if url.scheme() == "file" {
            return fetch_file_url(url).await;
        }

        let mut method = initial_method;
        let mut body = initial_body;
        if self.block_trackers {
            if let Some(host) = url.host_str() {
                if crate::blocklist::is_blocked(host) {
                    tracing::debug!("Blocked tracker: {}", url);
                    return Ok(Response {
                        status: 0,
                        url: url.clone(),
                        headers: HashMap::new(),
                        body: Vec::new(),
                        redirected_from: Vec::new(),
                    });
                }
            }
        }

        let mut current_url = url.clone();
        let mut redirects = Vec::new();
        let max_redirects = 20;

        for _redirect_count in 0..max_redirects {
            let request_info = RequestInfo {
                url: current_url.clone(),
                method: method.to_string(),
                headers: self.extra_headers.read().await.clone(),
                resource_type: ResourceType::Document,
            };

            if let Some(interceptor) = self.interceptor.read().await.as_ref() {
                match interceptor.intercept(&request_info).await {
                    InterceptAction::Continue => {}
                    InterceptAction::Block => {
                        return Err(ObscuraNetError::Blocked(current_url.to_string()));
                    }
                    InterceptAction::Fulfill(response) => {
                        return Ok(response);
                    }
                    InterceptAction::ModifyHeaders(headers) => {
                        let mut extra = self.extra_headers.write().await;
                        extra.extend(headers);
                    }
                }
            }

            for cb in self.on_request.read().await.iter() {
                cb(&request_info);
            }

            let ua = self.user_agent.read().await.clone();
            let mut headers = HeaderMap::new();
            headers.insert(USER_AGENT, HeaderValue::from_str(&ua).unwrap_or_else(|_| {
                HeaderValue::from_static("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36")
            }));
            headers.insert(
                reqwest::header::ACCEPT,
                HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"),
            );
            headers.insert(
                reqwest::header::ACCEPT_LANGUAGE,
                HeaderValue::from_static("en-US,en;q=0.9"),
            );
            headers.insert(
                HeaderName::from_static("sec-ch-ua"),
                HeaderValue::from_static(
                    "\"Chromium\";v=\"145\", \"Not;A=Brand\";v=\"24\", \"Google Chrome\";v=\"145\"",
                ),
            );
            headers.insert(
                HeaderName::from_static("sec-ch-ua-mobile"),
                HeaderValue::from_static("?0"),
            );
            headers.insert(
                HeaderName::from_static("sec-ch-ua-platform"),
                HeaderValue::from_static("\"Linux\""),
            );
            headers.insert(
                HeaderName::from_static("sec-fetch-dest"),
                HeaderValue::from_static("document"),
            );
            headers.insert(
                HeaderName::from_static("sec-fetch-mode"),
                HeaderValue::from_static("navigate"),
            );
            headers.insert(
                HeaderName::from_static("sec-fetch-site"),
                HeaderValue::from_static("none"),
            );
            headers.insert(
                HeaderName::from_static("sec-fetch-user"),
                HeaderValue::from_static("?1"),
            );
            headers.insert(
                HeaderName::from_static("upgrade-insecure-requests"),
                HeaderValue::from_static("1"),
            );

            let cookie_header = self.cookie_jar.get_cookie_header(&current_url);
            tracing::debug!(
                "Cookie header for {}: {} cookies ({} bytes)",
                current_url.host_str().unwrap_or("?"),
                cookie_header.split("; ").filter(|s| !s.is_empty()).count(),
                cookie_header.len(),
            );
            if !cookie_header.is_empty() {
                match HeaderValue::from_str(&cookie_header) {
                    Ok(val) => {
                        headers.insert(reqwest::header::COOKIE, val);
                    }
                    Err(_) => {
                        let filtered: String = cookie_header
                            .split("; ")
                            .filter(|pair| HeaderValue::from_str(pair).is_ok())
                            .collect::<Vec<_>>()
                            .join("; ");
                        if !filtered.is_empty() {
                            if let Ok(val) = HeaderValue::from_str(&filtered) {
                                headers.insert(reqwest::header::COOKIE, val);
                            }
                        }
                        tracing::debug!(
                            "Cookie header invalid chars, filtered {} -> {} bytes",
                            cookie_header.len(),
                            filtered.len(),
                        );
                    }
                }
            }

            for (k, v) in self.extra_headers.read().await.iter() {
                if let (Ok(name), Ok(val)) = (
                    HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(v),
                ) {
                    headers.insert(name, val);
                }
            }

            let mut req_builder = self
                .get_client()
                .await?
                .request(method.clone(), current_url.as_str())
                .headers(headers);

            if let Some(ref b) = body {
                if method == Method::POST {
                    req_builder = req_builder.header(
                        reqwest::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    );
                }
                req_builder = req_builder.body(b.clone());
            }

            self.in_flight
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let resp = req_builder.send().await.map_err(|e| {
                self.in_flight
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                ObscuraNetError::Network(format!("{}: {}", current_url, e))
            })?;
            self.in_flight
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

            let status = resp.status();

            for val in resp.headers().get_all(reqwest::header::SET_COOKIE) {
                if let Ok(s) = val.to_str() {
                    self.cookie_jar.set_cookie(s, &current_url);
                }
            }

            let response_headers: HashMap<String, String> = resp
                .headers()
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_lowercase(),
                        v.to_str().unwrap_or("").to_string(),
                    )
                })
                .collect();

            if status.is_redirection() {
                if let Some(location) = resp.headers().get(reqwest::header::LOCATION) {
                    let location_str = location.to_str().map_err(|_| {
                        ObscuraNetError::Network("Invalid redirect Location header".into())
                    })?;
                    let next_url = current_url.join(location_str).map_err(|e| {
                        ObscuraNetError::Network(format!("Invalid redirect URL: {}", e))
                    })?;
                    validate_url(&next_url, self.allow_private_network)?;
                    redirects.push(current_url.clone());
                    current_url = next_url;
                    if status == reqwest::StatusCode::MOVED_PERMANENTLY
                        || status == reqwest::StatusCode::FOUND
                        || status == reqwest::StatusCode::SEE_OTHER
                    {
                        method = Method::GET;
                        body = None;
                    }
                    continue;
                }
            }

            let body_bytes = resp
                .bytes()
                .await
                .map_err(|e| ObscuraNetError::Network(format!("Failed to read body: {}", e)))?
                .to_vec();

            let response = Response {
                url: current_url,
                status: status.as_u16(),
                headers: response_headers,
                body: body_bytes,
                redirected_from: redirects,
            };

            for cb in self.on_response.read().await.iter() {
                cb(&request_info, &response);
            }

            return Ok(response);
        }

        Err(ObscuraNetError::TooManyRedirects(current_url.to_string()))
    }

    pub async fn set_user_agent(&self, ua: &str) {
        *self.user_agent.write().await = ua.to_string();
    }

    pub async fn set_extra_headers(&self, headers: HashMap<String, String>) {
        *self.extra_headers.write().await = headers;
    }

    pub fn active_requests(&self) -> u32 {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn is_network_idle(&self) -> bool {
        self.active_requests() == 0
    }
}

impl Default for ObscuraHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ObscuraNetError {
    #[error("Network error: {0}")]
    Network(String),

    #[error("Too many redirects: {0}")]
    TooManyRedirects(String),

    #[error("Request blocked: {0}")]
    Blocked(String),
}

#[cfg(test)]
mod ssrf_tests {
    use super::{is_forbidden_ip, validate_url, SsrfGuardResolver};
    use reqwest::dns::{Name, Resolve};
    use std::net::IpAddr;
    use std::str::FromStr;
    use url::Url;

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).unwrap()
    }

    #[test]
    fn ipv4_private_and_special_ranges_are_forbidden() {
        for s in [
            "127.0.0.1",
            "127.5.6.7",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254", // cloud-metadata endpoint
            "0.0.0.0",         // unspecified -> localhost (was a bypass)
            "255.255.255.255", // broadcast
            "192.0.2.1",       // documentation
        ] {
            assert!(is_forbidden_ip(ip(s)), "{s} should be forbidden");
        }
    }

    #[test]
    fn public_ipv4_is_allowed() {
        for s in ["1.1.1.1", "8.8.8.8", "93.184.216.34"] {
            assert!(!is_forbidden_ip(ip(s)), "{s} should be allowed");
        }
    }

    #[test]
    fn ipv6_loopback_ula_linklocal_and_mapped_are_forbidden() {
        for s in [
            "::1",                    // loopback
            "::",                     // unspecified
            "fc00::1",                // unique-local (was a bypass)
            "fd12:3456:789a::1",      // unique-local
            "fe80::1",                // link-local
            "::ffff:127.0.0.1",       // v4-mapped loopback (was a bypass)
            "::ffff:169.254.169.254", // v4-mapped metadata
        ] {
            assert!(is_forbidden_ip(ip(s)), "{s} should be forbidden");
        }
    }

    #[test]
    fn public_ipv6_is_allowed() {
        assert!(!is_forbidden_ip(ip("2606:4700:4700::1111"))); // cloudflare dns
    }

    #[test]
    fn validate_url_blocks_unspecified_and_allows_public() {
        // 0.0.0.0 previously slipped through validate_url's literal-host check.
        assert!(validate_url(&Url::parse("http://0.0.0.0:8080/").unwrap(), false).is_err());
        assert!(validate_url(&Url::parse("http://127.0.0.1/").unwrap(), false).is_err());
        assert!(validate_url(&Url::parse("http://example.com/").unwrap(), false).is_ok());
        // The allow flag bypasses the guard (local-dev escape hatch).
        assert!(validate_url(&Url::parse("http://127.0.0.1/").unwrap(), true).is_ok());
    }

    #[tokio::test]
    async fn resolver_blocks_hostname_that_resolves_to_loopback() {
        // localtest.me is a public DNS name that resolves to 127.0.0.1 — the
        // canonical DNS-rebinding test. The guard must reject it. If DNS is
        // unavailable the lookup itself errors (also Err), so the assertion
        // holds either way.
        let r = SsrfGuardResolver::new(false);
        let res = r.resolve(Name::from_str("localtest.me").unwrap()).await;
        assert!(res.is_err(), "localtest.me -> 127.0.0.1 must be blocked");
    }

    #[tokio::test]
    async fn resolver_does_not_ssrf_block_public_host() {
        // A public host must not be SSRF-blocked. Tolerate a no-network sandbox
        // by only failing on an actual SSRF rejection, not a lookup failure.
        let r = SsrfGuardResolver::new(false);
        match r.resolve(Name::from_str("example.com").unwrap()).await {
            Ok(_) => {}
            Err(e) => assert!(
                !e.to_string().contains("SSRF blocked"),
                "example.com wrongly SSRF-blocked: {e}"
            ),
        }
    }
}

// Custom-CA trust (the lane↔obscura governed-egress seam). These tests prove
// the real capability: obscura can be told to trust a custom CA and then
// successfully complete TLS to a server signed by it — WITHOUT weakening
// validation (the no-CA case must still fail with a cert error).
#[cfg(test)]
mod ca_trust_tests {
    use super::{load_ca_certs, ObscuraHttpClient};
    use crate::cookies::CookieJar;
    use std::io::Write;
    use std::sync::Arc;
    use url::Url;

    // ---- Unit tests for the PEM loader (fail-closed contract) ----

    #[test]
    fn load_ca_certs_loads_a_valid_self_signed_pem() {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(cert.cert.pem().as_bytes()).unwrap();
        f.flush().unwrap();

        let certs = load_ca_certs(f.path().to_str().unwrap())
            .expect("a valid PEM CA must load without error");
        assert_eq!(certs.len(), 1, "exactly one cert in the bundle");
    }

    #[test]
    fn load_ca_certs_loads_a_multi_cert_bundle() {
        let a = rcgen::generate_simple_self_signed(vec!["a.test".to_string()]).unwrap();
        let b = rcgen::generate_simple_self_signed(vec!["b.test".to_string()]).unwrap();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}{}", a.cert.pem(), b.cert.pem()).unwrap();
        f.flush().unwrap();

        let certs = load_ca_certs(f.path().to_str().unwrap()).expect("multi-cert bundle must load");
        assert_eq!(certs.len(), 2, "both certs in the bundle are parsed");
    }

    #[test]
    fn load_ca_certs_errors_on_missing_file() {
        let err = load_ca_certs("/nonexistent/obscura-ca-does-not-exist.pem")
            .expect_err("a missing CA file must error (fail-closed)");
        let msg = err.to_string();
        assert!(
            msg.contains("failed to read CA bundle"),
            "error should name the read failure: {msg}"
        );
    }

    #[test]
    fn load_ca_certs_errors_on_garbage_pem() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"this is not a certificate at all").unwrap();
        f.flush().unwrap();

        let err = load_ca_certs(f.path().to_str().unwrap())
            .expect_err("an invalid PEM must error (fail-closed)");
        let msg = err.to_string();
        assert!(
            msg.contains("invalid CA bundle") || msg.contains("contained no certificates"),
            "error should explain the bad bundle: {msg}"
        );
    }

    // ---- Real localhost HTTPS round-trip (does CA trust actually work?) ----

    /// Spin a one-shot HTTPS server on loopback whose leaf is `cert`/`key`,
    /// returning the bound port. The server answers a single request with
    /// `200 OK\r\n\r\nhello-ca`, then exits.
    async fn spawn_https_server(cert_pem: String, key_pem: String) -> u16 {
        use http_body_util::Full;
        use hyper::body::Bytes;
        use hyper::service::service_fn;
        use hyper_util::rt::TokioIo;
        use tokio::net::TcpListener;
        use tokio_rustls::rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
        use tokio_rustls::rustls::ServerConfig;
        use tokio_rustls::TlsAcceptor;

        let certs: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(cert_pem.as_bytes())
                .collect::<Result<_, _>>()
                .unwrap();
        let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).unwrap();

        // reqwest's rustls-tls backend uses the `ring` provider; install it as
        // the process default so the test server's ServerConfig builder can
        // resolve a CryptoProvider too (ignore the error if already installed).
        let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();

        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(config));

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            // Serve a handful of connections so both the with-CA and without-CA
            // probes (and any TLS retry) are answered; the test completes well
            // before this loop would idle out.
            for _ in 0..8 {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let tls = match acceptor.accept(stream).await {
                        Ok(t) => t,
                        // A client that does NOT trust the CA aborts the
                        // handshake here — expected for the negative probe.
                        Err(_) => return,
                    };
                    let io = TokioIo::new(tls);
                    let svc = service_fn(|_req| async {
                        Ok::<_, std::convert::Infallible>(hyper::Response::new(Full::new(
                            Bytes::from_static(b"hello-ca"),
                        )))
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });

        port
    }

    #[tokio::test]
    async fn custom_ca_is_trusted_and_validation_stays_enforced() {
        // Mint a self-signed cert for `localhost`. It is both the leaf the
        // server presents AND the root we ask obscura to trust.
        let issued = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let cert_pem = issued.cert.pem();
        let key_pem = issued.key_pair.serialize_pem();

        let port = spawn_https_server(cert_pem.clone(), key_pem).await;
        // Give the listener a moment to be ready.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let url = Url::parse(&format!("https://localhost:{port}/")).unwrap();

        // Write the CA to a PEM file obscura will load.
        let mut ca_file = tempfile::NamedTempFile::new().unwrap();
        ca_file.write_all(cert_pem.as_bytes()).unwrap();
        ca_file.flush().unwrap();
        let ca_path = ca_file.path().to_str().unwrap().to_string();

        // (1) WITHOUT the custom CA: the fetch must FAIL with a TLS/cert error.
        // This proves we did NOT accidentally disable validation. The server is
        // on loopback, so allow_private_network=true to clear the SSRF guard and
        // isolate the TLS behaviour as the only variable.
        let no_ca = ObscuraHttpClient::with_options_ca(
            Arc::new(CookieJar::new()),
            None,
            true, // allow_private_network (loopback target)
            None, // no custom CA
        );
        let without = no_ca.fetch(&url).await;
        assert!(
            without.is_err(),
            "fetch WITHOUT the custom CA must fail (validation enforced); got {without:?}"
        );

        // (2) WITH the custom CA configured: the same fetch must SUCCEED.
        let with_ca = ObscuraHttpClient::with_options_ca(
            Arc::new(CookieJar::new()),
            None,
            true,
            Some(&ca_path),
        );
        let resp = with_ca
            .fetch(&url)
            .await
            .expect("fetch WITH the custom CA must succeed");
        assert_eq!(resp.status, 200, "served 200 over the trusted TLS channel");
        assert_eq!(
            String::from_utf8_lossy(&resp.body),
            "hello-ca",
            "round-trip body proves real CA-trusted TLS, not a bypass"
        );
    }

    #[tokio::test]
    async fn bogus_ca_path_fails_closed_on_fetch() {
        // A configured-but-unreadable CA must fail the request loudly, never
        // silently fall back to the default root store.
        let client = ObscuraHttpClient::with_options_ca(
            Arc::new(CookieJar::new()),
            None,
            true,
            Some("/nonexistent/obscura-bogus-ca.pem"),
        );
        // example.com is a normally-trusted public host; the ONLY reason this
        // must error is the unreadable CA file (fail-closed). Tolerate a
        // no-network sandbox: a DNS/connect failure is also an Err, which still
        // satisfies "the request did not silently succeed without the CA".
        let url = Url::parse("https://example.com/").unwrap();
        let res = client.fetch(&url).await;
        assert!(
            res.is_err(),
            "a bogus CA path must fail-closed; got {res:?}"
        );
    }
}
