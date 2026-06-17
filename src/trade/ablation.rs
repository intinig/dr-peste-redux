//! Ablation pricing: gather comparables (relaxing thin queries), estimate a
//! price, and break a price down into per-characteristic contributions.

use anyhow::Result;
use async_trait::async_trait;

use crate::trade::client::TradeApi;
use crate::trade::model::{
    AblationKind, Breakdown, Confidence, Contribution, Listing, PriceEstimate, SynergyNote,
    TradeQuery,
};

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
        listings.sort_by(|a, b| {
            a.price_divine
                .partial_cmp(&b.price_divine)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if listings.len() >= limit || relaxations >= max_relax || q.stats.is_empty() {
            return Ok(listings);
        }
        q.stats.pop(); // relax the loosest-to-add constraint
        relaxations += 1;
    }
}

/// Cheapest, typical (low-percentile), and high prices over the comparables,
/// all in divine. `typical` is the cheapest (asking-price floor) — the most
/// defensible single number for "what it sells for".
pub async fn estimate<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
) -> Result<PriceEstimate> {
    let listings = c.comparables(query, limit).await?;
    Ok(estimate_from(&listings))
}

fn estimate_from(listings: &[Listing]) -> PriceEstimate {
    let mut prices: Vec<f64> = listings.iter().map(|l| l.price_divine).collect();
    prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = prices.len();
    let (low, typical, high) = if n == 0 {
        (0.0, 0.0, 0.0)
    } else {
        let low = prices[0];
        let typical = prices[0];
        let high = prices[(n * 3 / 4).min(n - 1)]; // ~75th percentile
        (low, typical, high)
    };
    PriceEstimate {
        low,
        typical,
        high,
        listing_count: n,
        confidence: Confidence::from_count(n),
    }
}

