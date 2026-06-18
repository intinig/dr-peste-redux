# Per-Member PoE Sessions + Residential Proxy — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route each guild member's rare-item trade2 searches through that member's own POESESSID and a sticky per-member IPRoyal residential IP, captured on first `/paste`.

**Architecture:** A new `src/trade/session.rs` owns an in-memory `MemberSessions` registry (per-member cookie + per-member proxy-bound `reqwest::Client`) and hands out a per-call `TradeSession {client, cookie}`. The trade call chain is refactored from session-baked-at-construction to session-injected-per-call. The Discord layer captures the cookie via a button→modal flow on first use and threads `ctx.author().id` into pricing.

**Tech Stack:** Rust, tokio, poise 0.6 / serenity 0.12, reqwest 0.12 (rustls + socks), `secrecy` (zeroize), `percent-encoding`, anyhow, tracing.

**Design spec:** `docs/superpowers/specs/2026-06-18-per-member-poe-sessions-design.md` (read it for rationale; this plan is self-contained for implementation).

## Global Constraints

- **Async throughout** (tokio). The store is the only shared mutable state; `trade/` has **no Discord knowledge** (`MemberSessions` is keyed by raw `u64`, never a serenity type).
- **Secrets:** members' POESESSIDs use `secrecy::SecretString` (redacted Debug, zeroize-on-drop), stored as `Arc<SecretString>`, materialized into a **sensitive** `HeaderValue` per request, never logged, never in the probe log / breakdown output / any `Debug`/`serde` derive. Operator secrets (`DISCORD_TOKEN`, `POESESSID`, `PROXY_PASS`) stay plain `String`, kept out of the hand-written `Config` Debug.
- **Never commit secrets.** Only `.env.example` is tracked. Stage files by name; never `git add -A`.
- **IPRoyal proxy URL format (verified against docs.iproyal.com):** params attach to the **password**: `socks5h://{user}:{pass}_country-{cc}_session-{sid}_lifetime-{n}m@{host:port}`. `sid` = exactly 8 alphanumeric chars. Lifetime min 1s / max 7d. Omitting `_session-` = rotating.
- **Rate-limit etiquette:** reuse the existing 429 `Retry-After` backoff (`send_with_retry`) and 60s query cache; never retry through a 429; never tighten polling.
- **Tests are offline by default**; anything hitting the network is `#[ignore]`d. This is a **binary crate with no lib target** — run `cargo test` (not `cargo test --lib`).
- **Commit trailer** on every commit:
  ```
  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
  ```

---

## File Structure

| File | Responsibility |
|---|---|
| `src/trade/session.rs` | **new** — `ProxyConfig`, `sticky_session_id`/`sticky_proxy_url` helpers, `TradeSession` (per-call DI), `MemberSession`/`MemberSessions` registry |
| `src/trade/mod.rs` | export `session`; `TradePricer::{price,breakdown}` take `&TradeSession` |
| `src/trade/client.rs` | `pub(crate)` `USER_AGENT`/`TRADE_BASE`; `TradeApi::{search,fetch}` + `Comparables::comparables` take `&TradeSession`; per-request sensitive cookie via `with_cookie`; base `http` keeps catalog |
| `src/trade/ablation.rs` | thread `&TradeSession` through `gather_comparables`/`estimate`/`breakdown`; update test fakes |
| `src/config.rs` | rename `poe_sessid`→`poesessid` (env `POE_SESSID`→`POESESSID`); add `proxy: Option<ProxyConfig>`, `session_ttl_mins`; Debug + `from_lookup` |
| `src/main.rs` | build `MemberSessions`; add to `Data`; rename `config.poesessid`; register `logout` |
| `src/discord/mod.rs` | `Data.sessions: Arc<MemberSessions>`, `Data.pending`; `logout` module decl |
| `src/discord/paste.rs` | `run_pricing` extract; session check; `prompt_connect` + `ConnectModal`; `valid_poesessid` |
| `src/discord/logout.rs` | **new** — `/logout` |
| `src/discord/help.rs` | mention connect + `/logout` |
| `.env.example`, `CLAUDE.md` | document new vars; rename `POE_SESSID`→`POESESSID` |
| `drp-legal` (other repo) | Privacy Policy update (Task 7) |

---

## Task 1: Dependencies + `session.rs` pure pieces (`ProxyConfig`, helpers, `TradeSession`)

**Files:**
- Modify: `Cargo.toml`
- Create: `src/trade/session.rs`
- Modify: `src/trade/mod.rs` (add `pub mod session;`)
- Modify: `src/trade/client.rs` (make `USER_AGENT`, `TRADE_BASE` `pub(crate)`)

