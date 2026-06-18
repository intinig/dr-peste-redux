use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::header::{self, HeaderValue};
use secrecy::{ExposeSecret, SecretString};

use crate::trade::client::{TRADE_BASE, USER_AGENT};

/// Operator-configured IPRoyal residential proxy.
/// No `Debug` derive: `pass` is an operator secret.
#[derive(Clone)]
pub struct ProxyConfig {
    pub gateway: String, // host:port, e.g. "geo.iproyal.com:12321"
    pub user: String,
    pub pass: String,       // operator secret
    pub country: String,    // ISO-2 lowercase, e.g. "us"
    pub lifetime_mins: u64, // sticky IP lifetime (IPRoyal max 7d)
}

/// Deterministic 8-char alphanumeric session id for a member (IPRoyal requires
/// exactly 8). FNV-1a 32-bit over the user id → zero-padded lowercase hex.
/// Stable across Rust releases (unlike `DefaultHasher`).
pub fn sticky_session_id(user_id: u64) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for b in user_id.to_le_bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

/// Builds the IPRoyal sticky-session SOCKS5 URL. Params attach to the PASSWORD.
/// `user` and base `pass` are percent-encoded (reqwest decodes the userinfo back
/// to the SOCKS5 credentials); the literal `_country-…_session-…_lifetime-…`
/// suffix is appended after encoding.
pub fn sticky_proxy_url(cfg: &ProxyConfig, user_id: u64) -> String {
    let user = utf8_percent_encode(&cfg.user, NON_ALPHANUMERIC);
    let pass = utf8_percent_encode(&cfg.pass, NON_ALPHANUMERIC);
    let sid = sticky_session_id(user_id);
    format!(
        "socks5h://{user}:{pass}_country-{country}_session-{sid}_lifetime-{life}m@{gw}",
        country = cfg.country,
        life = cfg.lifetime_mins,
        gw = cfg.gateway,
    )
}

/// Per-call session injected into the trade call chain.
/// No `Debug`: holds the member cookie.
pub struct TradeSession {
    pub client: Arc<reqwest::Client>,
    pub cookie: Arc<SecretString>,
}

impl TradeSession {
    /// Offline test handle: a default client + dummy secret.
    pub fn for_test() -> TradeSession {
        TradeSession {
            client: Arc::new(reqwest::Client::new()),
            cookie: Arc::new(SecretString::new("test-cookie".to_string())),
        }
    }
}

struct MemberSession {
    cookie: Arc<SecretString>,
    captured_at: Instant,
}

/// In-memory, per-member session store + per-member proxy-bound client cache.
/// No `Debug`: holds member cookies.
pub struct MemberSessions {
    sessions: RwLock<HashMap<u64, MemberSession>>,
    clients: RwLock<HashMap<u64, Arc<reqwest::Client>>>,
    proxy: Option<ProxyConfig>,
    ttl: Duration,
}

impl MemberSessions {
    pub fn new(proxy: Option<ProxyConfig>, ttl: Duration) -> Self {
        MemberSessions {
            sessions: RwLock::new(HashMap::new()),
            clients: RwLock::new(HashMap::new()),
            proxy,
            ttl,
        }
    }

    /// A member's proxy-bound reqwest client (built once, cached). The cookie is
    /// NOT baked in here — it is applied per request (see `TradeApi`).
    fn build_client(&self, user_id: u64) -> Result<Arc<reqwest::Client>> {
        if let Some(c) = self.clients.read().unwrap().get(&user_id) {
            return Ok(c.clone());
        }
        let mut builder = reqwest::Client::builder().user_agent(USER_AGENT);
        if let Some(p) = &self.proxy {
            let url = sticky_proxy_url(p, user_id);
            builder = builder.proxy(reqwest::Proxy::all(url).context("invalid proxy url")?);
        }
        let client = Arc::new(builder.build().context("build member client")?);
        self.clients
            .write()
            .unwrap()
            .insert(user_id, client.clone());
        Ok(client)
    }

    pub fn has_live_session(&self, user_id: u64) -> bool {
        self.sessions
            .read()
            .unwrap()
            .get(&user_id)
            .is_some_and(|s| s.captured_at.elapsed() < self.ttl)
    }

