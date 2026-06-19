# trade2 Proactive Rate-Limit Throttle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop trade2 429s by pacing every search/fetch *before* it is sent, per member, against the live rate-limit headers — so the full ablation breakdown always completes (just slower) instead of failing.

**Architecture:** A new per-member `RateLimiter` (separate Search/Fetch buckets) learns trade2's `X-Rate-Limit-*` rules from each response and sleeps just enough before the next send to stay under the cap. It is carried on `TradeSession` and gated centrally inside `send_with_retry`; the existing reactive 429 backoff stays as a safety net behind it.

**Tech Stack:** Rust; `tokio::sync::Mutex` (tokio `full` is already enabled); `reqwest` headers; reuses the existing `RateRule` / `parse_rate_rules` in `src/trade/client.rs`.

**Design spec:** `docs/superpowers/specs/2026-06-19-trade2-rate-limit-throttle-design.md`.

## Global Constraints

- **Ablation untouched** (explicit user decision): no change to probe count, relaxation, `COMPARABLE_SAMPLE`, the craftability filter, percentiles, or the fallback ladder. This change only paces requests.
- **Per-member, not global:** the limiter is keyed per `user_id`; budgets are per-account + per-IP and independent across members.
- **Reactive backoff retained:** keep the existing 429 `Retry-After` backoff in `send_with_retry` as a safety net behind the proactive throttle.
- **Limiter never errors** — it only delays. All genuine failures propagate exactly as today.
- Binary crate, no lib target — verify with `cargo test` (never `--lib`); the final `cargo build` must be **zero warnings**.
- Test-only constructors are gated `#[cfg(test)]` so they aren't dead code under `cargo build`.
- Commit trailer (after a blank line): `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Stage files by name; never `git add -A`.

---

## Task 1: Per-member proactive rate-limit throttle (limiter + wiring)

The limiter module and the code that calls it ship together: a limiter alone would be entirely dead code under `cargo build` (its only non-test callers are the wiring), which would break the zero-warnings constraint. The task is staged TDD-first on the pure scheduling core, then the async limiter, then the plumbing.

**Files:**
- Create: `src/trade/limiter.rs`
- Modify: `src/trade/mod.rs` (declare the module)
- Modify: `src/trade/session.rs` (carry the limiter on `TradeSession`; cache one per member)
- Modify: `src/trade/client.rs` (gate `send_with_retry`; pass the limiter from `search`/`fetch`/`fetch_stats_raw`)

**Interfaces:**
- Consumes (already exist, both `pub` in `src/trade/client.rs`): `RateRule { pub max: u32, pub period: u32, pub restriction: u32 }`; `parse_rate_rules(&str) -> Vec<RateRule>`.
- Produces:
  - `pub enum Endpoint { Search, Fetch }`
  - `pub struct RateLimiter` with `pub fn new() -> Self`, `#[cfg(test)] pub fn permissive() -> Self`, `pub async fn acquire(&self, ep: Endpoint)`, `pub async fn observe(&self, ep: Endpoint, headers: &reqwest::header::HeaderMap)`.
  - `TradeSession` gains `pub limiter: Arc<RateLimiter>`.
  - `TradeClient::send_with_retry` gains a `(limiter: &RateLimiter, ep: Endpoint)` prefix to its arguments.

---

- [ ] **Step 1: Declare the module and scaffold the pure core with failing tests**

In `src/trade/mod.rs`, add the module declaration alongside the others (after `pub mod client;`):

```rust
pub mod client;
pub mod limiter;
pub mod model;
```

Create `src/trade/limiter.rs` with the imports, the `RateRule` import, a **stub** `wait_secs` (returns `0.0` so the non-trivial tests fail), and the pure-logic tests:

