//! Ablation pricing: gather comparables (relaxing thin queries), estimate a
//! price, and break a price down into per-characteristic contributions.

use anyhow::Result;
use async_trait::async_trait;

use crate::trade::client::TradeApi;
use crate::trade::model::{
    AblationKind, Breakdown, Confidence, Contribution, Currency, EstimateBasis, Listing,
    PriceEstimate, SynergyNote, TradeQuery,
};
use crate::trade::session::TradeSession;

/// High-level seam the pricer depends on. `TradeClient` implements it via
/// `gather_comparables`; tests fake it directly.
#[async_trait]
pub trait Comparables {
    async fn comparables(
        &self,
        query: &TradeQuery,
        limit: usize,
        session: &TradeSession,
    ) -> Result<Vec<Listing>>;
}

/// Hard cap on how many stats a single breakdown will probe, to bound the
/// query budget for pathological items. Normal rares have far fewer.
const PROBE_CEILING: usize = 16;

/// Bottom fraction of (sorted-ascending) comparables dropped as dump/troll outliers.
const TRIM_BOTTOM_FRAC: f64 = 0.10;
/// Only trim when at least this many comparables survive the craftability filter.
const TRIM_MIN_N: usize = 8;

/// Searches + fetches up to `limit` cheapest listings. If fewer than `limit`
/// are found, relaxes the query (drops the last stat filter, then the last
/// equipment band) and retries, up to `max_relax` times. Returns whatever it
/// has (possibly empty).
pub async fn gather_comparables<A: TradeApi + ?Sized>(
    api: &A,
    query: &TradeQuery,
    limit: usize,
    max_relax: usize,
    session: &TradeSession,
) -> Result<Vec<Listing>> {
    let mut q = query.clone();
    let mut relaxations = 0;
    loop {
        let resp = api.search(&q, session).await?;
        let take = resp.hashes.len().min(limit);
        let mut listings = api.fetch(&resp.id, &resp.hashes[..take], session).await?;
        listings.sort_by(|a, b| {
            a.price_divine
                .partial_cmp(&b.price_divine)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let exhausted = q.stats.is_empty() && q.equipment.is_empty();
        if listings.len() >= limit || relaxations >= max_relax || exhausted {
            return Ok(listings);
        }
        // Relax the loosest constraint: stat filters first, then equipment bands.
        if !q.stats.is_empty() {
            q.stats.pop();
        } else {
            q.equipment.pop();
        }
        relaxations += 1;
    }
}

/// Keep listings in the same-or-more-open craftability tier as our item:
/// those with no extra explicit mods beyond the ones the search already pinned.
fn craftability_filter(listings: &[Listing], max_explicit: usize) -> Vec<Listing> {
    listings
        .iter()
        .filter(|l| l.explicit_count <= max_explicit)
        .cloned()
        .collect()
}

/// Price comparables for `query`, filtering to the item's craftability tier when
/// `max_explicit` is `Some` (falling back to a broad-market estimate if no
/// comparable bases survive, or to affixes-only when craftability is unknown).
pub async fn estimate<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    session: &TradeSession,
    max_explicit: Option<usize>,
) -> Result<PriceEstimate> {
    let listings = c.comparables(query, limit, session).await?;
    let est = match max_explicit {
        None => estimate_from(&listings, EstimateBasis::AffixesOnly),
        Some(max) => {
            let kept = craftability_filter(&listings, max);
            if kept.is_empty() {
                estimate_from(&listings, EstimateBasis::BroadMarket)
            } else {
                estimate_from(&kept, EstimateBasis::CraftTier)
            }
        }
    };
    Ok(est)
}

/// Linear-interpolation percentile of an ascending-sorted slice. `p` in [0,1].
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = p * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (rank - lo as f64)
}

/// The currency most listings are priced in (the market's preferred unit for
/// this item). Defaults to Divine when there are no listings.
fn modal_currency(listings: &[Listing]) -> Currency {
    use std::collections::HashMap;
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for l in listings {
        *counts.entry(l.price.currency.code()).or_insert(0) += 1;
    }
    match counts.into_iter().max_by_key(|(_, n)| *n).map(|(c, _)| c) {
        Some("exalted") => Currency::Exalted,
        Some("chaos") => Currency::Chaos,
        Some("divine") => Currency::Divine,
        Some(other) => Currency::Other(other.to_string()),
        None => Currency::Divine,
    }
}

