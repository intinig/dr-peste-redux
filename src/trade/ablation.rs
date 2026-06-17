//! Ablation pricing: gather comparables (relaxing thin queries), estimate a
//! price, and break a price down into per-characteristic contributions.

use anyhow::Result;
use async_trait::async_trait;

use crate::trade::client::TradeApi;
use crate::trade::model::{Listing, TradeQuery};

/// High-level seam the pricer depends on. `TradeClient` implements it via
/// `gather_comparables`; tests fake it directly.
#[async_trait]
pub trait Comparables {
    async fn comparables(&self, query: &TradeQuery, limit: usize) -> Result<Vec<Listing>>;
}

/// Searches + fetches up to `limit` cheapest listings. If fewer than `limit`
/// are found, relaxes the query (drops the last stat filter) and retries, up to
/// `max_relax` times. Returns whatever it has (possibly empty).
pub async fn gather_comparables<A: TradeApi + ?Sized>(
    api: &A,
    query: &TradeQuery,
    limit: usize,
    max_relax: usize,
) -> Result<Vec<Listing>> {
    let mut q = query.clone();
    let mut relaxations = 0;
    loop {
        let resp = api.search(&q).await?;
        let take = resp.hashes.len().min(limit);
        let mut listings = api.fetch(&resp.id, &resp.hashes[..take]).await?;
        listings.sort_by(|a, b| a.price_divine.partial_cmp(&b.price_divine).unwrap_or(std::cmp::Ordering::Equal));
        if listings.len() >= limit || relaxations >= max_relax || q.stats.is_empty() {
            return Ok(listings);
        }
        q.stats.pop(); // relax the loosest-to-add constraint
        relaxations += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::client::TradeApi;
    use crate::trade::model::{Currency, Listing, MiscFilters, Money, SearchResponse, StatFilter, TradeQuery};
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn listing(divine: f64) -> Listing {
        Listing { price: Money { amount: divine, currency: Currency::Divine }, price_divine: divine }
    }

    /// Fake low-level API: returns listings whose count/prices depend on how
    /// many stat filters the query still carries (more constraints → fewer,
    /// pricier listings). Records the queries it saw.
    struct FakeApi {
        seen: Mutex<Vec<TradeQuery>>,
    }

    #[async_trait]
    impl TradeApi for FakeApi {
        async fn search(&self, q: &TradeQuery) -> anyhow::Result<SearchResponse> {
            self.seen.lock().unwrap().push(q.clone());
            let n = 1 + (3usize.saturating_sub(q.stats.len())) * 4;
            let hashes = (0..n).map(|i| format!("h{i}")).collect::<Vec<_>>();
            Ok(SearchResponse { id: "qid".into(), total: n as u64, hashes })
        }
        async fn fetch(&self, _id: &str, hashes: &[String]) -> anyhow::Result<Vec<Listing>> {
            Ok(hashes.iter().enumerate().map(|(i, _)| listing(10.0 + i as f64)).collect())
        }
    }

    fn q_with(n_stats: usize) -> TradeQuery {
        TradeQuery {
            league: "Standard".into(),
            category: None,
            type_line: Some("Sapphire Ring".into()),
            stats: (0..n_stats)
                .map(|i| StatFilter { id: format!("s{i}"), label: format!("s{i}"), min: Some(10.0), max: None })
                .collect(),
            misc: MiscFilters::default(),
        }
    }

    #[tokio::test]
    async fn relaxes_until_min_listings_reached() {
        let api = FakeApi { seen: Mutex::new(vec![]) };
        // 3 stats → 1 listing (< k=5). Must relax (drop a stat) until ≥ 5.
        let got = gather_comparables(&api, &q_with(3), 5, 3).await.unwrap();
        assert!(got.len() >= 5);
    }
}