```rust
//! Per-member proactive rate limiter for trade2 search/fetch.
//!
//! trade2 enforces per-account and per-IP limits and reports them in
//! `X-Rate-Limit-*` response headers. We pace *before* sending so we stay under
//! the cap and never trigger a 429. Search and fetch have independent server-side
//! limits, so each gets its own bucket. The reactive 429 backoff in `client.rs`
//! remains as a safety net behind this.

use crate::trade::client::RateRule;

/// Seconds to wait before the next send so that, after it, no rule is violated.
///
/// `ages` are the ages-in-seconds of recent sends, sorted ascending (most recent
/// first). For a rule `(max, period)`, the new send is safe once the `max`-th
/// most recent existing send has aged out of the window; the limiter waits the
/// longest such gap across all rules. Returns `0.0` when free to send now.
fn wait_secs(_rules: &[RateRule], _ages: &[f64]) -> f64 {
    0.0 // stub — replaced in Step 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(max: u32, period: u32) -> RateRule {
        RateRule { max, period, restriction: 0 }
    }

    #[test]
    fn wait_zero_when_empty() {
        assert_eq!(wait_secs(&[rule(5, 10)], &[]), 0.0);
    }

    #[test]
    fn wait_zero_under_cap() {
        // 3 recent sends, cap 5 → still room.
        assert_eq!(wait_secs(&[rule(5, 10)], &[0.1, 0.2, 0.3]), 0.0);
    }

    #[test]
    fn wait_until_oldest_in_window_ages_out() {
        // cap 2, period 10; ages ascending [1, 4]. The 2nd-most-recent (age 4)
        // must reach age 10 → wait = 10 - 4 = 6.
        let w = wait_secs(&[rule(2, 10)], &[1.0, 4.0]);
        assert!((w - 6.0).abs() < 1e-9, "got {w}");
    }

    #[test]
    fn tightest_rule_governs() {
        // rule A: 2/10s, ages [1,4] → wait 6. rule B: 3/60s, only 2 sends → 0.
        let w = wait_secs(&[rule(2, 10), rule(3, 60)], &[1.0, 4.0]);
        assert!((w - 6.0).abs() < 1e-9, "got {w}");
    }

    #[test]
    fn zero_max_blocks_for_period() {
        assert_eq!(wait_secs(&[rule(0, 7)], &[]), 7.0);
    }
}
```

(Step 1 imports and uses only what the pure tests need, so `cargo test` is warning-free here; the sliding-window types, helpers, and remaining imports arrive in Step 5.)

- [ ] **Step 2: Run the pure tests to verify they fail**

Run: `cargo test trade::limiter::tests`
Expected: FAIL — `wait_until_oldest_in_window_ages_out`, `tightest_rule_governs`, and `zero_max_blocks_for_period` fail (stub returns `0.0`); the two zero-cases pass.

- [ ] **Step 3: Implement the pure `wait_secs`**

Replace the stub body in `src/trade/limiter.rs`:

```rust
fn wait_secs(rules: &[RateRule], ages: &[f64]) -> f64 {
    let mut wait = 0.0_f64;
    for r in rules {
        let max = r.max as usize;
        if max == 0 {
            // Pathological rule: block for a full period.
            wait = wait.max(r.period as f64);
            continue;
        }
        if ages.len() >= max {
            // Age of the max-th most recent send (0-indexed `max - 1`). When it
            // is already older than the period this is <= 0 → no wait.
            let w = r.period as f64 - ages[max - 1];
            if w > wait {
                wait = w;
            }
        }
    }
    wait.max(0.0)
}
```

- [ ] **Step 4: Run the pure tests to green**

Run: `cargo test trade::limiter::tests`
Expected: PASS — all five pure tests pass.

- [ ] **Step 5: Add `Endpoint`, the buckets, the `RateLimiter` (acquire/observe), helpers, and async tests**

First, extend the module imports at the top of `src/trade/limiter.rs` to the full set (replacing the single `use crate::trade::client::RateRule;` line from Step 1):

```rust
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use reqwest::header::HeaderMap;
use tokio::sync::Mutex;

use crate::trade::client::{parse_rate_rules, RateRule};
```

