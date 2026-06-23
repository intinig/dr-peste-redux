//! Ablation pricing: gather comparables (relaxing thin queries), estimate a
//! price, and break a price down into per-characteristic contributions.

use anyhow::Result;
use async_trait::async_trait;

use crate::trade::age::{is_fresh_at, now_unix, MAX_LISTING_AGE_DAYS};
use crate::trade::client::TradeApi;
use crate::trade::model::{
    AblationKind, Breakdown, Confidence, Contribution, Currency, EstimateBasis, Listing,
    PriceEstimate, SynergyNote, TradeQuery,
};
use crate::trade::session::TradeSession;

/// High-level seam the pricer depends on. `TradeClient` implements it via
/// `gather_comparables`; tests fake it directly. `max_relax` lets callers control
/// whether query relaxation is used (0 = exact sampling). `min_matches` is the
/// relaxation target: relax only until at least this many comparables are found.
/// Pass `1` for the tightest-non-empty read (price-check: the closest comparables,
/// even if few) or `MIN_COMPARABLES` when a fuller sample is wanted before the
/// craftability filter runs (breakdown/estimate).
#[async_trait]
pub trait Comparables {
    async fn comparables(
        &self,
        query: &TradeQuery,
        limit: usize,
        max_relax: usize,
        min_matches: usize,
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
/// Default relaxation target for the broad/breakdown path: relax until at least
/// this many comparables are found, so the craftability filter has a real sample
/// to work with. The price-check path overrides this with `1` (tightest non-empty).
pub(crate) const MIN_COMPARABLES: usize = 10;

pub async fn gather_comparables<A: TradeApi + ?Sized>(
    api: &A,
    query: &TradeQuery,
    limit: usize,
    max_relax: usize,
    min_matches: usize,
    session: &TradeSession,
) -> Result<Vec<Listing>> {
    let mut q = query.clone();
    let mut relaxations = 0;
    // One clock read per call keeps freshness decisions consistent across the
    // deep-fetch walk and any relaxations (and avoids repeated syscalls).
    let now = now_unix();
    loop {
        let resp = api.search(&q, session).await?;
        // Collect the cheapest `limit` *fresh* listings, fetching progressively
        // deeper into the price-sorted results when the cheap prefix is stale.
        // Stale listings are disproportionately cheap dregs (corpus age EDA), so the
        // cheapest `limit` hashes can be all-stale while fresh matches sit just above
        // them — fetching only that prefix then dropping stale would make a tight
        // query look empty and wrongly relax to a broader, cheaper market. Walking
        // in `limit`-sized windows stops at the first window for a healthy market
        // (same cost as before) and only digs when the cheap tail is stale.
        let mut listings: Vec<Listing> = Vec::new();
        let mut cursor = 0;
        while listings.len() < limit && cursor < resp.hashes.len() {
            let end = (cursor + limit).min(resp.hashes.len());
            let batch = api
                .fetch(&resp.id, &resp.hashes[cursor..end], session)
                .await?;
            listings.extend(
                batch
                    .into_iter()
                    .filter(|l| is_fresh_at(l.indexed.as_deref(), now, MAX_LISTING_AGE_DAYS)),
            );
            cursor = end;
        }
        // Accumulated in price-ascending hash order; keep the cheapest `limit` fresh.
        listings.truncate(limit);
        listings.sort_by(|a, b| {
            a.price_divine
                .partial_cmp(&b.price_divine)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let exhausted = q.stats.is_empty() && q.equipment.is_empty();
        if listings.len() >= min_matches || relaxations >= max_relax || exhausted {
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

/// Keep listings in the same-or-more-open craftability tier as our item: those
/// with no extra explicit mods beyond the ones the search already pinned.
/// `explicit_count == 0` is the "unknown/absent mods" sentinel from `parse_fetch`
/// (a real matching rare always has ≥1 explicit), so exclude it rather than treat
/// it as the most-craftable tier — that keeps the junk floor out when mod data is
/// missing (the sample then falls back to BroadMarket).
fn craftability_filter(listings: &[Listing], max_explicit: usize) -> Vec<Listing> {
    listings
        .iter()
        .filter(|l| l.explicit_count >= 1 && l.explicit_count <= max_explicit)
        .cloned()
        .collect()
}

/// Price comparables for `query`, filtering to the item's craftability tier when
/// `max_explicit` is `Some` (falling back to a broad-market estimate if no
/// comparable bases survive, or to affixes-only when craftability is unknown).
/// `max_relax` is forwarded to `c.comparables` unchanged.
pub async fn estimate<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    max_relax: usize,
    session: &TradeSession,
    max_explicit: Option<usize>,
) -> Result<PriceEstimate> {
    // Relax until a real sample (MIN_COMPARABLES) is found before the craftability
    // filter runs, so a tight query returning only filled/unknown listings doesn't
    // collapse straight to BroadMarket.
    let listings = c
        .comparables(query, limit, max_relax, MIN_COMPARABLES, session)
        .await?;
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

/// Relax-and-read price-check, reading p20/p50/p80 over the cheapest matches.
/// No craftability filter — the query constraint plus the cheapest-first read
/// define the comparable set.
///
/// Exact-first: the full constraint is searched with no relaxation.
/// - non-empty → the item's genuine comparables; priced as `CraftTier`, confidence
///   scaling by count (`Confidence::from_count`). A handful of matches is low
///   confidence but still the right set — for a top-tier item the few matching
///   listings ARE the value, and dropping them for a broadened cheap-market sample
///   is exactly what under-priced rare items.
/// - empty → relax, requesting `min_matches = 1` so the `Comparables` impl returns
///   the tightest query that matches anything (for `TradeClient`/`gather_comparables`
///   that means dropping the weakest affixes first, keeping the strongest mods
///   longest). That broader set is priced as `BroadMarket` (low confidence). The
///   estimate may still be empty (`listing_count == 0`) when the base has no live
///   listings at all.
///
/// Returns `(estimate, observations)`. `observations` is the priced-over comparable
/// set, which the caller logs to the corpus.
pub async fn price_check<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    max_relax: usize,
    session: &TradeSession,
) -> Result<(PriceEstimate, Vec<Listing>)> {
    // Exact (no relaxation): the full-constraint matches are the item's true
    // comparables. Price them whenever there is at least one — a thin set is low
    // confidence, but it is the right set. Only fall back to the relaxed broad
    // market when the full constraint matches nothing at all.
    let exact = c.comparables(query, limit, 0, 1, session).await?;
    if !exact.is_empty() {
        let est = estimate_from(&exact, EstimateBasis::CraftTier);
        return Ok((est, exact));
    }
    // Full constraint matched nothing → relax to the tightest non-empty set
    // (`min_matches = 1`; strongest mods kept longest) and price that broader
    // sample, low-confidence.
    let relaxed = c.comparables(query, limit, max_relax, 1, session).await?;
    let est = estimate_from(&relaxed, EstimateBasis::BroadMarket);
    Ok((est, relaxed))
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
    // Drop at least one when trimming, so small samples (n = 8, 9) still shed the
    // floor instead of floor(n*0.10) rounding to zero.
    let priced: &[f64] = if prices.len() >= TRIM_MIN_N {
        let drop = (((prices.len() as f64) * TRIM_BOTTOM_FRAC).floor() as usize).max(1);
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
    // BroadMarket means "no comparable craft-tier bases were found" — never present
    // that as high confidence even if the broad sample is large.
    let confidence = if matches!(basis, EstimateBasis::BroadMarket) {
        Confidence::Low
    } else {
        Confidence::from_count(listings.len())
    };
    // listing_count reports the comparable set size (pre-trim) — trimming is an
    // internal outlier guard, not a change to "how many comps we found".
    PriceEstimate {
        low,
        typical,
        high,
        listing_count: listings.len(),
        confidence,
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
///
/// `max_relax = 3` is used for all estimate sub-calls so the breakdown probes can
/// find enough comparables for delta measurement even when the item is rare.
pub async fn breakdown<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    k: usize,
    session: &TradeSession,
    max_explicit: Option<usize>,
) -> Result<Breakdown> {
    let baseline = estimate(c, query, limit, 3, session, max_explicit).await?;

    // Probe every stat up to the ceiling so ranking is by measured impact.
    let probe_count = query.stats.len().min(PROBE_CEILING);

    let mut ranked: Vec<Contribution> = Vec::new();
    for i in 0..probe_count {
        let sf = &query.stats[i];
        let mut q = query.clone();
        q.stats.remove(i);
        let without = estimate(c, &q, limit, 3, session, max_explicit).await?;
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
                let without_both = estimate(c, &q, limit, 3, session, max_explicit).await?;
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
            id: String::new(),
            base_type: None,
            mods: vec![],
            indexed: None,
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
            id: String::new(),
            base_type: None,
            mods: vec![],
            indexed: None,
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
            min_price_divine: None,
            max_price_divine: None,
        }
    }

    #[tokio::test]
    async fn does_not_relax_when_full_query_matches() {
        // The full 3-stat query already returns a match (FakeApi: 1 listing) → gather
        // must NOT relax. Relaxing would broaden to a cheaper, less-comparable market
        // — the bug that under-priced rare items. It returns the tightest set as-is.
        let api = FakeApi {
            seen: Mutex::new(vec![]),
        };
        let got = gather_comparables(&api, &q_with(3), 5, 3, 1, &TradeSession::for_test())
            .await
            .unwrap();
        assert_eq!(got.len(), 1, "the un-relaxed 3-stat query's single match");
        assert_eq!(
            api.seen.lock().unwrap().len(),
            1,
            "searched once; no relaxation"
        );
    }

    #[tokio::test]
    async fn gather_comparables_drops_stale_listings() {
        // The fetch returns one listing indexed years ago (datable-stale) and one
        // with no timestamp (undatable → kept). gather must drop only the stale one,
        // so old postings can't distort the estimate or satisfy `min_matches`.
        struct AgeApi;
        #[async_trait]
        impl TradeApi for AgeApi {
            async fn search(
                &self,
                _q: &TradeQuery,
                _s: &TradeSession,
            ) -> anyhow::Result<SearchResponse> {
                Ok(SearchResponse {
                    id: "qid".into(),
                    total: 2,
                    hashes: vec!["a".into(), "b".into()],
                })
            }
            async fn fetch(
                &self,
                _id: &str,
                _h: &[String],
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                let mut ancient = listing(10.0);
                ancient.indexed = Some("2000-01-01T00:00:00Z".into());
                let undated = listing(20.0); // indexed: None → kept
                Ok(vec![ancient, undated])
            }
        }
        let got = gather_comparables(&AgeApi, &q_with(0), 10, 0, 1, &TradeSession::for_test())
            .await
            .unwrap();
        assert_eq!(got.len(), 1, "ancient listing dropped, undated kept");
        assert_eq!(got[0].price_divine, 20.0);
    }

    #[tokio::test]
    async fn gather_comparables_fetches_past_stale_prefix() {
        // Search returns 20 price-sorted hashes: the cheapest 10 are stale dregs,
        // the next 10 are fresh. With limit=10, naively fetching the cheapest 10 then
        // dropping stale would yield nothing and wrongly relax/empty. gather must dig
        // deeper into the same result and return the fresh comparables.
        struct DeepApi;
        #[async_trait]
        impl TradeApi for DeepApi {
            async fn search(
                &self,
                _q: &TradeQuery,
                _s: &TradeSession,
            ) -> anyhow::Result<SearchResponse> {
                let mut hashes: Vec<String> = (0..10).map(|i| format!("old{i}")).collect();
                hashes.extend((0..10).map(|i| format!("new{i}")));
                Ok(SearchResponse {
                    id: "qid".into(),
                    total: 20,
                    hashes,
                })
            }
            async fn fetch(
                &self,
                _id: &str,
                h: &[String],
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                Ok(h.iter()
                    .map(|id| {
                        let mut l = listing(10.0);
                        l.id = id.clone();
                        if id.starts_with("old") {
                            l.indexed = Some("2000-01-01T00:00:00Z".into()); // stale
                        }
                        // "new" → indexed None → fresh
                        l
                    })
                    .collect())
            }
        }
        let got = gather_comparables(&DeepApi, &q_with(0), 10, 0, 1, &TradeSession::for_test())
            .await
            .unwrap();
        assert_eq!(
            got.len(),
            10,
            "fresh comparables found past the stale cheap prefix"
        );
        assert!(got.iter().all(|l| l.id.starts_with("new")));
    }

    #[tokio::test]
    async fn relaxes_past_empty_until_first_match() {
        // The tight query matches nothing; relaxation drops the weakest filters until
        // the first query that matches, and STOPS there (does not over-relax to the
        // bare base). `EmptyUntilOne` returns nothing while >1 stat remains.
        struct EmptyUntilOne {
            seen: Mutex<Vec<TradeQuery>>,
        }
        #[async_trait]
        impl TradeApi for EmptyUntilOne {
            async fn search(
                &self,
                q: &TradeQuery,
                _s: &TradeSession,
            ) -> anyhow::Result<SearchResponse> {
                self.seen.lock().unwrap().push(q.clone());
                let n = if q.stats.len() > 1 { 0 } else { 3 };
                Ok(SearchResponse {
                    id: "qid".into(),
                    total: n as u64,
                    hashes: (0..n).map(|i| format!("h{i}")).collect(),
                })
            }
            async fn fetch(
                &self,
                _id: &str,
                hashes: &[String],
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                Ok(hashes.iter().map(|_| listing(10.0)).collect())
            }
        }
        let api = EmptyUntilOne {
            seen: Mutex::new(vec![]),
        };
        // 3 stats (empty) → 2 stats (empty) → 1 stat (3 matches) → stop.
        let got = gather_comparables(&api, &q_with(3), 5, 5, 1, &TradeSession::for_test())
            .await
            .unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(
            api.seen.lock().unwrap().len(),
            3,
            "relaxed twice to the first non-empty query, then stopped"
        );
    }

    #[tokio::test]
    async fn breakdown_path_relaxes_past_thin_result_to_reach_min_matches() {
        // With min_matches > 1 (the breakdown/estimate path), a thin non-empty
        // result is NOT enough: gather keeps relaxing until it has a real sample,
        // so the craftability filter downstream has something to work with. FakeApi
        // returns 1 listing at 3 stats, growing as stats drop.
        let api = FakeApi {
            seen: Mutex::new(vec![]),
        };
        let got = gather_comparables(
            &api,
            &q_with(3),
            100,
            3,
            MIN_COMPARABLES,
            &TradeSession::for_test(),
        )
        .await
        .unwrap();
        assert!(
            got.len() >= MIN_COMPARABLES,
            "relaxed to reach a real sample, got {}",
            got.len()
        );
        assert!(
            api.seen.lock().unwrap().len() > 1,
            "relaxed past the thin first result"
        );
    }

    #[tokio::test]
    async fn does_not_relax_when_result_meets_minimum() {
        // A tight 3-stat query already returning >= MIN_COMPARABLES must NOT be
        // relaxed (relaxing would price from a broadened, non-matching market).
        struct FatApi {
            seen: Mutex<Vec<TradeQuery>>,
        }
        #[async_trait]
        impl TradeApi for FatApi {
            async fn search(
                &self,
                q: &TradeQuery,
                _s: &TradeSession,
            ) -> anyhow::Result<SearchResponse> {
                self.seen.lock().unwrap().push(q.clone());
                let hashes = (0..12).map(|i| format!("h{i}")).collect::<Vec<_>>();
                Ok(SearchResponse {
                    id: "qid".into(),
                    total: 12,
                    hashes,
                })
            }
            async fn fetch(
                &self,
                _id: &str,
                hashes: &[String],
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                Ok(hashes
                    .iter()
                    .enumerate()
                    .map(|(i, _)| listing(10.0 + i as f64))
                    .collect())
            }
        }
        let api = FatApi {
            seen: Mutex::new(vec![]),
        };
        let got = gather_comparables(
            &api,
            &q_with(3),
            100,
            3,
            MIN_COMPARABLES,
            &TradeSession::for_test(),
        )
        .await
        .unwrap();
        assert_eq!(got.len(), 12);
        assert_eq!(
            api.seen.lock().unwrap().len(),
            1,
            "must not relax — one search only"
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
            _max_relax: usize,
            _min_matches: usize,
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
            min_price_divine: None,
            max_price_divine: None,
        }
    }

    #[tokio::test]
    async fn estimate_reports_typical_and_confidence() {
        let est = estimate(
            &FakePricer,
            &two_stat_query(),
            10,
            0,
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
            _max_relax: usize,
            _min_matches: usize,
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
            _max_relax: usize,
            _min_matches: usize,
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
            0,
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
            0,
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
        let est = estimate(
            &c,
            &two_stat_query(),
            30,
            0,
            &TradeSession::for_test(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(est.basis, EstimateBasis::AffixesOnly);
    }

    #[test]
    fn trim_drops_at_least_one_at_min_n() {
        // n = 8 (== TRIM_MIN_N): floor(8*0.10)=0, but we must still drop ≥1.
        let ls: Vec<Listing> = (1..=8).map(|i| listing_ec(i as f64, 4)).collect();
        let est = estimate_from(&ls, EstimateBasis::CraftTier);
        assert_eq!(est.listing_count, 8); // pre-trim count
        assert!((est.typical - 5.0).abs() < 0.001); // median of [2..=8] (cheapest dropped)
    }

    #[test]
    fn craftability_filter_excludes_unknown_zero() {
        let ls = vec![listing_ec(2.0, 0), listing_ec(2.0, 4), listing_ec(2.0, 6)];
        let kept = craftability_filter(&ls, 4);
        assert_eq!(kept.len(), 1); // ec=0 (unknown) and ec=6 (more-filled) both excluded
        assert_eq!(kept[0].explicit_count, 4);
    }

    #[tokio::test]
    async fn estimate_all_unknown_falls_back_to_broad_market() {
        // explicitMods missing on every listing → all ec=0 → none survive the
        // craft-tier filter → BroadMarket (not a CraftTier estimate over the floor).
        let c = FixedListings(vec![listing_ec(0.05, 0), listing_ec(0.04, 0)]);
        let est = estimate(
            &c,
            &two_stat_query(),
            30,
            0,
            &TradeSession::for_test(),
            Some(4),
        )
        .await
        .unwrap();
        assert_eq!(est.basis, EstimateBasis::BroadMarket);
    }

    #[test]
    fn broad_market_forces_low_confidence() {
        // 12 listings would be High via from_count, but BroadMarket must read Low.
        let ls: Vec<Listing> = (1..=12).map(|i| listing_ec(i as f64, 6)).collect();
        let est = estimate_from(&ls, EstimateBasis::BroadMarket);
        assert_eq!(est.confidence, Confidence::Low);
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
            min_price_divine: None,
            max_price_divine: None,
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

    #[tokio::test]
    async fn price_check_relaxed_result_is_broad_market_low_confidence() {
        // Exact (max_relax=0) matches nothing; relaxing yields a set (12). The
        // relaxed result must NOT be presented as a precise CraftTier match — it is
        // BroadMarket with Low confidence (the fallback when the full query is empty).
        struct Relaxer;
        #[async_trait]
        impl Comparables for Relaxer {
            async fn comparables(
                &self,
                _q: &TradeQuery,
                _l: usize,
                max_relax: usize,
                _min_matches: usize,
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                let n = if max_relax > 0 { 12 } else { 0 };
                Ok((0..n).map(|i| listing(2.0 + i as f64)).collect())
            }
        }
        let q = two_stat_query();
        let (est, _listings) =
            price_check(&Relaxer, &q, 40, q.stats.len(), &TradeSession::for_test())
                .await
                .unwrap();
        assert_eq!(est.basis, EstimateBasis::BroadMarket);
        assert_eq!(est.confidence, Confidence::Low);
        assert!(est.low <= est.typical && est.typical <= est.high);
    }

    #[tokio::test]
    async fn price_check_exact_match_is_craft_tier() {
        // The full constraint already yields >= MIN_COMPARABLES → precise set,
        // priced as CraftTier (count-based confidence), no relaxation needed.
        struct Plenty;
        #[async_trait]
        impl Comparables for Plenty {
            async fn comparables(
                &self,
                _q: &TradeQuery,
                _l: usize,
                _max_relax: usize,
                _min_matches: usize,
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                Ok((0..12).map(|i| listing(2.0 + i as f64)).collect())
            }
        }
        let q = two_stat_query();
        let (est, _listings) =
            price_check(&Plenty, &q, 40, q.stats.len(), &TradeSession::for_test())
                .await
                .unwrap();
        assert_eq!(est.basis, EstimateBasis::CraftTier);
    }

    #[tokio::test]
    async fn price_check_prices_thin_exact_not_relaxed_floor() {
        // Exact (max_relax=0) returns 2 expensive matches (50, 51 div); the relaxed
        // path would return a cheap floor. The estimate must be the exact matches
        // (CraftTier, ~50 div) — NOT the relaxed floor — and only those are logged.
        fn lst(divine: f64, id: &str) -> Listing {
            Listing {
                price: Money {
                    amount: divine,
                    currency: Currency::Divine,
                },
                price_divine: divine,
                explicit_count: 1,
                id: id.to_string(),
                base_type: None,
                mods: vec![],
                indexed: None,
            }
        }
        struct ExactThin;
        #[async_trait]
        impl Comparables for ExactThin {
            async fn comparables(
                &self,
                _q: &TradeQuery,
                _l: usize,
                max_relax: usize,
                _min_matches: usize,
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                if max_relax == 0 {
                    Ok((0..2)
                        .map(|i| lst(50.0 + i as f64, &format!("exact-{i}")))
                        .collect())
                } else {
                    Ok((0..12)
                        .map(|i| lst(2.0 + i as f64, &format!("relaxed-{i}")))
                        .collect())
                }
            }
        }
        let q = two_stat_query();
        let (est, to_log) =
            price_check(&ExactThin, &q, 40, q.stats.len(), &TradeSession::for_test())
                .await
                .unwrap();
        assert_eq!(est.basis, EstimateBasis::CraftTier); // priced on the exact set
        assert!(est.typical >= 50.0, "got typical={}", est.typical);
        assert_eq!(to_log.len(), 2); // only the exact matches logged
        assert!(to_log.iter().all(|l| l.id.starts_with("exact")));
    }

    #[tokio::test]
    async fn price_check_thin_expensive_exact_is_priced_not_floored() {
        // Regression for the top-tier-staff bug: a rare item whose full constraint
        // matches a FEW expensive listings must be priced off THOSE matches, not
        // off a broadened cheap-market set. Exact (max_relax=0) → 3 expensive
        // comparables; relaxed → a cheap floor. The estimate must reflect the
        // expensive exact matches.
        struct ThinExpensive;
        #[async_trait]
        impl Comparables for ThinExpensive {
            async fn comparables(
                &self,
                _q: &TradeQuery,
                _l: usize,
                max_relax: usize,
                _min_matches: usize,
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                if max_relax == 0 {
                    Ok(vec![listing(80.0), listing(150.0), listing(210.0)])
                } else {
                    Ok((0..40).map(|_| listing(0.1)).collect())
                }
            }
        }
        let q = two_stat_query();
        let (est, to_log) = price_check(
            &ThinExpensive,
            &q,
            40,
            q.stats.len(),
            &TradeSession::for_test(),
        )
        .await
        .unwrap();
        assert!(
            est.typical >= 50.0,
            "thin expensive exact matches must drive the price, got typical={}",
            est.typical
        );
        assert_eq!(est.basis, EstimateBasis::CraftTier); // the exact constrained set
        assert_eq!(to_log.len(), 3); // logged the expensive comparables, not the floor
    }
}
