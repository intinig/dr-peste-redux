//! Per-member proactive rate limiter for trade2 search/fetch.
//!
//! trade2 enforces per-account and per-IP limits and reports them in
//! `X-Rate-Limit-*` response headers. We pace *before* sending so we stay under
//! the cap and never trigger a 429. Search and fetch have independent server-side
//! limits, so each gets its own bucket. The reactive 429 backoff in `client.rs`
//! remains as a safety net behind this.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use reqwest::header::HeaderMap;
use tokio::sync::Mutex;

use crate::trade::client::{parse_rate_rules, RateRule};

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
            rules: vec![RateRule {
                max: 5,
                period: 10,
                restriction: 0,
            }],
            sends: VecDeque::new(),
        }
    }

    /// No rules → never waits. Test-only handle.
    #[cfg(test)]
    fn empty() -> Self {
        Bucket {
            rules: Vec::new(),
            sends: VecDeque::new(),
        }
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
        // Reconcile only the tightest (smallest-period) window against the
        // server's reported usage, taking the max current across the Account and
        // IP scopes for that period. Seeding every triple would double-count
        // shared-period Account+IP state and let a long window's count pollute
        // tighter windows; the reactive 429 backoff covers any longer window.
        if let Some(min_period) = states.iter().map(|(_, p)| *p).filter(|p| *p > 0).min() {
            let current = states
                .iter()
                .filter(|(_, p)| *p == min_period)
                .map(|(c, _)| *c)
                .max()
                .unwrap_or(0);
            let now = Instant::now();
            let local = b
                .sends
                .iter()
                .filter(|t| now.duration_since(**t).as_secs_f64() < min_period as f64)
                .count() as u32;
            for _ in local..current {
                b.sends.push_back(now);
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

/// Seconds to wait before the next send so that, after it, no rule is violated.
///
/// `ages` are the ages-in-seconds of recent sends, sorted ascending (most recent
/// first). For a rule `(max, period)`, the new send is safe once the `max`-th
/// most recent existing send has aged out of the window; the limiter waits the
/// longest such gap across all rules. Returns `0.0` when free to send now.
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

#[cfg(test)]
mod tests {
    use reqwest::header::{HeaderMap, HeaderValue};

    use super::*;

    fn rule(max: u32, period: u32) -> RateRule {
        RateRule {
            max,
            period,
            restriction: 0,
        }
    }

    fn rule_r(max: u32, period: u32, restriction: u32) -> RateRule {
        RateRule {
            max,
            period,
            restriction,
        }
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
        h.insert(
            "X-Rate-Limit-Account",
            HeaderValue::from_static("8:10:60,15:60:120"),
        );
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
        h.insert(
            "X-Rate-Limit-Account-State",
            HeaderValue::from_static("4:10:0"),
        );
        lim.observe(Endpoint::Fetch, &h).await;
        assert_eq!(lim.fetch.lock().await.sends.len(), 4);
    }

    #[tokio::test]
    async fn endpoints_are_independent() {
        let lim = RateLimiter::new();
        let mut h = HeaderMap::new();
        h.insert("X-Rate-Limit-Account", HeaderValue::from_static("5:10:60"));
        h.insert(
            "X-Rate-Limit-Account-State",
            HeaderValue::from_static("3:10:0"),
        );
        lim.observe(Endpoint::Search, &h).await;
        assert_eq!(lim.search.lock().await.sends.len(), 3);
        assert_eq!(lim.fetch.lock().await.sends.len(), 0);
    }

    #[tokio::test]
    async fn observe_does_not_double_count_account_and_ip_state() {
        let lim = RateLimiter::new();
        let mut h = HeaderMap::new();
        h.insert("X-Rate-Limit-Account", HeaderValue::from_static("8:10:60"));
        h.insert(
            "X-Rate-Limit-Account-State",
            HeaderValue::from_static("4:10:0"),
        );
        h.insert("X-Rate-Limit-Ip-State", HeaderValue::from_static("3:10:0"));
        lim.observe(Endpoint::Search, &h).await;
        // The shared 10s window seeds max(4, 3) = 4 — NOT 4 + 3 = 7.
        assert_eq!(lim.search.lock().await.sends.len(), 4);
    }

    #[tokio::test]
    async fn observe_reconciles_only_tightest_window() {
        let lim = RateLimiter::new();
        let mut h = HeaderMap::new();
        h.insert(
            "X-Rate-Limit-Account-State",
            HeaderValue::from_static("5:10:0,30:300:0"),
        );
        lim.observe(Endpoint::Fetch, &h).await;
        // Only the tightest (10s) window is reconciled (5); the 300s window's 30
        // does not pollute it.
        assert_eq!(lim.fetch.lock().await.sends.len(), 5);
    }
}