Then insert the following **after** the imports and **before** `fn wait_secs` (the `Endpoint`/`Bucket`/`RateLimiter` definitions), keeping `wait_secs` where it is:

```rust
/// Which trade2 endpoint a request hits. Each has its own limit bucket.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Endpoint {
    Search,
    Fetch,
}

/// One endpoint's learned rules + a sliding window of recent send instants.
struct Bucket {
    rules: Vec<RateRule>,
    sends: VecDeque<Instant>,
}

impl Bucket {
    /// Conservative defaults used only until the first live response replaces
    /// them: 5 requests / 10s (≈ trade2's tightest documented search rule).
    fn with_defaults() -> Self {
        Bucket {
            rules: vec![RateRule { max: 5, period: 10, restriction: 0 }],
            sends: VecDeque::new(),
        }
    }

    /// No rules → never waits. Test-only handle.
    #[cfg(test)]
    fn empty() -> Self {
        Bucket { rules: Vec::new(), sends: VecDeque::new() }
    }
}

/// Per-member limiter with independent Search and Fetch buckets.
pub struct RateLimiter {
    search: Mutex<Bucket>,
    fetch: Mutex<Bucket>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    /// Live limiter seeded with conservative defaults (calibrates from headers).
    pub fn new() -> Self {
        RateLimiter {
            search: Mutex::new(Bucket::with_defaults()),
            fetch: Mutex::new(Bucket::with_defaults()),
        }
    }

    /// Permissive limiter (no rules → never waits). Test-only.
    #[cfg(test)]
    pub fn permissive() -> Self {
        RateLimiter {
            search: Mutex::new(Bucket::empty()),
            fetch: Mutex::new(Bucket::empty()),
        }
    }

    fn bucket(&self, ep: Endpoint) -> &Mutex<Bucket> {
        match ep {
            Endpoint::Search => &self.search,
            Endpoint::Fetch => &self.fetch,
        }
    }

    /// Async-sleep until sending one request on `ep` keeps every learned rule
    /// satisfied, then record the send. Holds the bucket lock across the sleep so
    /// concurrent callers for the same member queue in order.
    pub async fn acquire(&self, ep: Endpoint) {
        let mut b = self.bucket(ep).lock().await;
        let now = Instant::now();
        let mut ages: Vec<f64> = b
            .sends
            .iter()
            .map(|t| now.duration_since(*t).as_secs_f64())
            .collect();
        ages.sort_by(|a, c| a.partial_cmp(c).unwrap_or(std::cmp::Ordering::Equal));
        let wait = wait_secs(&b.rules, &ages);
        if wait > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(wait)).await;
        }
        let sent = Instant::now();
        b.sends.push_back(sent);
        // Prune sends older than the longest rule period to bound memory.
        let longest = b.rules.iter().map(|r| r.period).max().unwrap_or(0) as f64;
        while let Some(front) = b.sends.front() {
            if sent.duration_since(*front).as_secs_f64() > longest {
                b.sends.pop_front();
            } else {
                break;
            }
        }
    }

    /// Update a bucket's rules from the response's `X-Rate-Limit-*` headers and
    /// reconcile usage: if the server reports more current hits in a window than
    /// our local record holds, seed synthetic sends so we back off. Best-effort.
    pub async fn observe(&self, ep: Endpoint, headers: &HeaderMap) {
        let rules = read_rules(headers);
        let states = read_states(headers);
        let mut b = self.bucket(ep).lock().await;
        if !rules.is_empty() {
            b.rules = rules;
        }
        if !states.is_empty() {
            let now = Instant::now();
            for (current, period) in states {
                let local = b
                    .sends
                    .iter()
                    .filter(|t| now.duration_since(**t).as_secs_f64() < period as f64)
                    .count() as u32;
                for _ in local..current {
                    b.sends.push_back(now);
                }
            }
        }
    }
}

/// Rules from the `Account`/`Ip` limit headers (their union — every request
/// counts against both scopes, so satisfying all rules satisfies both).
fn read_rules(headers: &HeaderMap) -> Vec<RateRule> {
    let mut out = Vec::new();
    for name in ["X-Rate-Limit-Account", "X-Rate-Limit-Ip"] {
        if let Some(v) = headers.get(name).and_then(|h| h.to_str().ok()) {
            out.extend(parse_rate_rules(v));
        }
    }
    out
}

/// `(current_hits, period)` pairs from the `-State` headers. The first triple
/// field is the current hit count here (not a max), so we read `RateRule.max`.
fn read_states(headers: &HeaderMap) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for name in ["X-Rate-Limit-Account-State", "X-Rate-Limit-Ip-State"] {
        if let Some(v) = headers.get(name).and_then(|h| h.to_str().ok()) {
            for r in parse_rate_rules(v) {
                out.push((r.max, r.period));
            }
        }
    }
    out
}
```

