//! trade2 HTTP client behind the `TradeApi` trait, with rate-limit-header
//! parsing. Anonymous by default; an optional POESESSID raises the ceiling.

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::{header, Client};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use std::sync::{Arc, RwLock};

use crate::trade::limiter::{Endpoint, RateLimiter};
use crate::trade::model::{Currency, Listing, ListingMod, Money, SearchResponse, TradeQuery};
use crate::trade::query::to_payload;
use crate::trade::rates::RateTable;
use crate::trade::session::TradeSession;

pub(crate) const TRADE_BASE: &str = "https://www.pathofexile.com/api/trade2";
pub(crate) const USER_AGENT: &str =
    "dr-peste-redux/0.1 (Discord guild price bot; not affiliated with Grinding Gear Games)";

/// Max item ids per trade2 `/fetch` request. Verified live: 10 ids → HTTP 200,
/// 11+ → HTTP 400 `{"error":{"code":2,"message":"Invalid query"}}`. `fetch`
/// batches its hashes into groups of this size.
const FETCH_BATCH: usize = 10;

#[derive(Clone, Debug, PartialEq)]
pub struct RateRule {
    pub max: u32,
    pub period: u32,
    pub restriction: u32,
}

/// Filled prefix+suffix affix count for a fetched item, hybrid-safe, from the
/// best signal the trade2 fetch response carries:
/// `extended.prefixes+suffixes` → `extended.mods.explicit` → `explicitMods` → 0.
fn affix_count(item: &Value) -> usize {
    if let Some(ext) = item.get("extended") {
        let p = ext.get("prefixes").and_then(|v| v.as_u64());
        let s = ext.get("suffixes").and_then(|v| v.as_u64());
        if p.is_some() || s.is_some() {
            return (p.unwrap_or(0) + s.unwrap_or(0)) as usize;
        }
        if let Some(n) = ext
            .get("mods")
            .and_then(|m| m.get("explicit"))
            .and_then(|e| e.as_array())
            .map(|a| a.len())
        {
            return n;
        }
    }
    item.get("explicitMods")
        .and_then(|m| m.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

/// Parses a fetch `tier` string like `"P5"`/`"S3"` → `5`/`3`.
fn parse_fetch_tier(t: &str) -> Option<u8> {
    let digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// First number in a mod description (the displayed roll), sign-preserving, e.g.
/// "123% increased …" → 123.0; "Adds 5 to 10 …" → 5.0; "-12% to …" → -12.0.
fn first_number(s: &str) -> Option<f64> {
    let mut num = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            if num.is_empty() && prev_dash {
                num.push('-'); // preserve the sign of a negative roll
            }
            num.push(c);
        } else if c == '.' && !num.is_empty() {
            num.push(c);
        } else if !num.is_empty() {
            break;
        } else {
            prev_dash = c == '-';
        }
    }
    num.parse().ok()
}

/// Per-listing explicit mods with stat id, tier, and rolled value. Stat id from
/// `explicitMods[].hash` (strip the `stat.` prefix); tier from `mods[0].tier`;
/// roll from the first number of the description.
fn listing_mods(item: &Value) -> Vec<ListingMod> {
    let mut mods: Vec<ListingMod> = item
        .get("explicitMods")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let hash = m.get("hash").and_then(|h| h.as_str())?;
                    let stat_id = hash.strip_prefix("stat.").unwrap_or(hash).to_string();
                    let tier = m
                        .get("mods")
                        .and_then(|x| x.as_array())
                        .and_then(|a| a.first())
                        .and_then(|m0| m0.get("tier"))
                        .and_then(|t| t.as_str())
                        .and_then(parse_fetch_tier);
                    let roll = m
                        .get("description")
                        .and_then(|d| d.as_str())
                        .and_then(first_number);
                    Some(ListingMod {
                        stat_id,
                        tier,
                        roll,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // Fallback: include any stat ids in `extended.hashes.explicit` that the
    // `explicitMods` entries didn't carry (e.g. display-string mods with no
    // `hash`), with tier/roll unknown — so the corpus keeps the stat presence.
    let known: std::collections::HashSet<String> = mods.iter().map(|m| m.stat_id.clone()).collect();
    if let Some(arr) = item
        .pointer("/extended/hashes/explicit")
        .and_then(|v| v.as_array())
    {
        for id in arr
            .iter()
            .filter_map(|pair| pair.get(0).and_then(|s| s.as_str()))
        {
            if !known.contains(id) {
                mods.push(ListingMod {
                    stat_id: id.to_string(),
                    tier: None,
                    roll: None,
                });
            }
        }
    }
    mods
}

/// Splits fetch hashes into comma-joined batches of at most `FETCH_BATCH` ids,
/// because trade2's `/fetch` rejects requests with more than 10 ids. An empty
/// input yields no batches.
fn fetch_batches(hashes: &[String]) -> Vec<String> {
    hashes.chunks(FETCH_BATCH).map(|c| c.join(",")).collect()
}

/// Parses an `X-Rate-Limit-*` value: comma-separated `max:period:restriction`.
pub fn parse_rate_rules(header_value: &str) -> Vec<RateRule> {
    header_value
        .split(',')
        .filter_map(|triple| {
            let mut it = triple.split(':');
            Some(RateRule {
                max: it.next()?.trim().parse().ok()?,
                period: it.next()?.trim().parse().ok()?,
                restriction: it.next()?.trim().parse().ok()?,
            })
        })
        .collect()
}

/// Seconds to wait after a 429: the standard `Retry-After` header if present,
/// else the largest period from the rate-limit rule headers, clamped to [1,120].
pub fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> u64 {
    if let Some(v) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
    {
        return v.clamp(1, 120);
    }
    for name in ["X-Rate-Limit-Ip", "X-Rate-Limit-Account"] {
        if let Some(period) = headers
            .get(name)
            .and_then(|h| h.to_str().ok())
            .and_then(|v| {
                parse_rate_rules(v)
                    .into_iter()
                    .map(|r| r.period as u64)
                    .max()
            })
        {
            return period.clamp(1, 120);
        }
    }
    5
}

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

#[async_trait]
pub trait TradeApi {
    async fn search(&self, query: &TradeQuery, session: &TradeSession) -> Result<SearchResponse>;
    async fn fetch(
        &self,
        query_id: &str,
        hashes: &[String],
        session: &TradeSession,
    ) -> Result<Vec<Listing>>;
}

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

impl TradeClient {
    /// `poe_sessid` optional: when present it is sent as the POESESSID cookie to
    /// raise the rate-limit ceiling; otherwise requests are anonymous.
    /// `rates` is the live currency rate table shared with the refresher task.
    pub fn new(poe_sessid: Option<String>, rates: Arc<RwLock<RateTable>>) -> Result<Self> {
        let mut builder = Client::builder().user_agent(USER_AGENT);
        if let Some(sess) = poe_sessid.filter(|s| !s.is_empty()) {
            let mut headers = header::HeaderMap::new();
            let cookie = format!("POESESSID={sess}");
            headers.insert(header::COOKIE, header::HeaderValue::from_str(&cookie)?);
            builder = builder.default_headers(headers);
        }
        Ok(Self {
            http: builder.build()?,
            rates,
            default_limiter: Arc::new(RateLimiter::new()),
            cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    fn parse_currency(s: &str) -> Currency {
        match s {
            "divine" => Currency::Divine,
            "exalted" => Currency::Exalted,
            "chaos" => Currency::Chaos,
            other => Currency::Other(other.to_string()),
        }
    }

    /// Parses a /fetch response body into listings. Assumption (smoke-verified):
    /// `{ result: [ { listing: { price: { amount, currency } } } ] }`.
    fn parse_fetch(&self, v: &Value) -> Vec<Listing> {
        v.get("result")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|entry| {
                        let price = entry.get("listing")?.get("price")?;
                        let amount = price.get("amount")?.as_f64()?;
                        let code = price.get("currency")?.as_str()?;
                        // Drop listings in currencies we can't convert to divine
                        // (e.g. "aug"); pricing them at 0 would poison the estimate.
                        let price_divine = self.rates.read().unwrap().to_divine(amount, code)?;
                        if price_divine <= 0.0 {
                            return None;
                        }
                        let item = entry.get("item");
                        let explicit_count = item.map(affix_count).unwrap_or(0);
                        let mods = item.map(listing_mods).unwrap_or_default();
                        let base_type = item
                            .and_then(|it| it.get("baseType"))
                            .and_then(|b| b.as_str())
                            .map(String::from);
                        let id = entry
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let money = Money {
                            amount,
                            currency: Self::parse_currency(code),
                        };
                        Some(Listing {
                            price: money,
                            price_divine,
                            explicit_count,
                            id,
                            base_type,
                            mods,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

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

    /// Fetches the raw `data/stats` catalog JSON.
    pub async fn fetch_stats_raw(&self) -> Result<String> {
        let url = format!("{TRADE_BASE}/data/stats");
        Ok(self
            .send_with_retry(&self.default_limiter, Endpoint::Fetch, || {
                self.http.get(&url)
            })
            .await
            .context("trade2 data/stats failed")?
            .text()
            .await?)
    }
}

#[async_trait]
impl TradeApi for TradeClient {
    async fn search(&self, query: &TradeQuery, session: &TradeSession) -> Result<SearchResponse> {
        let url = format!("{TRADE_BASE}/search/{}", query.league);
        let payload = to_payload(query);
        let resp = self
            .send_with_retry(&session.limiter, Endpoint::Search, || {
                with_cookie(session.client.post(&url).json(&payload), &session.cookie)
            })
            .await
            .context("trade2 search failed")?;
        let v: Value = resp.json().await?;
        let id = v
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        let total = v.get("total").and_then(|x| x.as_u64()).unwrap_or(0);
        let hashes = v
            .get("result")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|h| h.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        Ok(SearchResponse { id, total, hashes })
    }

    async fn fetch(
        &self,
        query_id: &str,
        hashes: &[String],
        session: &TradeSession,
    ) -> Result<Vec<Listing>> {
        // trade2 /fetch caps at 10 ids per request (>10 → HTTP 400), so fetch in
        // batches and concatenate. Each batch is paced by the limiter.
        let mut listings = Vec::new();
        for csv in fetch_batches(hashes) {
            let url = format!("{TRADE_BASE}/fetch/{csv}?query={query_id}");
            let v: Value = self
                .send_with_retry(&session.limiter, Endpoint::Fetch, || {
                    with_cookie(session.client.get(&url), &session.cookie)
                })
                .await
                .context("trade2 fetch failed")?
                .json()
                .await?;
            listings.extend(self.parse_fetch(&v));
        }
        Ok(listings)
    }
}

#[async_trait]
impl crate::trade::ablation::Comparables for TradeClient {
    /// Fetches comparable listings, with a 60-second in-memory TTL cache.
    ///
    /// The cache deduplicates repeated calls for the same query (e.g. the
    /// baseline probe issued by both `price` and `breakdown`), keeping trade2
    /// traffic polite.  We never hold the mutex across an `.await`; the pattern
    /// is: lock → check/copy → unlock → await → lock → insert → unlock.
    ///
    /// Relaxation is caller-controlled via `max_relax`: the routing probe and
    /// value-path sub-queries pass 0 (exact sampling), while breakdown probes
    /// pass 3 to recover enough comparables for delta measurement.
    async fn comparables(
        &self,
        query: &crate::trade::model::TradeQuery,
        limit: usize,
        max_relax: usize,
        session: &TradeSession,
    ) -> anyhow::Result<Vec<crate::trade::model::Listing>> {
        use std::time::{Duration, Instant};

        let key = format!(
            "{}|{}|{}",
            limit,
            max_relax,
            serde_json::to_string(query).unwrap_or_default()
        );

        // --- lock, check, unlock ---
        let cached = {
            let guard = self.cache.lock().unwrap();
            guard.get(&key).and_then(|(ts, listings)| {
                if ts.elapsed() < Duration::from_secs(60) {
                    Some(listings.clone())
                } else {
                    None
                }
            })
        };
        if let Some(listings) = cached {
            return Ok(listings);
        }

        // --- await (no mutex held) ---
        let result =
            crate::trade::ablation::gather_comparables(self, query, limit, max_relax, session)
                .await?;

        // --- lock, prune expired, insert, unlock ---
        {
            let mut guard = self.cache.lock().unwrap();
            guard.retain(|_, (ts, _)| ts.elapsed() < Duration::from_secs(60));
            guard.insert(key, (Instant::now(), result.clone()));
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rate_limit_rule_triples() {
        let rules = parse_rate_rules("5:10:60,15:60:120");
        assert_eq!(
            rules,
            vec![
                RateRule {
                    max: 5,
                    period: 10,
                    restriction: 60
                },
                RateRule {
                    max: 15,
                    period: 60,
                    restriction: 120
                }
            ]
        );
    }

    #[test]
    fn retry_after_prefers_retry_after_header() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, "12".parse().unwrap());
        assert_eq!(retry_after_secs(&h), 12);
    }

    #[test]
    fn retry_after_falls_back_to_rule_period() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("X-Rate-Limit-Ip", "5:10:60".parse().unwrap());
        assert_eq!(retry_after_secs(&h), 10);
    }

    fn test_client() -> TradeClient {
        TradeClient::new(
            None,
            std::sync::Arc::new(std::sync::RwLock::new(crate::trade::rates::RateTable::new(
                std::collections::HashMap::from([
                    ("divine".to_string(), 1.0),
                    ("chaos".to_string(), 0.1),
                ]),
            ))),
        )
        .unwrap()
    }

    #[test]
    fn parse_fetch_drops_unconvertible_currency_listings() {
        let client = test_client();
        let v = serde_json::json!({
            "result": [
                { "listing": { "price": { "amount": 2.0, "currency": "divine" } },
                  "item": { "explicitMods": ["a", "b", "c"] } },
                { "listing": { "price": { "amount": 1.0, "currency": "aug" } },
                  "item": { "explicitMods": ["x"] } },
                { "listing": { "price": { "amount": 50.0, "currency": "chaos" } },
                  "item": { "explicitMods": ["p", "q", "r", "s"] } }
            ]
        });
        let listings = client.parse_fetch(&v);
        // "aug" is unconvertible → dropped; divine + chaos kept, both positive.
        assert_eq!(listings.len(), 2);
        assert!(listings.iter().all(|l| l.price_divine > 0.0));
        // explicit_count via affix_count() — these fixtures have no `extended`, so it falls to the explicitMods line count
        let divine = listings.iter().find(|l| l.price.amount == 2.0).unwrap();
        assert_eq!(divine.explicit_count, 3);
        let chaos = listings.iter().find(|l| l.price.amount == 50.0).unwrap();
        assert_eq!(chaos.explicit_count, 4);
    }

    #[test]
    fn parse_fetch_affix_count_layers() {
        let client = test_client();
        let v = serde_json::json!({
            "result": [
                // Layer 1: extended.prefixes + suffixes (exact, hybrid-safe)
                { "listing": { "price": { "amount": 1.0, "currency": "divine" } },
                  "item": { "extended": { "prefixes": 2, "suffixes": 3 },
                            "explicitMods": ["a","b","c","d","e","f"] } },
                // Layer 2: extended.mods.explicit (one entry per affix)
                { "listing": { "price": { "amount": 2.0, "currency": "divine" } },
                  "item": { "extended": { "mods": { "explicit": ["x","y"] } },
                            "explicitMods": ["x1","x2","y1"] } },
                // Layer 3: explicitMods only (display lines)
                { "listing": { "price": { "amount": 3.0, "currency": "divine" } },
                  "item": { "explicitMods": ["p","q","r","s"] } },
                // Layer 4: nothing → 0 (unknown)
                { "listing": { "price": { "amount": 4.0, "currency": "divine" } },
                  "item": {} }
            ]
        });
        let ls = client.parse_fetch(&v);
        assert_eq!(ls.len(), 4);
        let ec = |amt: f64| {
            ls.iter()
                .find(|l| l.price.amount == amt)
                .unwrap()
                .explicit_count
        };
        assert_eq!(ec(1.0), 5); // 2 + 3, NOT the 6 explicitMods lines
        assert_eq!(ec(2.0), 2); // per-affix, NOT the 3 lines
        assert_eq!(ec(3.0), 4); // line count
        assert_eq!(ec(4.0), 0); // unknown
    }

    #[test]
    fn parse_fetch_extracts_id_and_stat_ids() {
        let client = test_client();
        let v = serde_json::json!({
            "result": [{
                "id": "abc123",
                "listing": { "price": { "amount": 1.0, "currency": "divine" } },
                "item": {
                    "explicitMods": [
                        { "hash": "stat.explicit.stat_2768835289", "mods": [] },
                        { "hash": "stat.explicit.stat_1050105434", "mods": [] }
                    ],
                    "extended": {
                        "hashes": { "explicit": [["explicit.stat_2768835289", [0]],
                                                 ["explicit.stat_1050105434", [1]]] }
                    }
                }
            }]
        });
        let ls = client.parse_fetch(&v);
        assert_eq!(ls.len(), 1);
        assert_eq!(ls[0].id, "abc123");
        // Both mods extracted via listing_mods.
        let stat_ids: Vec<&str> = ls[0].mods.iter().map(|m| m.stat_id.as_str()).collect();
        assert_eq!(
            stat_ids,
            vec!["explicit.stat_2768835289", "explicit.stat_1050105434"]
        );
    }

    #[test]
    fn parse_fetch_stat_ids_fall_back_to_explicit_mods_hash() {
        let client = test_client();
        let v = serde_json::json!({
            "result": [{
                "id": "x",
                "listing": { "price": { "amount": 1.0, "currency": "divine" } },
                "item": { "explicitMods": [ { "hash": "stat.explicit.stat_999" } ] }
            }]
        });
        let ls = client.parse_fetch(&v);
        // "stat." prefix stripped to match StatFilter ids.
        assert_eq!(ls[0].mods[0].stat_id, "explicit.stat_999");
    }

    #[test]
    fn parse_fetch_extracts_mods_with_tier_and_roll() {
        let client = test_client();
        let v = serde_json::json!({
            "result": [{
                "id": "abc123",
                "listing": { "price": { "amount": 1.0, "currency": "divine" } },
                "item": {
                    "explicitMods": [
                        { "name": "Sadistic", "hash": "stat.explicit.stat_2768835289",
                          "description": "123% increased Spell Physical Damage",
                          "mods": [ { "tier": "P5", "magnitudes": [ { "min": "109", "max": "128" } ] } ] }
                    ],
                    "extended": { "hashes": { "explicit": [["explicit.stat_2768835289", [0]]] } }
                }
            }]
        });
        let ls = client.parse_fetch(&v);
        assert_eq!(ls.len(), 1);
        assert_eq!(ls[0].mods.len(), 1);
        assert_eq!(ls[0].mods[0].stat_id, "explicit.stat_2768835289");
        assert_eq!(ls[0].mods[0].tier, Some(5)); // "P5" → 5
        assert_eq!(ls[0].mods[0].roll, Some(123.0)); // first number in the description
    }

    #[test]
    fn first_number_preserves_sign() {
        assert_eq!(
            first_number("123% increased Spell Physical Damage"),
            Some(123.0)
        );
        assert_eq!(first_number("+298 to maximum Mana"), Some(298.0));
        assert_eq!(first_number("Adds 5 to 10 Physical Damage"), Some(5.0));
        assert_eq!(first_number("-12% to Chaos Resistance"), Some(-12.0));
        assert_eq!(first_number("1.5% of Damage Leeched"), Some(1.5));
        assert_eq!(first_number("no digits here"), None);
    }

    #[test]
    fn listing_mods_falls_back_to_extended_hashes() {
        let client = test_client();
        // One explicitMods entry has a hash (rich); a second stat id exists ONLY in
        // extended.hashes.explicit (a display-string mod with no `hash`).
        let v = serde_json::json!({
            "result": [{
                "id": "x",
                "listing": { "price": { "amount": 1.0, "currency": "divine" } },
                "item": {
                    "explicitMods": [
                        { "hash": "stat.explicit.stat_AAA", "description": "50% increased",
                          "mods": [ { "tier": "P2" } ] }
                    ],
                    "extended": { "hashes": { "explicit": [
                        ["explicit.stat_AAA", [0]],
                        ["explicit.stat_BBB", [1]]
                    ] } }
                }
            }]
        });
        let ls = client.parse_fetch(&v);
        assert_eq!(ls.len(), 1);
        // The rich mod (AAA) keeps its tier; the fallback mod (BBB) is captured
        // stat-id-only so the corpus doesn't lose its presence.
        let aaa = ls[0]
            .mods
            .iter()
            .find(|m| m.stat_id == "explicit.stat_AAA")
            .unwrap();
        assert_eq!(aaa.tier, Some(2));
        let bbb = ls[0]
            .mods
            .iter()
            .find(|m| m.stat_id == "explicit.stat_BBB")
            .unwrap();
        assert_eq!(bbb.tier, None);
        assert_eq!(bbb.roll, None);
    }

    #[test]
    fn parse_fetch_extracts_base_type() {
        let client = test_client();
        let v = serde_json::json!({
            "result": [{
                "id": "abc",
                "listing": { "price": { "amount": 1.0, "currency": "divine" } },
                "item": { "baseType": "Chiming Staff", "explicitMods": [] }
            }]
        });
        let ls = client.parse_fetch(&v);
        assert_eq!(ls.len(), 1);
        assert_eq!(ls[0].base_type.as_deref(), Some("Chiming Staff"));
    }

    #[test]
    fn fetch_batches_caps_at_ten_ids() {
        let hashes: Vec<String> = (0..25).map(|i| format!("h{i}")).collect();
        let batches = fetch_batches(&hashes);
        assert_eq!(batches.len(), 3); // 10 + 10 + 5
        assert!(
            batches.iter().all(|b| b.split(',').count() <= FETCH_BATCH),
            "no batch may exceed the trade2 10-id /fetch cap"
        );
        assert_eq!(batches[0].split(',').count(), 10);
        assert_eq!(batches[2].split(',').count(), 5);
        assert!(fetch_batches(&[]).is_empty()); // empty input → no requests
    }

    #[tokio::test]
    #[ignore = "hits the live trade2 API"]
    async fn live_search_fetch_smoke() {
        use crate::trade::model::{MiscFilters, TradeQuery};
        let nc = crate::poeninja::NinjaClient::new().unwrap();
        let league = nc.current_league().await.unwrap().name;
        let rates = std::sync::Arc::new(std::sync::RwLock::new(
            crate::trade::rates::RateTable::new(nc.currency_rates(&league).await.unwrap()),
        ));
        let client = TradeClient::new(None, rates).unwrap();
        let q = TradeQuery {
            league: league.clone(),
            category: None,
            type_line: Some("Sapphire Ring".into()),
            stats: vec![],
            misc: MiscFilters::default(),
            equipment: vec![],
        };
        let session = crate::trade::session::TradeSession::for_test();
        let resp = client.search(&q, &session).await.unwrap();
        assert!(resp.total > 0);
        let listings = client
            .fetch(&resp.id, &resp.hashes[..resp.hashes.len().min(5)], &session)
            .await
            .unwrap();
        assert!(
            !listings.is_empty(),
            "expected non-empty listings with live rates"
        );
        assert!(listings.iter().all(|l| l.price_divine > 0.0));
    }
}