fn estimate_from(listings: &[Listing], basis: EstimateBasis) -> PriceEstimate {
    let mut prices: Vec<f64> = listings.iter().map(|l| l.price_divine).collect();
    prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Trim the cheapest outliers (dump/troll listings) when we have enough.
    let priced: &[f64] = if prices.len() >= TRIM_MIN_N {
        let drop = ((prices.len() as f64) * TRIM_BOTTOM_FRAC).floor() as usize;
        &prices[drop..]
    } else {
        &prices[..]
    };

    let (low, typical, high) = if priced.is_empty() {
        (0.0, 0.0, 0.0)
    } else {
        (
            percentile(priced, 0.20),
            percentile(priced, 0.50),
            percentile(priced, 0.80),
        )
    };
    // listing_count reports the comparable set size (pre-trim) — trimming is an
    // internal outlier guard, not a change to "how many comps we found".
    PriceEstimate {
        low,
        typical,
        high,
        listing_count: listings.len(),
        confidence: Confidence::from_count(listings.len()),
        modal_currency: modal_currency(listings),
        basis,
    }
}

/// Ablate every stat filter (up to `PROBE_CEILING`), rank by measured price delta,
/// and display the top-`k`; plus one pairwise probe on the top two for synergy.
/// All probes share `max_explicit`, so deltas compare the same craftability tier.
///
/// Query budget per call: 1 baseline + min(n, ceiling) single-drops + 1 pairwise
/// (deduplicated by the client's 60s query cache).
pub async fn breakdown<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    k: usize,
    session: &TradeSession,
    max_explicit: Option<usize>,
) -> Result<Breakdown> {
    let baseline = estimate(c, query, limit, session, max_explicit).await?;

    // Probe every stat up to the ceiling so ranking is by measured impact.
    let probe_count = query.stats.len().min(PROBE_CEILING);

    let mut ranked: Vec<Contribution> = Vec::new();
    for i in 0..probe_count {
        let sf = &query.stats[i];
        let mut q = query.clone();
        q.stats.remove(i);
        let without = estimate(c, &q, limit, session, max_explicit).await?;
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
    // Truncate display to top-k by measured delta.
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
                let without_both = estimate(c, &q, limit, session, max_explicit).await?;
                let drop_both = baseline.typical - without_both.typical;
                let sum_individual = ranked[0].delta_divine + ranked[1].delta_divine;
                // Super-additive synergy: A and B are worth more together than apart.
                // The shared interaction term is counted in BOTH single-drop deltas,
                // so `sum_individual - drop_both` isolates it — positive exactly when
                // the pair is super-additive.
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

/// Percent-encodes a string as a URL path segment: RFC 3986 unreserved chars
/// are kept, everything else (spaces, reserved chars, non-ASCII) is encoded, so
/// arbitrary league names produce a well-formed URL.
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Human-clickable trade2 search URL for the item's league (a fresh search; the
/// API search id is ephemeral, so we link to the site search page). The PoE2
/// trade site route is realm-namespaced (`/trade2/search/poe2/<league>`), and
/// the league is percent-encoded so the embed `url` is always well-formed.
pub fn trade_url(query: &TradeQuery) -> String {
    format!(
        "https://www.pathofexile.com/trade2/search/poe2/{}",
        encode_segment(&query.league)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::client::TradeApi;
    use crate::trade::model::{
        AblationKind, Confidence, Currency, EstimateBasis, Listing, MiscFilters, Money,
        SearchResponse, StatFilter, TradeQuery,
    };
    use crate::trade::session::TradeSession;
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn listing(divine: f64) -> Listing {
        Listing {
            price: Money {
                amount: divine,
                currency: Currency::Divine,
            },
            price_divine: divine,
            explicit_count: 0,
        }
    }

    fn listing_ec(divine: f64, explicit_count: usize) -> Listing {
        Listing {
            price: Money {
                amount: divine,
                currency: Currency::Divine,
            },
            price_divine: divine,
            explicit_count,
        }
    }

    #[test]
    fn estimate_trims_bottom_and_uses_p20_p50_p80() {
        // 10 listings 1..=10 div. Trim bottom 10% (drop the 1.0), then
        // p20/p50/p80 over [2..10]; listing_count still reports the full 10.
        let ls: Vec<Listing> = (1..=10).map(|i| listing_ec(i as f64, 4)).collect();
        let est = estimate_from(&ls, EstimateBasis::CraftTier);
        assert_eq!(est.basis, EstimateBasis::CraftTier);
        assert_eq!(est.listing_count, 10); // pre-trim comparable count
        assert!(est.low < est.typical && est.typical < est.high);
        assert!((est.typical - 6.0).abs() < 0.001); // median of [2..10] = 6
    }

    #[test]
    fn estimate_no_trim_when_below_min_n() {
        let ls = vec![listing_ec(2.0, 4), listing_ec(4.0, 4), listing_ec(6.0, 4)];
        let est = estimate_from(&ls, EstimateBasis::BroadMarket);
        assert_eq!(est.listing_count, 3); // < TRIM_MIN_N → no trim
        assert!((est.typical - 4.0).abs() < 0.001); // median of [2,4,6] = 4
    }

    /// Fake low-level API: returns listings whose count/prices depend on how
    /// many stat filters the query still carries (more constraints → fewer,
    /// pricier listings). Records the queries it saw.
    struct FakeApi {
        seen: Mutex<Vec<TradeQuery>>,
    }

    #[async_trait]
    impl TradeApi for FakeApi {
        async fn search(
            &self,
            q: &TradeQuery,
            _session: &TradeSession,
        ) -> anyhow::Result<SearchResponse> {
            self.seen.lock().unwrap().push(q.clone());
            let n = 1 + (3usize.saturating_sub(q.stats.len() + q.equipment.len())) * 4;
            let hashes = (0..n).map(|i| format!("h{i}")).collect::<Vec<_>>();
            Ok(SearchResponse {
                id: "qid".into(),
                total: n as u64,
                hashes,
            })
        }
        async fn fetch(
            &self,
            _id: &str,
            hashes: &[String],
            _session: &TradeSession,
        ) -> anyhow::Result<Vec<Listing>> {
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
            equipment: vec![],
        }
    }

    #[tokio::test]
    async fn relaxes_until_min_listings_reached() {
        let api = FakeApi {
            seen: Mutex::new(vec![]),
        };
        // 3 stats → 1 listing (< k=5). Must relax (drop a stat) until ≥ 5.
        let got = gather_comparables(&api, &q_with(3), 5, 3, &TradeSession::for_test())
            .await
            .unwrap();
        assert!(got.len() >= 5);
    }

    #[tokio::test]
    async fn relaxes_equipment_when_stats_exhausted() {
        // No stat filters, only equipment bands: relaxation must drop equipment
        // (otherwise a too-tight defence band returns a thin result).
        let api = FakeApi {
            seen: Mutex::new(vec![]),
        };
        let q = TradeQuery {
            league: "Standard".into(),
            category: None,
            type_line: Some("Sandsworn Sandals".into()),
            stats: vec![],
            misc: MiscFilters::default(),
            equipment: (0..3)
                .map(|i| crate::trade::model::EquipFilter {
                    key: format!("e{i}"),
                    min: Some(50.0),
                    max: None,
                })
                .collect(),
        };
        let got = gather_comparables(&api, &q, 5, 3, &TradeSession::for_test())
            .await
            .unwrap();
        assert!(
            got.len() >= 5,
            "should relax equipment bands to reach the limit"
        );
    }

    /// Fake high-level Comparables: maps a query to a fixed price based on which
    /// stat ids are present, so ablation deltas are deterministic.
    struct FakePricer;

    #[async_trait]
    impl Comparables for FakePricer {
        async fn comparables(
            &self,
            q: &TradeQuery,
            _limit: usize,
            _session: &TradeSession,
        ) -> anyhow::Result<Vec<Listing>> {
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
            equipment: vec![],
        }
    }

    #[tokio::test]
    async fn estimate_reports_typical_and_confidence() {
        let est = estimate(
            &FakePricer,
            &two_stat_query(),
            10,
            &TradeSession::for_test(),
            None,
        )
        .await
        .unwrap();
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
            _session: &TradeSession,
        ) -> anyhow::Result<Vec<Listing>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Return 12 listings at a fixed price so estimates always succeed.
            Ok(vec![listing(10.0); 12])
        }
    }

    #[tokio::test]
    async fn breakdown_probes_all_stats_ranks_by_delta() {
        // Build a query with 6 stat filters.
        let q = q_with(6);
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fake = CountingComparables {
            calls: calls.clone(),
        };
        // 6 stats, k=4, ceiling=16 → 1 baseline + 6 single-drops + 1 pairwise = 8
        let bd = breakdown(&fake, &q, 10, 4, &TradeSession::for_test(), None)
            .await
            .unwrap();
        let n = calls.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(n, 8, "expected 8 comparables calls (1+6+1), got {n}");
        assert_eq!(bd.ranked.len(), 4, "display should be truncated to k=4");
    }

    #[tokio::test]
    async fn breakdown_ranks_contributions_and_flags_synergy() {
        let bd = breakdown(
            &FakePricer,
            &two_stat_query(),
            10,
            2,
            &TradeSession::for_test(),
            None,
        )
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

    #[test]
    fn percentile_interpolates_correctly() {
        assert_eq!(super::percentile(&[10.0, 20.0, 30.0, 40.0], 0.25), 17.5);
        assert_eq!(super::percentile(&[10.0], 0.5), 10.0);
        assert_eq!(super::percentile(&[], 0.5), 0.0);
    }

    struct FixedListings(Vec<Listing>);
    #[async_trait]
    impl Comparables for FixedListings {
        async fn comparables(
            &self,
            _q: &TradeQuery,
            _limit: usize,
            _session: &TradeSession,
        ) -> anyhow::Result<Vec<Listing>> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn craftability_filter_keeps_same_or_more_open() {
        let ls = vec![
            listing_ec(2.0, 4),  // our tier (clean base, explicit_count == ours)
            listing_ec(0.05, 6), // bad-filled (more mods) → dropped
            listing_ec(1.5, 3),  // cleaner (fewer mods) → kept
            listing_ec(0.04, 5), // more-filled → dropped
        ];
        let kept = craftability_filter(&ls, 4);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|l| l.explicit_count <= 4));
    }

    #[tokio::test]
    async fn estimate_filters_to_craft_tier_not_floor() {
        // Junk floor (cheap, 6 mods) vs open-tier bases (~2 div, 4 mods).
        // Filtering to explicit_count<=4 must ignore the floor.
        let mut ls = vec![
            listing_ec(0.03, 6),
            listing_ec(0.04, 6),
            listing_ec(0.05, 6),
        ];
        ls.extend((0..8).map(|i| listing_ec(1.8 + i as f64 * 0.1, 4))); // ~1.8–2.5
        let c = FixedListings(ls);
        let est = estimate(
            &c,
            &two_stat_query(),
            30,
            &TradeSession::for_test(),
            Some(4),
        )
        .await
        .unwrap();
        assert_eq!(est.basis, EstimateBasis::CraftTier);
        assert!(
            est.typical >= 1.5,
            "fair {} should reflect open tier, not the 0.05 floor",
            est.typical
        );
    }

    #[tokio::test]
    async fn estimate_falls_back_to_broad_market_when_no_comparable_bases() {
        // Every listing is more-filled than ours → 0 survivors → BroadMarket.
        let c = FixedListings(vec![
            listing_ec(0.03, 6),
            listing_ec(0.04, 6),
            listing_ec(0.05, 6),
        ]);
        let est = estimate(
            &c,
            &two_stat_query(),
            30,
            &TradeSession::for_test(),
            Some(4),
        )
        .await
        .unwrap();
        assert_eq!(est.basis, EstimateBasis::BroadMarket);
        assert!(est.typical > 0.0);
    }

    #[tokio::test]
    async fn estimate_affixes_only_when_craftability_unknown() {
        let c = FixedListings(vec![listing_ec(0.03, 6), listing_ec(2.0, 4)]);
        let est = estimate(&c, &two_stat_query(), 30, &TradeSession::for_test(), None)
            .await
            .unwrap();
        assert_eq!(est.basis, EstimateBasis::AffixesOnly);
    }

    #[test]
    fn trade_url_has_poe2_realm_and_encodes_league() {
        let q = TradeQuery {
            league: "Runes of Aldur".into(),
            category: None,
            type_line: None,
            stats: vec![],
            misc: MiscFilters::default(),
            equipment: vec![],
        };
        let url = trade_url(&q);
        assert!(
            !url.contains(' '),
            "url must not contain a raw space: {url}"
        );
        assert!(
            url.ends_with("/trade2/search/poe2/Runes%20of%20Aldur"),
            "{url}"
        );
        // reserved characters in a league name are percent-encoded
        let q2 = TradeQuery {
            league: "A/B".into(),
            ..q
        };
        let url2 = trade_url(&q2);
        assert!(url2.ends_with("/poe2/A%2FB"), "{url2}");
    }
}