Then, inside the existing `mod tests`: add `use reqwest::header::{HeaderMap, HeaderValue};` at the top (next to `use super::*;`), the `rule_r` helper next to `rule`, and the following tests after the pure ones:

```rust
    fn rule_r(max: u32, period: u32, restriction: u32) -> RateRule {
        RateRule { max, period, restriction }
    }

    #[test]
    fn default_rule_paces_a_burst() {
        // A fresh bucket's default rule (5 / 10s) makes the 6th rapid send wait.
        let b = Bucket::with_defaults();
        let ages = [0.0, 0.1, 0.2, 0.3, 0.4]; // 5 sends already in the window
        let w = wait_secs(&b.rules, &ages);
        // 5th most recent (index 4, age 0.4) must reach age 10 → 10 - 0.4 = 9.6.
        assert!((w - 9.6).abs() < 1e-9, "got {w}");
    }

    #[tokio::test]
    async fn permissive_never_waits() {
        let lim = RateLimiter::permissive();
        for _ in 0..50 {
            lim.acquire(Endpoint::Search).await; // no rules → returns ~instantly
        }
    }

    #[tokio::test]
    async fn observe_learns_rules_from_headers() {
        let lim = RateLimiter::new();
        let mut h = HeaderMap::new();
        h.insert("X-Rate-Limit-Account", HeaderValue::from_static("8:10:60,15:60:120"));
        lim.observe(Endpoint::Search, &h).await;
        let b = lim.search.lock().await;
        assert_eq!(b.rules, vec![rule_r(8, 10, 60), rule_r(15, 60, 120)]);
    }

    #[tokio::test]
    async fn observe_seeds_backoff_from_state() {
        let lim = RateLimiter::new();
        let mut h = HeaderMap::new();
        // Server: 4 hits already used in the 10s window; we have 0 locally.
        h.insert("X-Rate-Limit-Account", HeaderValue::from_static("5:10:60"));
        h.insert("X-Rate-Limit-Account-State", HeaderValue::from_static("4:10:0"));
        lim.observe(Endpoint::Fetch, &h).await;
        assert_eq!(lim.fetch.lock().await.sends.len(), 4);
    }

    #[tokio::test]
    async fn endpoints_are_independent() {
        let lim = RateLimiter::new();
        let mut h = HeaderMap::new();
        h.insert("X-Rate-Limit-Account", HeaderValue::from_static("5:10:60"));
        h.insert("X-Rate-Limit-Account-State", HeaderValue::from_static("3:10:0"));
        lim.observe(Endpoint::Search, &h).await;
        assert_eq!(lim.search.lock().await.sends.len(), 3);
        assert_eq!(lim.fetch.lock().await.sends.len(), 0);
    }
```

- [ ] **Step 6: Run the full limiter module to green**

Run: `cargo test trade::limiter`
Expected: PASS — six sync tests (five pure + `default_rule_paces_a_burst`) plus four async tests (10 total).