/// Ablate the top-`k` stat filters (single-drop), ranked by delta, plus one
/// pairwise probe on the top two to flag synergy.
///
/// Query budget per call: 1 baseline + min(k, stats.len()) single-drops + 1 pairwise.
/// v1 stat selection: iterate `query.stats` in order and take the first `k`
/// (a smarter clipboard heuristic is a documented follow-up).
pub async fn breakdown<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    k: usize,
) -> Result<Breakdown> {
    let baseline = estimate(c, query, limit).await?;

    // Select at most k stats to probe (v1: take first k in order).
    let probe_count = k.max(1).min(query.stats.len());

    let mut ranked: Vec<Contribution> = Vec::new();
    for i in 0..probe_count {
        let sf = &query.stats[i];
        let mut q = query.clone();
        q.stats.remove(i);
        let without = estimate(c, &q, limit).await?;
        ranked.push(Contribution {
            characteristic: sf.label.clone(),
            kind: AblationKind::Drop,
            delta_divine: baseline.typical - without.typical,
        });
    }
    ranked.sort_by(|a, b| {
        b.delta_divine
            .partial_cmp(&a.delta_divine)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Defensive truncate — ranked already has at most k entries, but keep for clarity.
    ranked.truncate(k.max(1));

    // Pairwise synergy on the top two (by name → find their indices in query).
    let synergy = if ranked.len() >= 2 {
        let a_label = ranked[0].characteristic.clone();
        let b_label = ranked[1].characteristic.clone();
        let a_idx = query.stats.iter().position(|s| s.label == a_label);
        let b_idx = query.stats.iter().position(|s| s.label == b_label);
        match (a_idx, b_idx) {
            (Some(ai), Some(bi)) if ai != bi => {
                let mut q = query.clone();
                // remove higher index first to keep the other valid
                let (hi, lo) = if ai > bi { (ai, bi) } else { (bi, ai) };
                q.stats.remove(hi);
                q.stats.remove(lo);
                let without_both = estimate(c, &q, limit).await?;
                let drop_both = baseline.typical - without_both.typical;
                let sum_individual = ranked[0].delta_divine + ranked[1].delta_divine;
                // Super-additive synergy: removing both costs more than the sum
                // of removing each individually.
                let extra = sum_individual - drop_both;
                if extra > f64::EPSILON {
                    Some(SynergyNote {
                        a: a_label,
                        b: b_label,
                        extra_divine: extra,
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    } else {
        None
    };

    Ok(Breakdown {
        baseline,
        ranked,
        synergy,
        trade_url: trade_url(query),
    })
}

/// Human-clickable trade2 search URL for the item's league (a fresh search; the
/// API search id is ephemeral, so we link to the site search page instead).
pub fn trade_url(query: &TradeQuery) -> String {
    format!("https://www.pathofexile.com/trade2/search/{}", query.league)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::client::TradeApi;
    use crate::trade::model::{
        AblationKind, Confidence, Currency, Listing, MiscFilters, Money, SearchResponse,
        StatFilter, TradeQuery,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn listing(divine: f64) -> Listing {
        Listing {
            price: Money {
                amount: divine,
                currency: Currency::Divine,
            },
            price_divine: divine,
        }
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
            Ok(SearchResponse {
                id: "qid".into(),
                total: n as u64,
                hashes,
            })
        }
        async fn fetch(&self, _id: &str, hashes: &[String]) -> anyhow::Result<Vec<Listing>> {
            Ok(hashes
                .iter()
                .enumerate()
                .map(|(i, _)| listing(10.0 + i as f64))
                .collect())
        }
    }

    fn q_with(n_stats: usize) -> TradeQuery {
        TradeQuery {
            league: "Standard".into(),
            category: None,
            type_line: Some("Sapphire Ring".into()),
            stats: (0..n_stats)
                .map(|i| StatFilter {
                    id: format!("s{i}"),
                    label: format!("s{i}"),
                    min: Some(10.0),
                    max: None,
                })
                .collect(),
            misc: MiscFilters::default(),
        }
    }

    #[tokio::test]
    async fn relaxes_until_min_listings_reached() {
        let api = FakeApi {
            seen: Mutex::new(vec![]),
        };
        // 3 stats → 1 listing (< k=5). Must relax (drop a stat) until ≥ 5.
        let got = gather_comparables(&api, &q_with(3), 5, 3).await.unwrap();
        assert!(got.len() >= 5);
    }

    /// Fake high-level Comparables: maps a query to a fixed price based on which
    /// stat ids are present, so ablation deltas are deterministic.
    struct FakePricer;

    #[async_trait]
    impl Comparables for FakePricer {
        async fn comparables(&self, q: &TradeQuery, _limit: usize) -> anyhow::Result<Vec<Listing>> {
            // base 5; +10 if "spell" present; +2 if "crit" present; +6 extra if BOTH (synergy)
            let has_spell = q.stats.iter().any(|s| s.id.contains("spell"));
            let has_crit = q.stats.iter().any(|s| s.id.contains("crit"));
            let mut price = 5.0;
            if has_spell {
                price += 10.0;
            }
            if has_crit {
                price += 2.0;
            }
            if has_spell && has_crit {
                price += 6.0;
            }
            Ok(vec![listing(price); 12]) // 12 listings → High confidence
        }
    }

    fn two_stat_query() -> TradeQuery {
        TradeQuery {
            league: "Standard".into(),
            category: None,
            type_line: Some("Expert Crackling Staff".into()),
            stats: vec![
                StatFilter {
                    id: "explicit.spell".into(),
                    label: "+to all Spell Skills".into(),
                    min: Some(7.0),
                    max: None,
                },
                StatFilter {
                    id: "explicit.crit".into(),
                    label: "Critical Chance".into(),
                    min: Some(80.0),
                    max: None,
                },
            ],
            misc: MiscFilters::default(),
        }
    }

    #[tokio::test]
    async fn estimate_reports_typical_and_confidence() {
        let est = estimate(&FakePricer, &two_stat_query(), 10).await.unwrap();
        assert_eq!(est.listing_count, 12);
        assert_eq!(est.confidence, Confidence::High);
        // both stats present → 5+10+2+6 = 23
        assert_eq!(est.typical, 23.0);
    }

    /// Counting fake: increments an atomic counter on every `comparables` call,
    /// and always returns a fixed set of listings so estimates are well-defined.
    struct CountingComparables {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl Comparables for CountingComparables {
        async fn comparables(
            &self,
            _q: &TradeQuery,
            _limit: usize,
        ) -> anyhow::Result<Vec<Listing>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Return 12 listings at a fixed price so estimates always succeed.
            Ok(vec![listing(10.0); 12])
        }
    }

    #[tokio::test]
    async fn breakdown_query_budget_is_bounded_by_k() {
        // Build a query with 6 stat filters.
        let q = q_with(6);
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fake = CountingComparables {
            calls: calls.clone(),
        };
        // k=4 → budget ≤ 1 baseline + 4 single-drops + 1 pairwise = 6
        breakdown(&fake, &q, 10, 4).await.unwrap();
        let n = calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!(n <= 6, "expected ≤ 6 comparables calls (1+4+1), got {n}");
    }

    #[tokio::test]
    async fn breakdown_ranks_contributions_and_flags_synergy() {
        let bd = breakdown(&FakePricer, &two_stat_query(), 10, 2)
            .await
            .unwrap();
        // baseline 23; drop spell → 5+2 = 7 (delta 16); drop crit → 5+10 = 15 (delta 8)
        assert_eq!(bd.ranked[0].characteristic, "+to all Spell Skills");
        assert_eq!(bd.ranked[0].delta_divine, 16.0);
        assert_eq!(bd.ranked[0].kind, AblationKind::Drop);
        assert_eq!(bd.ranked[1].delta_divine, 8.0);
        // drop-both → 5 (delta 18). individual deltas sum 16+8=24. extra = 24-18 = 6.
        let syn = bd.synergy.unwrap();
        assert_eq!(syn.extra_divine, 6.0);
    }
}
