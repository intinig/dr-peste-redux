use std::sync::Arc;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use secrecy::SecretString;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