- [ ] **Step 7: Carry the limiter on `TradeSession` and cache one per member**

In `src/trade/session.rs`, add the import after the existing `use crate::trade::client::{TRADE_BASE, USER_AGENT};` line:

```rust
use crate::trade::limiter::RateLimiter;
```

Add the field to `TradeSession`:

```rust
pub struct TradeSession {
    pub client: Arc<reqwest::Client>,
    pub cookie: Arc<SecretString>,
    pub limiter: Arc<RateLimiter>,
}
```

Update `for_test` to supply a permissive limiter:

```rust
    #[cfg(test)]
    pub fn for_test() -> TradeSession {
        TradeSession {
            client: Arc::new(reqwest::Client::new()),
            cookie: Arc::new(SecretString::new("test-cookie".to_string())),
            limiter: Arc::new(RateLimiter::permissive()),
        }
    }
```

Add the per-member limiter cache field to `MemberSessions`:

```rust
pub struct MemberSessions {
    sessions: RwLock<HashMap<u64, MemberSession>>,
    clients: RwLock<HashMap<u64, Arc<reqwest::Client>>>,
    limiters: RwLock<HashMap<u64, Arc<RateLimiter>>>,
    proxy: Option<ProxyConfig>,
    ttl: Duration,
}
```

Initialize it in `new`:

```rust
    pub fn new(proxy: Option<ProxyConfig>, ttl: Duration) -> Self {
        MemberSessions {
            sessions: RwLock::new(HashMap::new()),
            clients: RwLock::new(HashMap::new()),
            limiters: RwLock::new(HashMap::new()),
            proxy,
            ttl,
        }
    }
```

Add a `limiter_for` helper directly after the `build_client` method:

```rust
    /// A member's rate limiter (built once, cached). Independent per member so
    /// each paces against their own account+IP budget.
    fn limiter_for(&self, user_id: u64) -> Arc<RateLimiter> {
        if let Some(l) = self.limiters.read().unwrap().get(&user_id) {
            return l.clone();
        }
        let l = Arc::new(RateLimiter::new());
        self.limiters.write().unwrap().insert(user_id, l.clone());
        l
    }
```

Include the limiter when building the live session (the `Found::Live` arm of `session_for`):

```rust
            Found::Live(cookie) => {
                let client = self.build_client(user_id).ok()?;
                let limiter = self.limiter_for(user_id);
                Some(TradeSession {
                    client,
                    cookie,
                    limiter,
                })
            }
```

Drop the limiter in `forget` (so a re-prompted member starts fresh):

```rust
    pub fn forget(&self, user_id: u64) {
        self.sessions.write().unwrap().remove(&user_id);
        self.clients.write().unwrap().remove(&user_id);
        self.limiters.write().unwrap().remove(&user_id);
    }
```

- [ ] **Step 8: Gate `send_with_retry` and pass the limiter from every caller**

In `src/trade/client.rs`, add the import after `use crate::trade::session::TradeSession;`:

```rust
use crate::trade::limiter::{Endpoint, RateLimiter};
```

Add the process-wide default limiter field to `TradeClient` (for the anonymous catalog call):

```rust
pub struct TradeClient {
    http: Client,
    rates: Arc<RwLock<RateTable>>,
    default_limiter: Arc<RateLimiter>,
    /// Short-lived cache keyed by `"<limit>|<query_json>"`.
    /// Entries expire after 60 seconds so repeated calls (e.g. the baseline
    /// probe shared between `price` and `breakdown`) hit trade2 only once,
    /// keeping traffic polite without stale data across normal poll cycles.
    cache: std::sync::Mutex<
        std::collections::HashMap<String, (std::time::Instant, Vec<crate::trade::model::Listing>)>,
    >,
}
```

Initialize it in `new` (the returned `Self { … }`):

```rust
        Ok(Self {
            http: builder.build()?,
            rates,
            default_limiter: Arc::new(RateLimiter::new()),
            cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
```