**Interfaces:**
- Produces:
  - `pub struct ProxyConfig { pub gateway: String, pub user: String, pub pass: String, pub country: String, pub lifetime_mins: u64 }` (derives `Clone`; **no** `Debug`)
  - `pub fn sticky_session_id(user_id: u64) -> String` (exactly 8 lowercase hex)
  - `pub fn sticky_proxy_url(cfg: &ProxyConfig, user_id: u64) -> String`
  - `pub struct TradeSession { pub client: Arc<reqwest::Client>, pub cookie: Arc<secrecy::SecretString> }` (**no** `Debug`) with `pub fn for_test() -> TradeSession`

- [ ] **Step 1: Add dependencies**

In `Cargo.toml`, change the `reqwest` line to add the `socks` feature and add two crates:

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "socks"] }
secrecy = "0.8"
percent-encoding = "2"
```

(Place `secrecy` and `percent-encoding` alphabetically among `[dependencies]`.)

- [ ] **Step 2: Make the trade constants crate-visible**

In `src/trade/client.rs`, change the two consts from private to `pub(crate)`:

```rust
pub(crate) const TRADE_BASE: &str = /* unchanged value */;
pub(crate) const USER_AGENT: &str = /* unchanged value */;
```

(Keep their existing string values; only add `pub(crate)`.)

- [ ] **Step 3: Create `src/trade/session.rs` with the pure pieces + failing tests**

```rust
use std::sync::Arc;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use secrecy::SecretString;

/// Operator-configured IPRoyal residential proxy.
/// No `Debug` derive: `pass` is an operator secret.
#[derive(Clone)]
pub struct ProxyConfig {
    pub gateway: String,    // host:port, e.g. "geo.iproyal.com:12321"
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
```

- [ ] **Step 4: Wire the module**

In `src/trade/mod.rs`, add alongside the other `pub mod` lines:

```rust
pub mod session;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test session::`
Expected: PASS (3 tests). Then `cargo build` succeeds.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt
cargo clippy
git add Cargo.toml Cargo.lock src/trade/session.rs src/trade/mod.rs src/trade/client.rs
git commit -m "feat(trade): proxy config + sticky-session URL + TradeSession (session.rs)"
```

---

## Task 2: `MemberSessions` registry

**Files:**
- Modify: `src/trade/session.rs`

**Interfaces:**
- Consumes: `ProxyConfig`, `sticky_proxy_url`, `TradeSession` (Task 1); `crate::trade::client::{USER_AGENT, TRADE_BASE}`; `crate::trade::rates::RateTable`.
- Produces:
  - `pub struct MemberSessions` with:
    - `pub fn new(proxy: Option<ProxyConfig>, ttl: std::time::Duration) -> Self`
    - `pub fn has_live_session(&self, user_id: u64) -> bool`
    - `pub async fn store(&self, user_id: u64, cookie: SecretString) -> anyhow::Result<()>`
    - `pub fn session_for(&self, user_id: u64) -> Option<TradeSession>`
    - `pub fn forget(&self, user_id: u64)`

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/trade/session.rs`:

```rust
    use std::time::{Duration, Instant};

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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test session::`
Expected: FAIL — `MemberSessions`, `insert_test`, `build_client` not defined.

- [ ] **Step 3: Implement the registry**

Add to `src/trade/session.rs` (top: extend imports). Replace the `use` block at the top of the file with:

```rust
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::header::{self, HeaderValue};
use secrecy::{ExposeSecret, SecretString};

use crate::trade::client::{TRADE_BASE, USER_AGENT};
```

Then add (after `TradeSession`):

```rust
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
        self.clients.write().unwrap().insert(user_id, client.clone());
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
        resp.error_for_status().context("trade rejected the request")?;
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
```

Note: `build_client` is referenced by the `#[test]` `builds_a_proxied_client_without_panicking`; since it's a private method tested from the same module's `tests` submodule, that is fine (child modules see private items).

- [ ] **Step 4: Run tests**

Run: `cargo test session::`
Expected: PASS (6 tests total). `cargo build` succeeds.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt
cargo clippy
git add src/trade/session.rs
git commit -m "feat(trade): in-memory MemberSessions registry (proxy client cache + TTL + revocation)"
```

---

## Task 3: Config + main wiring + docs

**Files:**
- Modify: `src/config.rs`
- Modify: `src/discord/mod.rs` (`Data.sessions`, `Data.pending`)
- Modify: `src/main.rs`
- Modify: `.env.example`
- Modify: `CLAUDE.md`

**Interfaces:**
- Consumes: `ProxyConfig`, `MemberSessions` (Tasks 1-2).
- Produces:
  - `Config { … poesessid: Option<String>, proxy: Option<ProxyConfig>, session_ttl_mins: u64 }`
  - `Data { …, pub sessions: std::sync::Arc<crate::trade::session::MemberSessions>, pub pending: std::sync::RwLock<std::collections::HashMap<u64, (crate::itemtext::ParsedItem, std::time::Instant)>> }`

- [ ] **Step 1: Write failing config tests**

In `src/config.rs` `tests` module add:

```rust
    #[test]
    fn parses_proxy_and_ttl_when_all_present() {
        let cfg = Config::from_lookup(|k| match k {
            "DISCORD_TOKEN" => Some("t".into()),
            "GUILD_ID" => Some("1".into()),
            "PROXY_GATEWAY" => Some("geo.iproyal.com:12321".into()),
            "PROXY_USER" => Some("u".into()),
            "PROXY_PASS" => Some("p".into()),
            "PROXY_COUNTRY" => Some("de".into()),
            "SESSION_TTL_MINS" => Some("60".into()),
            _ => None,
        })
        .unwrap();
        let proxy = cfg.proxy.expect("proxy configured");
        assert_eq!(proxy.gateway, "geo.iproyal.com:12321");
        assert_eq!(proxy.country, "de");
        assert_eq!(cfg.session_ttl_mins, 60);
    }