    /// Validate connectivity (proxy reachable + trade responds) through the
    /// member's client with the candidate cookie, then store it. On any failure
    /// nothing is stored and a member-safe error is returned.
    pub async fn store(&self, user_id: u64, cookie: SecretString) -> Result<()> {
        let client = self.build_client(user_id)?;
        let url = format!("{TRADE_BASE}/data/leagues");
        let mut hv = HeaderValue::from_str(&format!("POESESSID={}", cookie.expose_secret()))
            .context("invalid cookie value")?;
        hv.set_sensitive(true);
        let resp = client
            .get(&url)
            .header(header::COOKIE, hv)
            .send()
            .await
            .context("couldn't reach trade through the proxy")?;
        resp.error_for_status()
            .context("trade rejected the request")?;
        self.sessions.write().unwrap().insert(
            user_id,
            MemberSession {
                cookie: Arc::new(cookie),
                captured_at: Instant::now(),
            },
        );
        Ok(())
    }

    pub fn session_for(&self, user_id: u64) -> Option<TradeSession> {
        let guard = self.sessions.read().unwrap();
        let s = guard.get(&user_id)?;
        if s.captured_at.elapsed() >= self.ttl {
            return None;
        }
        let cookie = s.cookie.clone();
        drop(guard);
        let client = self.build_client(user_id).ok()?;
        Some(TradeSession { client, cookie })
    }

    pub fn forget(&self, user_id: u64) {
        self.sessions.write().unwrap().remove(&user_id);
        self.clients.write().unwrap().remove(&user_id);
    }
}

#[cfg(test)]
impl MemberSessions {
    /// Test-only insert that skips network validation and lets the test control
    /// `captured_at` (for TTL tests).
    fn insert_test(&self, user_id: u64, captured_at: Instant) {
        self.sessions.write().unwrap().insert(
            user_id,
            MemberSession {
                cookie: Arc::new(SecretString::new("test-cookie".to_string())),
                captured_at,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn cfg() -> ProxyConfig {
        ProxyConfig {
            gateway: "geo.iproyal.com:12321".into(),
            user: "myuser".into(),
            pass: "mypass".into(),
            country: "us".into(),
            lifetime_mins: 30,
        }
    }

    #[test]
    fn session_id_is_8_hex_and_deterministic() {
        let a = sticky_session_id(123);
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, sticky_session_id(123)); // stable
        assert_ne!(a, sticky_session_id(124)); // differs per id
    }

    #[test]
    fn proxy_url_has_iproyal_shape() {
        let url = sticky_proxy_url(&cfg(), 123);
        let sid = sticky_session_id(123);
        assert_eq!(
            url,
            format!("socks5h://myuser:mypass_country-us_session-{sid}_lifetime-30m@geo.iproyal.com:12321")
        );
        // params attach to the password (after the first ':')
        let pass_part = url.split_once(':').unwrap().1; // strip "socks5h"
        assert!(pass_part.contains("mypass_country-us_session-"));
    }

    #[test]
    fn proxy_url_percent_encodes_special_chars() {
        let mut c = cfg();
        c.pass = "p@ss:w/rd".into();
        let url = sticky_proxy_url(&c, 1);
        assert!(!url.contains("p@ss:w/rd")); // raw special chars not present
        assert!(url.contains("_country-us_session-")); // suffix still literal
    }

    fn registry(ttl: Duration) -> MemberSessions {
        MemberSessions::new(None, ttl)
    }

    #[test]
    fn store_present_then_forgotten() {
        let r = registry(Duration::from_secs(3600));
        r.insert_test(7, Instant::now());
        assert!(r.has_live_session(7));
        assert!(r.session_for(7).is_some());
        r.forget(7);
        assert!(!r.has_live_session(7));
        assert!(r.session_for(7).is_none());
    }

    #[test]
    fn expired_session_is_not_live() {
        let r = registry(Duration::ZERO); // ttl 0 ⇒ anything is already expired
        r.insert_test(9, Instant::now());
        assert!(!r.has_live_session(9));
        assert!(r.session_for(9).is_none());
    }

    #[test]
    fn builds_a_proxied_client_without_panicking() {
        let r = MemberSessions::new(Some(cfg()), Duration::from_secs(60));
        assert!(r.build_client(42).is_ok());
    }
}