Replace `send_with_retry` with the gated version (acquire before each attempt, observe the response headers):

```rust
    /// Sends a request, pacing it through `limiter` first (proactive throttle)
    /// and retrying up to twice on HTTP 429 after sleeping for the server-advised
    /// period (reactive safety net). Other errors propagate immediately.
    async fn send_with_retry<F>(
        &self,
        limiter: &RateLimiter,
        ep: Endpoint,
        build: F,
    ) -> Result<reqwest::Response>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut attempt = 0u32;
        loop {
            limiter.acquire(ep).await;
            let resp = build().send().await?;
            limiter.observe(ep, resp.headers()).await;
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < 2 {
                let wait = retry_after_secs(resp.headers());
                tracing::warn!(wait_secs = wait, "trade2 rate-limited; backing off");
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                attempt += 1;
                continue;
            }
            return Ok(resp.error_for_status()?);
        }
    }
```

Update `fetch_stats_raw` to pass the default limiter:

```rust
    pub async fn fetch_stats_raw(&self) -> Result<String> {
        let url = format!("{TRADE_BASE}/data/stats");
        Ok(self
            .send_with_retry(&self.default_limiter, Endpoint::Fetch, || self.http.get(&url))
            .await
            .context("trade2 data/stats failed")?
            .text()
            .await?)
    }
```

Update the `search` call site (in `impl TradeApi for TradeClient`):

```rust
        let resp = self
            .send_with_retry(&session.limiter, Endpoint::Search, || {
                with_cookie(session.client.post(&url).json(&payload), &session.cookie)
            })
            .await
            .context("trade2 search failed")?;
```

Update the `fetch` call site:

```rust
        let v: Value = self
            .send_with_retry(&session.limiter, Endpoint::Fetch, || {
                with_cookie(session.client.get(&url), &session.cookie)
            })
            .await
            .context("trade2 fetch failed")?
            .json()
            .await?;
```

(`MemberSessions::store`'s one-shot connectivity probe is a single request that does not go through `send_with_retry`; leave it ungated — a single call is not a burst and gating it adds nothing.)

- [ ] **Step 9: Build and run the whole suite**

Run: `cargo build` then `cargo test`
Expected: `cargo build` completes with **zero warnings**; `cargo test` PASSES — the 9 new limiter tests plus the entire existing suite (all `TradeSession::for_test()` callers now get a permissive limiter and do not sleep).

- [ ] **Step 10: Format, lint, commit**

```bash
cargo fmt && cargo clippy
git add src/trade/limiter.rs src/trade/mod.rs src/trade/session.rs src/trade/client.rs
git commit -m "feat(trade): proactive per-member rate-limit throttle

Pace search/fetch before send against live X-Rate-Limit headers so the
full ablation breakdown completes without 429s; reactive backoff kept as
a safety net. Ablation/sample unchanged.
"
# + Co-Authored-By trailer
```

Expected: `cargo fmt` clean, `cargo clippy` clean (the `Default` impl satisfies `new_without_default`), commit created.

---

## Final verification (after the task)

- [ ] `cargo fmt --check` clean; `cargo clippy` clean; `cargo test` green; `cargo build` zero warnings.
- [ ] **Manual live acceptance** (after deploy): re-paste the Chiming Staff and click "Break it down". Confirm it **completes** (slower is fine) with **no** `trade2 rate-limited` / `trade breakdown failed error=trade2 search failed` in `docker logs`. Confirm a plain `/paste` price is still prompt.
- [ ] **Follow-up to re-check (out of scope here):** if the Chiming Staff still returns "No comparable listings found" once 429s are gone, investigate separately — note that `TradeApi::fetch` joins **all** hashes into one `/fetch/{csv}` URL, while trade2's fetch endpoint accepts at most ~10 ids per request; with `COMPARABLE_SAMPLE = 100` a liquid item may send >10 ids in one call. Verify whether that truncates/errors and, if so, plan a batched-fetch fix.