    #[test]
    fn proxy_is_none_when_incomplete() {
        let cfg = Config::from_lookup(|k| match k {
            "DISCORD_TOKEN" => Some("t".into()),
            "GUILD_ID" => Some("1".into()),
            "PROXY_GATEWAY" => Some("geo.iproyal.com:12321".into()),
            // missing PROXY_USER / PROXY_PASS
            _ => None,
        })
        .unwrap();
        assert!(cfg.proxy.is_none());
    }

    #[test]
    fn reads_poesessid_env() {
        let cfg = Config::from_lookup(|k| match k {
            "DISCORD_TOKEN" => Some("t".into()),
            "GUILD_ID" => Some("1".into()),
            "POESESSID" => Some("abc".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg.poesessid.as_deref(), Some("abc"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test config::`
Expected: FAIL — fields `proxy`/`session_ttl_mins`/`poesessid` not found; `POE_SESSID` still in use.

- [ ] **Step 3: Update the `Config` struct**

In `src/config.rs`, replace the struct with:

```rust
#[derive(Clone)]
pub struct Config {
    pub discord_token: String,
    pub guild_id: u64,
    pub poll_interval_mins: u64,
    pub min_volume: f64,
    pub poesessid: Option<String>,
    pub proxy: Option<crate::trade::session::ProxyConfig>,
    pub session_ttl_mins: u64,
}
```

- [ ] **Step 4: Update `from_lookup`**

Replace the `poe_sessid` line and the returned struct in `from_lookup` with:

```rust
        let poesessid = get("POESESSID").filter(|s| !s.is_empty());

        let session_ttl_mins = match get("SESSION_TTL_MINS") {
            Some(v) => v.parse::<u64>().context("SESSION_TTL_MINS must be a u64")?,
            None => 180,
        };

        let proxy = match (
            get("PROXY_GATEWAY").filter(|s| !s.is_empty()),
            get("PROXY_USER").filter(|s| !s.is_empty()),
            get("PROXY_PASS").filter(|s| !s.is_empty()),
        ) {
            (Some(gateway), Some(user), Some(pass)) => {
                let country = get("PROXY_COUNTRY")
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "us".to_string());
                let lifetime_mins = match get("PROXY_SESSION_LIFETIME_MINS") {
                    Some(v) => v
                        .parse::<u64>()
                        .context("PROXY_SESSION_LIFETIME_MINS must be a u64")?,
                    None => 30,
                };
                Some(crate::trade::session::ProxyConfig {
                    gateway,
                    user,
                    pass,
                    country,
                    lifetime_mins,
                })
            }
            _ => None,
        };

        Ok(Self {
            discord_token,
            guild_id,
            poll_interval_mins,
            min_volume,
            poesessid,
            proxy,
            session_ttl_mins,
        })
```

- [ ] **Step 5: Keep secrets out of Debug**

The hand-written `impl std::fmt::Debug for Config` already omits the token. Leave it as-is — `poesessid`, `proxy.pass` are not listed, so they stay redacted by omission. (Do not add them.)

- [ ] **Step 6: Run config tests**

Run: `cargo test config::`
Expected: PASS. (`cargo build` will still fail until main.rs is updated — that's the next step.)

- [ ] **Step 7: Add the registry + pending map to `Data`**

In `src/discord/mod.rs`, update the `Data` struct:

```rust
pub struct Data {
    pub store: PriceStore,
    pub config: Config,
    pub pricer: Arc<TradePricer<TradeClient>>,
    pub rates: Arc<RwLock<crate::trade::rates::RateTable>>,
    pub sessions: Arc<crate::trade::session::MemberSessions>,
    pub pending: RwLock<std::collections::HashMap<u64, (crate::itemtext::ParsedItem, std::time::Instant)>>,
}
```

Ensure `use std::sync::{Arc, RwLock};` is present at the top of the file (it already imports `Arc`/`RwLock` for the existing fields; add `RwLock` if missing).

- [ ] **Step 8: Build the registry in `main` and rename the cookie field**

In `src/main.rs`:

1. Change the client construction to the new field name:
   ```rust
   let trade_client = TradeClient::new(config.poesessid.clone(), rates.clone())?;
   ```
2. After `let pricer = …;`, build the registry:
   ```rust
   let sessions = std::sync::Arc::new(crate::trade::session::MemberSessions::new(
       config.proxy.clone(),
       std::time::Duration::from_secs(config.session_ttl_mins * 60),
   ));
   ```
3. In the `Ok(Data { … })` returned from `.setup(...)`, add the two new fields:
   ```rust
   Ok(Data {
       store,
       config,
       pricer,
       rates,
       sessions,
       pending: std::sync::RwLock::new(std::collections::HashMap::new()),
   })
   ```
   (Move `sessions` into the closure — it is captured like `store`/`pricer`. Confirm the `move` closure captures it.)

- [ ] **Step 9: Build**

Run: `cargo build`
Expected: success (the `sessions`/`pending` fields are unused so far — `pub` fields don't warn).

- [ ] **Step 10: Update `.env.example` and `CLAUDE.md`**

In `.env.example`: rename `POE_SESSID` → `POESESSID` (keep the placeholder/comment), and append:

```
# Residential proxy (IPRoyal). All three required to enable per-member proxy egress.
# Copy the SOCKS5 host:port from your IPRoyal dashboard (e.g. geo.iproyal.com:12321).
PROXY_GATEWAY=
PROXY_USER=
PROXY_PASS=
PROXY_COUNTRY=us
PROXY_SESSION_LIFETIME_MINS=30
# How long a captured member POESESSID is kept in memory before re-prompting.
SESSION_TTL_MINS=180
```

In `CLAUDE.md`, in the **Configuration** section: rename `POE_SESSID` → `POESESSID` in its bullet, and add a bullet documenting `PROXY_GATEWAY`/`PROXY_USER`/`PROXY_PASS` (**secret**), `PROXY_COUNTRY`, `PROXY_SESSION_LIFETIME_MINS`, `SESSION_TTL_MINS` — noting per-member sessions are captured at runtime and held only in memory.

- [ ] **Step 11: Format, lint, commit**

```bash
cargo fmt
cargo clippy
git add src/config.rs src/discord/mod.rs src/main.rs .env.example CLAUDE.md
git commit -m "feat(config): POESESSID rename + proxy/session-ttl config; wire MemberSessions into Data"
```

---

## Task 4: Thread `&TradeSession` through the trade call chain

This is an atomic refactor — the crate will not compile until every signature and call site is updated. Make all edits, then verify with `cargo test`.

**Files:**
- Modify: `src/trade/client.rs`, `src/trade/ablation.rs`, `src/trade/mod.rs`, `src/discord/paste.rs`

**Interfaces:**
- Consumes: `TradeSession` (Task 1), `Data.sessions` (Task 3).
- Produces (new signatures every later task relies on):
  - `TradeApi::search(&self, query: &TradeQuery, session: &TradeSession)`; `TradeApi::fetch(&self, query_id: &str, hashes: &[String], session: &TradeSession)`
  - `Comparables::comparables(&self, query: &TradeQuery, limit: usize, session: &TradeSession)`
  - `gather_comparables(api, query, limit, max_relax, session)`, `estimate(c, query, limit, session)`, `breakdown(c, query, limit, k, session)`
  - `TradePricer::price(&self, item, league, session: &TradeSession)`; `TradePricer::breakdown(&self, item, league, session: &TradeSession)`

- [ ] **Step 1: `client.rs` — imports + per-request cookie helper**

At the top of `src/trade/client.rs`, add:

```rust
use secrecy::{ExposeSecret, SecretString};
use crate::trade::session::TradeSession;
```

Add a free helper (module level, near the other helpers):

```rust
/// Attaches the member's POESESSID as a per-request, sensitive Cookie header.
fn with_cookie(rb: reqwest::RequestBuilder, cookie: &SecretString) -> reqwest::RequestBuilder {
    match header::HeaderValue::from_str(&format!("POESESSID={}", cookie.expose_secret())) {
        Ok(mut v) => {
            v.set_sensitive(true);
            rb.header(header::COOKIE, v)
        }
        Err(_) => rb, // malformed cookie ⇒ send anonymous rather than panic
    }
}
```

- [ ] **Step 2: `client.rs` — `TradeApi` uses the session's client + cookie**

Change the trait definition:

```rust
#[async_trait]
pub trait TradeApi {
    async fn search(&self, query: &TradeQuery, session: &TradeSession) -> Result<SearchResponse>;
    async fn fetch(&self, query_id: &str, hashes: &[String], session: &TradeSession)
        -> Result<Vec<Listing>>;
}
```

In `impl TradeApi for TradeClient`, replace `self.http.post(&url)` / `self.http.get(&url)` with the session client + cookie. `search`:

```rust
    async fn search(&self, query: &TradeQuery, session: &TradeSession) -> Result<SearchResponse> {
        let url = format!("{TRADE_BASE}/search/{}", query.league);
        let payload = to_payload(query);
        let resp = self
            .send_with_retry(|| with_cookie(session.client.post(&url).json(&payload), &session.cookie))
            .await
            .context("trade2 search failed")?;
        // ... rest unchanged (parse id/total/hashes) ...
    }
```

`fetch`:

```rust
    async fn fetch(&self, query_id: &str, hashes: &[String], session: &TradeSession)
        -> Result<Vec<Listing>>
    {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let csv = hashes.join(",");
        let url = format!("{TRADE_BASE}/fetch/{csv}?query={query_id}");
        let v: Value = self
            .send_with_retry(|| with_cookie(session.client.get(&url), &session.cookie))
            .await
            .context("trade2 fetch failed")?
            .json()
            .await?;
        Ok(self.parse_fetch(&v))
    }
```

`send_with_retry`, `parse_fetch`, `fetch_stats_raw` (which keeps using `self.http`) are unchanged.

- [ ] **Step 3: `client.rs` — `Comparables` passes the session through**

In `impl crate::trade::ablation::Comparables for TradeClient`, change the method signature and the `gather_comparables` call:

```rust
    async fn comparables(
        &self,
        query: &crate::trade::model::TradeQuery,
        limit: usize,
        session: &TradeSession,
    ) -> anyhow::Result<Vec<crate::trade::model::Listing>> {
        // ... cache check unchanged ...
        let result =
            crate::trade::ablation::gather_comparables(self, query, limit, 3, session).await?;
        // ... cache insert unchanged ...
    }
```

- [ ] **Step 4: `ablation.rs` — thread the param**

Add `use crate::trade::session::TradeSession;` at the top.

`Comparables` trait:

```rust
#[async_trait]
pub trait Comparables {
    async fn comparables(
        &self,
        query: &TradeQuery,
        limit: usize,
        session: &TradeSession,
    ) -> Result<Vec<Listing>>;
}
```

`gather_comparables`: add `session: &TradeSession` after `max_relax`, and pass it to both calls:

```rust
        let resp = api.search(&q, session).await?;
        let take = resp.hashes.len().min(limit);
        let mut listings = api.fetch(&resp.id, &resp.hashes[..take], session).await?;
```

`estimate`: add `session` and forward:

```rust
pub async fn estimate<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    session: &TradeSession,
) -> Result<PriceEstimate> {
    let listings = c.comparables(query, limit, session).await?;
    Ok(estimate_from(&listings))
}
```

`breakdown`: add `session` as the final param and pass it to **every** `estimate(c, …)` call inside it (there are three: the baseline, the per-stat loop, and the pairwise probe). Example:

```rust
pub async fn breakdown<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    k: usize,
    session: &TradeSession,
) -> Result<Breakdown> {
    let baseline = estimate(c, query, limit, session).await?;
    // ... in the loop:
    let without = estimate(c, &q, limit, session).await?;
    // ... pairwise:
    let without_both = estimate(c, &q, limit, session).await?;
    // ... rest unchanged ...
}
```

- [ ] **Step 5: `mod.rs` — `TradePricer` takes the session**

Add `use crate::trade::session::TradeSession;`. Change the two public methods:

```rust
    pub async fn price(
        &self,
        item: &ParsedItem,
        league: &str,
        session: &TradeSession,
    ) -> Result<PriceEstimate> {
        let query = build_baseline(item, &self.pseudo, &self.catalog, league);
        let est = estimate(&self.comparables, &query, LISTING_LIMIT, session).await?;
        self.record(&query, &est);
        Ok(est)
    }

    pub async fn breakdown(
        &self,
        item: &ParsedItem,
        league: &str,
        session: &TradeSession,
    ) -> Result<Breakdown> {
        let query = build_baseline(item, &self.pseudo, &self.catalog, league);
        let bd = crate::trade::ablation::breakdown(
            &self.comparables, &query, LISTING_LIMIT, TOP_K, session,
        )
        .await?;
        self.record(&query, &bd.baseline);
        Ok(bd)
    }
```

- [ ] **Step 6: Update the test fakes (so the crate compiles + tests pass)**

In `src/trade/ablation.rs` `tests`:
- `impl TradeApi for FakeApi`: add `_session: &TradeSession` to `search` and `fetch`.
- `impl Comparables for FakePricer`: add `_session: &TradeSession` to `comparables`.
- `impl Comparables for CountingComparables`: add `_session: &TradeSession` to `comparables`.
- Add `use crate::trade::session::TradeSession;` in the tests module.
- Every call to `gather_comparables(...)`, `estimate(...)`, `breakdown(...)`, and `.comparables(...)` in these tests: append `&TradeSession::for_test()` as the final argument.

In `src/trade/mod.rs` `tests`:
- `impl Comparables for Flat`: add `_session: &TradeSession` to `comparables`; add `use crate::trade::session::TradeSession;`.
- Every `pricer.price(...)` / `pricer.breakdown(...)` call: append `, &TradeSession::for_test()`.

- [ ] **Step 7: `paste.rs` — get a session from the registry (temporary stub for the no-session case)**

In `src/discord/paste.rs`, in `price_rare`, replace the `let pricer = …; let est = match pricer.price(parsed, &league.name).await { … }` opening with a session lookup. Insert at the top of `price_rare`:

```rust
    let uid = ctx.author().id.get();
    let Some(session) = ctx.data().sessions.session_for(uid) else {
        ctx.say("🔑 You need to connect your PoE account first — coming in the next step.")
            .await?;
        return Ok(());
    };
```

Then change the two pricer calls in `price_rare` to pass the session:
- `pricer.price(parsed, &league.name, &session).await`
- `pricer.breakdown(parsed, &league.name, &session).await`

(The stub message is replaced by the real connect flow in Task 5.)

- [ ] **Step 8: Build + full test run**

Run: `cargo build` then `cargo test`
Expected: compiles; all existing tests pass (now passing `&TradeSession::for_test()`).

- [ ] **Step 9: Format, lint, commit**

```bash
cargo fmt
cargo clippy
git add src/trade/client.rs src/trade/ablation.rs src/trade/mod.rs src/discord/paste.rs
git commit -m "refactor(trade): inject per-call TradeSession through the trade chain"
```

---

## Task 5: Discord capture flow (`/paste` → connect button → modal → validate → auto-run)

**Files:**
- Modify: `src/discord/paste.rs`

**Interfaces:**
- Consumes: `Data.sessions.{has_live_session,store,session_for}`, `Data.pending` (Task 3); `secrecy::SecretString`; `poise::execute_modal_on_component_interaction`.
- Produces: `run_pricing(ctx, parsed, league, session)`; `valid_poesessid(&str) -> bool`; `ConnectModal`.

- [ ] **Step 1: Write the failing unit test for `valid_poesessid`**

In `src/discord/paste.rs`, add a `tests` module (or extend an existing one):

```rust
#[cfg(test)]
mod tests {
    use super::valid_poesessid;

    #[test]
    fn accepts_32_hex_rejects_otherwise() {
        assert!(valid_poesessid("0123456789abcdef0123456789ABCDEF"));
        assert!(valid_poesessid("  0123456789abcdef0123456789abcdef  ")); // trimmed
        assert!(!valid_poesessid(""));
        assert!(!valid_poesessid("tooshort"));
        assert!(!valid_poesessid("zzzz567890abcdef0123456789abcdef")); // non-hex
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test valid_poesessid`
Expected: FAIL — `valid_poesessid` not defined.

- [ ] **Step 3: Implement `valid_poesessid`**

Add to `src/discord/paste.rs`:

```rust
/// A POESESSID is a 32-character hex string. Light pre-check so we don't burn a
/// trade call on an obvious paste error.
fn valid_poesessid(s: &str) -> bool {
    let s = s.trim();
    s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit())
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test valid_poesessid`
Expected: PASS.

- [ ] **Step 5: Add the `ConnectModal`**

Add to `src/discord/paste.rs` (note: **no `Debug` derive** — holds the secret. If the `Modal` derive macro fails to compile without `Debug`, add a manual redacting `impl std::fmt::Debug for ConnectModal` that prints `ConnectModal(***)`.):

```rust
#[derive(poise::Modal)]
#[name = "Connect your PoE account"]
struct ConnectModal {
    #[name = "POESESSID (from your pathofexile.com cookies)"]
    #[placeholder = "32-character hex value"]
    poesessid: String,
}
```

- [ ] **Step 6: Extract `run_pricing` from `price_rare`**

Move the body of `price_rare` that produces the estimate, the embed, the "Break it down" button, and the `ComponentInteractionCollector` loop into a new function. It takes the resolved `session`:

```rust
async fn run_pricing(
    ctx: &Context<'_>,
    parsed: &itemtext::ParsedItem,
    league: &League,
    session: &crate::trade::session::TradeSession,
) -> Result<(), Error> {
    use poise::serenity_prelude as serenity;

    let pricer = ctx.data().pricer.clone();
    let est = match pricer.price(parsed, &league.name, session).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "trade price failed");
            ctx.say("Couldn't reach trade right now — try again shortly.").await?;
            return Ok(());
        }
    };
    // ... the existing secondary_rate computation, embed, button, collector,
    //     and the `pricer.breakdown(parsed, &league.name, session)` call,
    //     all moved here verbatim from the old price_rare ...
}
```

(Move the existing logic exactly; only the function signature and the `session` argument threading are new.)

- [ ] **Step 7: Rewrite `price_rare` to branch on session presence**

```rust
async fn price_rare(
    ctx: &Context<'_>,
    parsed: &itemtext::ParsedItem,
    league: &League,
) -> Result<(), Error> {
    let uid = ctx.author().id.get();
    if let Some(session) = ctx.data().sessions.session_for(uid) {
        return run_pricing(ctx, parsed, league, &session).await;
    }
    prompt_connect(ctx, parsed, league).await
}
```

- [ ] **Step 8: Implement `prompt_connect`**

```rust
async fn prompt_connect(
    ctx: &Context<'_>,
    parsed: &itemtext::ParsedItem,
    league: &League,
) -> Result<(), Error> {
    use poise::serenity_prelude as serenity;

    let uid = ctx.author().id.get();
    // Stash the parsed item so we can price it after the member connects.
    ctx.data()
        .pending
        .write()
        .unwrap()
        .insert(uid, (parsed.clone(), std::time::Instant::now()));

    let button = serenity::CreateButton::new("drp_connect")
        .label("🔑 Connect your PoE account")
        .style(serenity::ButtonStyle::Primary);
    let row = serenity::CreateActionRow::Buttons(vec![button]);
    let reply = ctx
        .send(
            poise::CreateReply::default()
                .ephemeral(true)
                .content(
                    "To price rares I search the trade site as **you**. \
                     Click below and paste your **POESESSID** (pathofexile.com cookie). \
                     It's kept in memory only, used solely for your searches, and you can remove it any time with `/logout`. \
                     Privacy: https://drp.pme.it/privacy",
                )
                .components(vec![row]),
        )
        .await?;

    let msg = reply.message().await?;
    let interaction =
        serenity::ComponentInteractionCollector::new(ctx.serenity_context().shard.clone())
            .message_id(msg.id)
            .custom_ids(vec!["drp_connect".to_string()])
            .filter(move |mci| mci.user.id.get() == uid)
            .timeout(Duration::from_secs(120))
            .await;

    let Some(mci) = interaction else {
        reply
            .edit(*ctx, poise::CreateReply::default().content("Connect timed out — run `/paste` again when ready.").components(vec![]))
            .await?;
        return Ok(());
    };

    // Open the POESESSID modal off the component interaction.
    // If the `AsRef<Context>` bound complains, use `ctx.serenity_context().clone()`.
    let submitted = poise::execute_modal_on_component_interaction::<ConnectModal>(
        ctx.serenity_context(),
        mci,
        None,
        Some(Duration::from_secs(300)),
    )
    .await?;

    let Some(modal) = submitted else {
        return Ok(()); // member dismissed the modal
    };

    if !valid_poesessid(&modal.poesessid) {
        ctx.say("That doesn't look like a POESESSID (expected 32 hex chars). Run `/paste` and try again.")
            .await?;
        return Ok(());
    }

    let cookie = secrecy::SecretString::new(modal.poesessid.trim().to_string());
    if let Err(e) = ctx.data().sessions.store(uid, cookie).await {
        tracing::warn!(error = %e, "session store/validation failed"); // never logs the cookie
        ctx.say("Couldn't reach trade with that session — is your POESESSID current? Copy it again and `/paste`.")
            .await?;
        return Ok(());
    }

    // Pull the stashed item and price it now.
    let pending = ctx.data().pending.write().unwrap().remove(&uid);
    match (pending, ctx.data().sessions.session_for(uid)) {
        (Some((parsed_item, _)), Some(session)) => {
            run_pricing(ctx, &parsed_item, league, &session).await
        }
        _ => {
            ctx.say("Connected! Run `/paste` again to price your item.").await?;
            Ok(())
        }
    }
}
```

Notes for the implementer:
- `ParsedItem` must be `Clone` for the stash. It is already used by value in `paste`; if it does not derive `Clone`, add `#[derive(Clone)]` to `ParsedItem` in `src/itemtext.rs` (it is a plain data struct).
- Keep all user-facing replies **ephemeral** where the existing code allows; the connect prompt above is ephemeral.

- [ ] **Step 9: Build + test**

Run: `cargo build` then `cargo test`
Expected: compiles; `valid_poesessid` test passes; existing tests still pass.

- [ ] **Step 10: Format, lint, commit**

```bash
cargo fmt
cargo clippy
git add src/discord/paste.rs
git commit -m "feat(discord): capture member POESESSID on first /paste (button → modal → validate → auto-run)"
```

If `ParsedItem` needed `#[derive(Clone)]`, include `src/itemtext.rs` in the `git add`.

---

## Task 6: `/logout` command + help text

**Files:**
- Create: `src/discord/logout.rs`
- Modify: `src/discord/mod.rs` (module decl)
- Modify: `src/main.rs` (register command)
- Modify: `src/discord/help.rs`

**Interfaces:**
- Consumes: `Data.sessions.forget` (Task 2/3).
- Produces: `pub fn logout()` poise command.

- [ ] **Step 1: Create the command**

`src/discord/logout.rs`:

```rust
use crate::discord::{Context, Error};

/// Disconnect your PoE account (removes your stored session).
#[poise::command(slash_command)]
pub async fn logout(ctx: Context<'_>) -> Result<(), Error> {
    let uid = ctx.author().id.get();
    ctx.data().sessions.forget(uid);
    ctx.send(
        poise::CreateReply::default().ephemeral(true).content(
            "Disconnected — your session is removed from memory. \
             For full safety, also log out on pathofexile.com to invalidate the cookie.",
        ),
    )
    .await?;
    Ok(())
}
```

(Match the exact `Context`/`Error` import path used by the sibling commands — check `src/discord/help.rs`'s `use` lines and mirror them.)

- [ ] **Step 2: Declare the module**

In `src/discord/mod.rs`, add `pub mod logout;` alongside the other command module decls (`pub mod paste;` etc.).

- [ ] **Step 3: Register the command**

In `src/main.rs`, add to the `commands: vec![ … ]` list:

```rust
                discord::logout::logout(),
```

- [ ] **Step 4: Update help text**

In `src/discord/help.rs`, add two `.field(...)` entries before the `/help` field:

```rust
        .field(
            "/paste (first time)",
            "On your first rare price-check, the bot asks for your POESESSID (kept in memory only) so it can search trade as you.",
            false,
        )
        .field(
            "/logout",
            "Remove your stored session from the bot.",
            false,
        )
```

- [ ] **Step 5: Build + test**

Run: `cargo build` then `cargo test`
Expected: compiles; tests pass.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt
cargo clippy
git add src/discord/logout.rs src/discord/mod.rs src/main.rs src/discord/help.rs
git commit -m "feat(discord): /logout session revocation + help text"
```

---

## Task 7: Privacy Policy update (drp-legal repo)

**Files:**
- Modify: the privacy policy document in the separate `drp-legal` repo (served at https://drp.pme.it/privacy).

This task is in a **different repository** and is a hard prerequisite for shipping member-credential capture. Do it as a normal commit there (not in this repo).

- [ ] **Step 1: Replace the "no database / stores no personal data" statement** with a section disclosing that the bot:
  - captures and holds a member's **POESESSID in memory only**, transiently, used **solely to perform that member's own** trade searches;
  - never shares it, never writes it to disk, loses it on restart and after the session TTL (default 180 min);
  - routes that member's trade traffic via a residential proxy;
  - lets a member delete it any time with `/logout`.

- [ ] **Step 2: Verify the URL referenced in the connect prompt** (`https://drp.pme.it/privacy`) resolves to the updated policy. If the live path differs, update the link in `prompt_connect` (Task 5, Step 8) to match and amend that commit.

- [ ] **Step 3: Commit in the drp-legal repo** with a message describing the per-member session disclosure.

---

## Final verification (after all tasks)

- [ ] `cargo fmt --check` clean
- [ ] `cargo clippy` clean
- [ ] `cargo test` green (offline suite)
- [ ] `cargo test -- --ignored` (optional, hits network) — exercise live `store` validation through a real proxy + POESESSID if available
- [ ] Manual smoke in the guild: first `/paste` of a rare prompts connect → modal → prices; second `/paste` skips the prompt; `/logout` then `/paste` re-prompts.
- [ ] Confirm no secret appears in logs: grep the running logs for `POESESSID=` — should never appear.
