//! trade2 HTTP client behind the `TradeApi` trait, with rate-limit-header
//! parsing. Anonymous by default; an optional POESESSID raises the ceiling.

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::{header, Client};
use serde_json::Value;

use crate::trade::model::{Currency, Listing, Money, SearchResponse, TradeQuery};
use crate::trade::query::to_payload;

const TRADE_BASE: &str = "https://www.pathofexile.com/api/trade2";
const USER_AGENT: &str =
    "dr-peste-redux/0.1 (Discord guild price bot; not affiliated with Grinding Gear Games)";

/// Divine-Orb conversion rates. v1 defaults; refreshing from the live currency
/// market is a later refinement.
#[derive(Clone, Debug)]
pub struct CurrencyRates {
    pub exalted_per_divine: f64,
    pub chaos_per_divine: f64,
}

impl Default for CurrencyRates {
    fn default() -> Self {
        CurrencyRates { exalted_per_divine: 180.0, chaos_per_divine: 2000.0 }
    }
}

impl CurrencyRates {
    pub fn to_divine(&self, m: &Money) -> f64 {
        match m.currency {
            Currency::Divine => m.amount,
            Currency::Exalted => m.amount / self.exalted_per_divine,
            Currency::Chaos => m.amount / self.chaos_per_divine,
            Currency::Other(_) => 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RateRule {
    pub max: u32,
    pub period: u32,
    pub restriction: u32,
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

/// Seconds to wait given the strictest rule and current hit count. 0 under limit.
pub fn backoff_secs(rules: &[RateRule], current_hits: u32) -> u64 {
    rules
        .iter()
        .filter(|r| current_hits >= r.max)
        .map(|r| r.period as u64)
        .max()
        .unwrap_or(0)
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
            .and_then(|v| parse_rate_rules(v).into_iter().map(|r| r.period as u64).max())
        {
            return period.clamp(1, 120);
        }
    }
    5
}

#[async_trait]
pub trait TradeApi {
    async fn search(&self, query: &TradeQuery) -> Result<SearchResponse>;
    async fn fetch(&self, query_id: &str, hashes: &[String]) -> Result<Vec<Listing>>;
}

pub struct TradeClient {
    http: Client,
    rates: CurrencyRates,
}

impl TradeClient {
    /// `poe_sessid` optional: when present it is sent as the POESESSID cookie to
    /// raise the rate-limit ceiling; otherwise requests are anonymous.
    pub fn new(poe_sessid: Option<String>) -> Result<Self> {
        let mut builder = Client::builder().user_agent(USER_AGENT);
        if let Some(sess) = poe_sessid.filter(|s| !s.is_empty()) {
            let mut headers = header::HeaderMap::new();
            let cookie = format!("POESESSID={sess}");
            headers.insert(header::COOKIE, header::HeaderValue::from_str(&cookie)?);
            builder = builder.default_headers(headers);
        }
        Ok(Self { http: builder.build()?, rates: CurrencyRates::default() })
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
                        let currency = Self::parse_currency(price.get("currency")?.as_str()?);
                        let money = Money { amount, currency };
                        let price_divine = self.rates.to_divine(&money);
                        Some(Listing { price: money, price_divine })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Sends a request, retrying up to twice on HTTP 429 after sleeping for the
    /// server-advised period. Other errors propagate immediately.
    async fn send_with_retry<F>(&self, build: F) -> Result<reqwest::Response>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut attempt = 0u32;
        loop {
            let resp = build().send().await?;
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
}

#[async_trait]
impl TradeApi for TradeClient {
    async fn search(&self, query: &TradeQuery) -> Result<SearchResponse> {
        let url = format!("{TRADE_BASE}/search/{}", query.league);
        let payload = to_payload(query);
        let resp = self
            .send_with_retry(|| self.http.post(&url).json(&payload))
            .await
            .context("trade2 search failed")?;
        let v: Value = resp.json().await?;
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string();
        let total = v.get("total").and_then(|x| x.as_u64()).unwrap_or(0);
        let hashes = v
            .get("result")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|h| h.as_str().map(String::from)).collect())
            .unwrap_or_default();
        Ok(SearchResponse { id, total, hashes })
    }

    async fn fetch(&self, query_id: &str, hashes: &[String]) -> Result<Vec<Listing>> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let csv = hashes.join(",");
        let url = format!("{TRADE_BASE}/fetch/{csv}?query={query_id}");
        let v: Value = self
            .send_with_retry(|| self.http.get(&url))
            .await
            .context("trade2 fetch failed")?
            .json()
            .await?;
        Ok(self.parse_fetch(&v))
    }
}

#[async_trait]
impl crate::trade::ablation::Comparables for TradeClient {
    async fn comparables(
        &self,
        query: &crate::trade::model::TradeQuery,
        limit: usize,
    ) -> anyhow::Result<Vec<crate::trade::model::Listing>> {
        crate::trade::ablation::gather_comparables(self, query, limit, 3).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rate_limit_rule_triples() {
        let rules = parse_rate_rules("5:10:60,15:60:120");
        assert_eq!(rules, vec![RateRule { max: 5, period: 10, restriction: 60 }, RateRule { max: 15, period: 60, restriction: 120 }]);
    }

    #[test]
    fn backoff_is_zero_when_under_limit_and_period_when_at_limit() {
        let rule = RateRule { max: 5, period: 10, restriction: 60 };
        assert_eq!(backoff_secs(&[rule.clone()], 3), 0);
        assert_eq!(backoff_secs(&[rule], 5), 10);
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

    #[tokio::test]
    #[ignore = "hits the live trade2 API"]
    async fn live_search_fetch_smoke() {
        use crate::trade::model::{MiscFilters, TradeQuery};
        let client = TradeClient::new(None).unwrap();
        let q = TradeQuery {
            league: live_league().await,
            category: None,
            type_line: Some("Sapphire Ring".into()),
            stats: vec![],
            misc: MiscFilters::default(),
        };
        let resp = client.search(&q).await.unwrap();
        assert!(resp.total > 0);
        let listings = client.fetch(&resp.id, &resp.hashes[..resp.hashes.len().min(5)]).await.unwrap();
        assert!(!listings.is_empty());
        assert!(listings.iter().all(|l| l.price_divine >= 0.0));
    }

    #[cfg(test)]
    async fn live_league() -> String {
        let nc = crate::poeninja::NinjaClient::new().unwrap();
        nc.current_league().await.unwrap().name
    }
}
