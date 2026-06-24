//! On-demand rare-item pricing via live trade2 ablation. Isolated from
//! `poeninja`/`store`: data flows discord → trade, never sideways.

pub mod ablation;
pub mod age;
pub mod categories;
pub mod client;
pub mod limiter;
pub mod model;
pub mod pseudo;
pub mod quality;
pub mod query;
pub mod rates;
pub mod session;
pub mod stats;
pub mod value;

use anyhow::Result;

use crate::itemtext::ParsedItem;
use crate::observe::{Observation, ObservationLog, Source};
use crate::trade::ablation::{price_check, Comparables};
use crate::trade::model::{Breakdown, Listing, PriceEstimate};
use crate::trade::pseudo::PseudoMap;
use crate::trade::query::build_baseline;
use crate::trade::session::TradeSession;
use crate::trade::stats::StatCatalog;

/// Number of cheapest listings to fetch per query before craftability filtering.
/// Set to the practical search-result cap so the whole constrained result is
/// considered and craft-tier comparables in the tail aren't crowded out by the
/// junk floor. `gather_comparables` fetches `min(result, limit)`, so smaller
/// result sets cost no more; only the BroadMarket fallback (no craft-tier base in
/// the whole result) prices broadly, and that path is already low-confidence + labelled.
const COMPARABLE_SAMPLE: usize = 100;
/// Cheapest matches fetched for the price-check percentile read. Smaller than the
/// breakdown's COMPARABLE_SAMPLE: p20/p50/p80 over the cheapest ~40 is stable and
/// keeps the relax-and-read latency low (≤4 fetch batches per relaxation step).
const PRICE_SAMPLE: usize = 40;
/// Number of characteristics to ablate in a breakdown.
const TOP_K: usize = 4;
/// Price-band edges (Divine) for a harvest sweep. Consecutive edges define
/// disjoint bands `[lo, hi)` (the last open-ended), each searched with both a
/// min and max price so the band is a true slice of the market. Within a band,
/// listings are sampled evenly across the price-sorted results (`stride_sample`)
/// rather than taking the cheapest prefix — otherwise the sweep just re-collects
/// the global cheap end and the corpus stays blind to the roll/tier value curves
/// that distinguish a 50-div staff from a 500-div one.
const PRICE_BANDS: [f64; 7] = [0.0, 5.0, 20.0, 50.0, 100.0, 200.0, 500.0];
/// Listings sampled per band (evenly spaced across the band's price range).
const HARVEST_SAMPLE: usize = 100;
/// Adaptive sub-banding only kicks in at/above this price (Divine). Below it the
/// market is cheap junk pinned to the round-number floor — not worth extra queries
/// to resolve. The value-bearing mid-tier (≈20–200 div) and up gets bisected so the
/// trade2 100-result cap doesn't hide everything above each band's floor.
const SUBBAND_VALUE_FLOOR: f64 = 20.0;
/// Max recursive bisections of one band. Bounds harvest cost (a band expands to at
/// most 2^depth leaves) and the finest price resolution (band_width / 2^depth).
const MAX_SUBBAND_DEPTH: usize = 3;

/// Picks up to `n` evenly-spaced items from `items` (which the trade2 search
/// returns sorted by price ascending). Returns all items when `items.len() <= n`.
/// Striding — rather than taking the cheapest `n` prefix — keeps the sample
/// representative across the whole price band instead of skewed to its floor.
fn stride_sample<T: Clone>(items: &[T], n: usize) -> Vec<T> {
    if n == 0 {
        return Vec::new();
    }
    if items.len() <= n {
        return items.to_vec();
    }
    if n == 1 {
        return vec![items[0].clone()];
    }
    // Evenly spaced indices spanning [0, len-1] inclusive, so both the cheap floor
    // (k=0) and the expensive top of the band (k=n-1 → last item) are represented.
    let last = items.len() - 1;
    (0..n)
        .map(|k| items[(k * last + (n - 1) / 2) / (n - 1)].clone())
        .collect()
}

pub struct TradePricer<C: Comparables> {
    comparables: C,
    pseudo: PseudoMap,
    catalog: StatCatalog,
    log: ObservationLog,
    value: std::sync::Arc<std::sync::RwLock<crate::trade::value::ValueModel>>,
}

impl<C: Comparables> TradePricer<C> {
    /// Read access to the stat catalog (for /insights label resolution).
    pub fn catalog(&self) -> &StatCatalog {
        &self.catalog
    }
    pub fn new(
        comparables: C,
        pseudo: PseudoMap,
        catalog: StatCatalog,
        log: ObservationLog,
        value: std::sync::Arc<std::sync::RwLock<crate::trade::value::ValueModel>>,
    ) -> Self {
        TradePricer {
            comparables,
            pseudo,
            catalog,
            log,
            value,
        }
    }

    pub async fn price(
        &self,
        item: &ParsedItem,
        league: &str,
        session: &TradeSession,
    ) -> Result<PriceEstimate> {
        let query = {
            let model = self.value.read().unwrap_or_else(|e| e.into_inner());
            build_baseline(item, &self.pseudo, &self.catalog, &model, league)
        };
        // Relax up to the number of stat + equipment filters so the query can
        // broaden all the way to the bare base if needed (gather_comparables drops
        // stat filters first, then equipment bands); build_baseline ordered the
        // stats weakest-last so relaxation drops the weakest affix first and
        // cornerstones last.
        let max_relax = query.stats.len() + query.equipment.len();
        let (est, listings) =
            price_check(&self.comparables, &query, PRICE_SAMPLE, max_relax, session).await?;
        self.log_observations(item, league, &listings);
        Ok(est)
    }

    pub async fn breakdown(
        &self,
        item: &ParsedItem,
        league: &str,
        session: &TradeSession,
    ) -> Result<Breakdown> {
        let query = {
            let model = self.value.read().unwrap_or_else(|e| e.into_inner());
            build_baseline(item, &self.pseudo, &self.catalog, &model, league)
        };
        let max_explicit = item.craftability().map(|c| c.explicit_count as usize);
        let bd = crate::trade::ablation::breakdown(
            &self.comparables,
            &query,
            COMPARABLE_SAMPLE,
            TOP_K,
            session,
            max_explicit,
        )
        .await?;
        Ok(bd)
    }

    /// Return a k-NN value estimate from the learned `ValueModel` for `item` in
    /// `league`, or `None` when:
    /// - `item.item_class` is absent,
    /// - the category has no model for this league, or
    /// - the category is not trusted (`sample_size < TRUST_MIN_SAMPLE`, or no positive skill over the category-median baseline).
    ///
    /// The method is intentionally synchronous — it only reads the in-memory
    /// model; no I/O is performed.
    pub fn learned_estimate(
        &self,
        item: &ParsedItem,
        league: &str,
    ) -> Option<crate::trade::value::estimate::ValueEstimate> {
        // 1. Canonical category from item class.
        let canon = crate::trade::value::canonical_category(item.item_class.as_deref()?);

        // 2. Poison-safe model read.
        let model = self.value.read().unwrap_or_else(|e| e.into_inner());

        // 3. Category lookup.
        let cat = model.category(league, &canon)?;

        // 4. Trust bar: enough samples AND positive skill over the no-feature baseline.
        if !cat.is_trusted() {
            return None;
        }

        // 5. Resolve explicit mods to (stat_id, raw_roll).
        // INVARIANT: the corpus stores EXPLICIT mods only (`ListingMod` is "one explicit
        // mod on a fetched listing"), so the query must be built from explicits only —
        // the k-NN similarity compares mod-sets, and mixing in implicits/runes here would
        // desync the query's mod-set from the corpus's and skew Jaccard. If a future
        // harvest ever captures implicits into `Observation.mods`, revisit this resolve.
        let resolved: Vec<(String, Option<f64>)> = item
            .explicits
            .iter()
            .filter_map(|m| {
                let id = self
                    .catalog
                    .match_stat(&m.raw, crate::trade::stats::StatGroup::Explicit)?;
                Some((id, m.value))
            })
            .collect();

        // 6. Build normalised query and estimate.
        let query = cat.query_from_stats(&resolved);
        cat.estimate(&query)
    }

    /// Append one `Observation { source: Paste }` per fetched comparable. Best-
    /// effort: a write failure is logged, never fatal.
    fn log_observations(&self, item: &ParsedItem, league: &str, listings: &[Listing]) {
        let timestamp_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        for l in listings {
            // Skip listings whose mods we couldn't capture — an observation with no
            // mods can't inform the value model (it's noise in the corpus).
            if l.mods.is_empty() {
                continue;
            }
            let obs = Observation {
                timestamp_unix,
                league: league.to_string(),
                base_type: l.base_type.clone().or_else(|| item.base_type.clone()),
                category: item
                    .item_class
                    .as_deref()
                    .map(crate::trade::value::canonical_category),
                mods: l.mods.clone(),
                price_divine: l.price_divine,
                source: Source::Paste,
                indexed: l.indexed.clone(),
            };
            if let Err(e) = self.log.append(&obs) {
                tracing::warn!(error = %e, "failed to append observation");
            }
        }
    }
}

impl<C: Comparables + crate::trade::client::TradeApi> TradePricer<C> {
    /// Core adaptive price-band sweep. `base_query` is a `TradeQuery` template
    /// carrying the category (and optionally a stats filter); each band query is
    /// built by cloning the template and setting `min_price_divine`/`max_price_divine`.
    /// Returns the number of observations logged in this sweep.
    async fn harvest_sweep(
        &self,
        base_query: &crate::trade::model::TradeQuery,
        category_text: &str,
        session: &TradeSession,
    ) -> Result<usize> {
        let mut logged = 0usize;
        let timestamp_unix = crate::trade::age::now_unix();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Adaptive price-band sweep. trade2 `/search` caps its result list at ~100
        // ids per query with no pagination, so a band with more matches than that
        // exposes only its cheapest 100 — which in the dense low/mid range are all
        // pinned to the round-number floor. When a band reports more matches than it
        // returns (incomplete) and sits in the value range, bisect it at the geometric
        // midpoint and recover each half, until every fetched leaf is fully covered or
        // the depth cap is hit. Cheap (< SUBBAND_VALUE_FLOOR) and the open-ended top
        // band are harvested once. Work stack of (lo, hi, depth) seeded from
        // PRICE_BANDS; dedup by listing id so an item on a split boundary logs once.
        let mut stack: Vec<(f64, Option<f64>, usize)> = PRICE_BANDS
            .iter()
            .enumerate()
            .map(|(i, &lo)| (lo, PRICE_BANDS.get(i + 1).copied(), 0usize))
            .collect();
        while let Some((lo, hi, depth)) = stack.pop() {
            let q = crate::trade::model::TradeQuery {
                min_price_divine: if lo > 0.0 { Some(lo) } else { None },
                max_price_divine: hi,
                ..base_query.clone()
            };
            let resp = match self.comparables.search(&q, session).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, band_lo = lo, band_hi = ?hi, "harvest band search failed; skipping");
                    continue;
                }
            };
            // The API returned fewer ids than matched → this band is under-covered;
            // bisect (within the value range, up to the depth cap) rather than settle
            // for its cheapest 100.
            if let Some(hi_v) = hi {
                if resp.total > resp.hashes.len() as u64
                    && lo >= SUBBAND_VALUE_FLOOR
                    && depth < MAX_SUBBAND_DEPTH
                {
                    let mid = (lo * hi_v).sqrt(); // geometric: finer where prices cluster
                    if mid > lo && mid < hi_v {
                        tracing::info!(
                            band_lo = lo,
                            band_hi = hi_v,
                            total = resp.total,
                            depth,
                            "harvest band split"
                        );
                        stack.push((mid, hi, depth + 1));
                        stack.push((lo, Some(mid), depth + 1));
                        continue;
                    }
                }
            }
            let sampled = stride_sample(&resp.hashes, HARVEST_SAMPLE);
            tracing::info!(
                band_lo = lo,
                band_hi = ?hi,
                total = resp.total,
                returned = resp.hashes.len(),
                sampled = sampled.len(),
                depth,
                "harvest band"
            );
            let listings = match self.comparables.fetch(&resp.id, &sampled, session).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(error = %e, band_lo = lo, band_hi = ?hi, "harvest band fetch failed; skipping");
                    continue;
                }
            };
            for l in &listings {
                if !l.id.is_empty() && !seen.insert(l.id.clone()) {
                    continue; // already logged this listing from an earlier band
                }
                // Skip listings with no captured mods — they can't inform learning.
                if l.mods.is_empty() {
                    continue;
                }
                let obs = Observation {
                    timestamp_unix,
                    league: base_query.league.clone(),
                    base_type: l.base_type.clone(),
                    category: Some(category_text.to_string()),
                    mods: l.mods.clone(),
                    price_divine: l.price_divine,
                    source: Source::Harvest,
                    indexed: l.indexed.clone(),
                };
                if self.log.append(&obs).is_ok() {
                    logged += 1;
                } else {
                    tracing::warn!("failed to append harvest observation");
                }
            }
        }
        Ok(logged)
    }

    /// Price-band sweep of a whole category, logging every listing to the corpus
    /// as a Harvest observation. Returns the number of observations logged. Each
    /// search/fetch is throttle-paced by the member session; a per-band failure is
    /// logged and skipped so one bad band doesn't abort the whole harvest.
    pub async fn harvest(
        &self,
        category_id: &str,
        category_text: &str,
        league: &str,
        session: &TradeSession,
    ) -> Result<usize> {
        // Disjoint, bounded price bands `[lo, hi)` (last open-ended). Bounding each
        // band by max — and sampling evenly across the price-sorted results rather
        // than taking the cheapest prefix — keeps each band's sample representative
        // across its whole range instead of repeatedly oversampling the global cheap
        // end. Dedup by listing id across bands so a boundary item appearing in two
        // adjacent bands is logged once; empty ids can't be deduped, so are always
        // logged.
        let base_query = crate::trade::model::TradeQuery {
            league: league.to_string(),
            category: Some(category_id.to_string()),
            type_line: None,
            stats: vec![],
            misc: crate::trade::model::MiscFilters::default(),
            equipment: vec![],
            min_price_divine: None,
            max_price_divine: None,
        };
        self.harvest_sweep(&base_query, category_text, session)
            .await
    }

    /// Targeted price-band sweep for items carrying a specific mod (`stat_id`).
    /// Every band query carries a presence `StatFilter` (the stat id, no min/max)
    /// so every fetched listing has the gate mod. Uses the same adaptive bisecting
    /// sweep as `harvest`; one mod at a time to stay polite to the rate limiter.
    pub async fn harvest_mod(
        &self,
        category_id: &str,
        category_text: &str,
        league: &str,
        stat_id: &str,
        session: &TradeSession,
    ) -> Result<usize> {
        let base_query = crate::trade::model::TradeQuery {
            league: league.to_string(),
            category: Some(category_id.to_string()),
            type_line: None,
            stats: vec![crate::trade::model::StatFilter {
                id: stat_id.to_string(),
                label: String::new(),
                min: None,
                max: None,
            }],
            misc: crate::trade::model::MiscFilters::default(),
            equipment: vec![],
            min_price_divine: None,
            max_price_divine: None,
        };
        self.harvest_sweep(&base_query, category_text, session)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::itemtext::{ItemStat, ParsedItem, Rarity};
    use crate::trade::ablation::Comparables;
    use crate::trade::model::{Currency, EstimateBasis, Listing, Money, TradeQuery};
    use crate::trade::session::TradeSession;
    use async_trait::async_trait;

    struct Flat(f64);
    #[async_trait]
    impl Comparables for Flat {
        async fn comparables(
            &self,
            _q: &TradeQuery,
            _l: usize,
            _max_relax: usize,
            _min_matches: usize,
            _session: &TradeSession,
        ) -> anyhow::Result<Vec<Listing>> {
            // 8 listings with distinct, stable ids so the exact∪relaxed union in
            // price_check dedups deterministically to 8 (not 16).
            Ok((0..8)
                .map(|i| Listing {
                    price: Money {
                        amount: self.0,
                        currency: Currency::Divine,
                    },
                    price_divine: self.0,
                    explicit_count: 0,
                    id: format!("flat-{i}"),
                    base_type: None,
                    mods: vec![crate::trade::model::ListingMod {
                        stat_id: "explicit.stat_x".into(),
                        tier: None,
                        roll: None,
                    }],
                    indexed: None,
                })
                .collect())
        }
    }

    fn ring() -> ParsedItem {
        ParsedItem {
            rarity: Rarity::Rare,
            name: "Woe Coil".into(),
            base_type: Some("Sapphire Ring".into()),
            item_class: Some("Rings".into()),
            item_level: Some(80),
            quality: None,
            corrupted: false,
            energy_shield: None,
            armour: None,
            evasion: None,
            implicits: vec![],
            enchants: vec![],
            runes: vec![],
            explicits: vec![ItemStat {
                raw: "+40 to maximum Life".into(),
                value: Some(40.0),
                affix: None,
                tier: None,
            }],
        }
    }

    fn make_listing(divine: f64, ec: usize, id: &str) -> Listing {
        Listing {
            price: Money {
                amount: divine,
                currency: Currency::Divine,
            },
            price_divine: divine,
            explicit_count: ec,
            id: id.to_string(),
            base_type: None,
            mods: vec![crate::trade::model::ListingMod {
                stat_id: "explicit.stat_x".into(),
                tier: None,
                roll: None,
            }],
            indexed: None,
        }
    }

    // Returns the TempDir alongside the pricer so the caller keeps it alive —
    // otherwise the dir is deleted at return and the ObservationLog appends fail
    // (silently, since logging is best-effort), masking regressions.
    fn make_pricer<C: Comparables>(c: C) -> (TradePricer<C>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let pricer = TradePricer::new(
            c,
            crate::trade::pseudo::PseudoMap::load(),
            crate::trade::stats::StatCatalog::default(),
            crate::observe::ObservationLog::new(dir.path().join("obs.jsonl")),
            std::sync::Arc::new(std::sync::RwLock::new(
                crate::trade::value::ValueModel::default(),
            )),
        );
        (pricer, dir)
    }

    #[tokio::test]
    async fn price_logs_observations_and_returns_estimate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obs.jsonl");
        let log = crate::observe::ObservationLog::new(&path);
        let pricer = TradePricer::new(
            Flat(12.0),
            crate::trade::pseudo::PseudoMap::load(),
            crate::trade::stats::StatCatalog::default(),
            log,
            std::sync::Arc::new(std::sync::RwLock::new(
                crate::trade::value::ValueModel::default(),
            )),
        );
        let est = pricer
            .price(&ring(), "Standard", &TradeSession::for_test())
            .await
            .unwrap();
        assert_eq!(est.typical, 12.0);
        let contents = std::fs::read_to_string(&path).unwrap();
        // Flat returns 8 listings; all 8 should be logged
        assert_eq!(contents.lines().count(), 8);
    }

    #[tokio::test]
    async fn price_reads_percentiles_over_comparables_no_progress_arg() {
        // 12 comparables 1.0..12.0 div; price() reads p20/p50/p80 over them.
        struct Comps;
        #[async_trait]
        impl Comparables for Comps {
            async fn comparables(
                &self,
                _q: &TradeQuery,
                _l: usize,
                _mr: usize,
                _min_matches: usize,
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                Ok((1..=12)
                    .map(|i| make_listing(i as f64, 1, &format!("c{i}")))
                    .collect())
            }
        }
        let (pricer, _dir) = make_pricer(Comps);
        let est = pricer
            .price(&ring(), "Standard", &TradeSession::for_test())
            .await
            .unwrap();
        assert!(est.typical > 0.0 && est.typical <= 12.0);
        assert!(est.low <= est.typical && est.typical <= est.high);
        assert_eq!(est.listing_count, 12);
        assert_eq!(est.basis, EstimateBasis::CraftTier); // >= MIN_COMPARABLES found
    }

    #[tokio::test]
    async fn price_logs_one_observation_per_comparable() {
        struct Comps;
        #[async_trait]
        impl Comparables for Comps {
            async fn comparables(
                &self,
                _q: &TradeQuery,
                _l: usize,
                _mr: usize,
                _min_matches: usize,
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                Ok((1..=5)
                    .map(|i| make_listing(i as f64, 1, &format!("c{i}")))
                    .collect())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obs.jsonl");
        let pricer = TradePricer::new(
            Comps,
            crate::trade::pseudo::PseudoMap::load(),
            crate::trade::stats::StatCatalog::default(),
            crate::observe::ObservationLog::new(&path),
            std::sync::Arc::new(std::sync::RwLock::new(
                crate::trade::value::ValueModel::default(),
            )),
        );
        let est = pricer
            .price(&ring(), "Standard", &TradeSession::for_test())
            .await
            .unwrap();
        assert!(est.typical > 0.0);
        let lines = std::fs::read_to_string(&path).unwrap();
        assert_eq!(lines.lines().count(), 5); // one observation per comparable
        assert!(lines.contains("\"source\":\"paste\""));
        assert!(lines.contains("Sapphire Ring")); // base_type from the parsed item
    }

    #[tokio::test]
    async fn harvest_logs_one_observation_per_band_listing() {
        use crate::trade::client::TradeApi;
        use crate::trade::model::SearchResponse;

        struct HarvestFake;
        #[async_trait]
        impl Comparables for HarvestFake {
            async fn comparables(
                &self,
                _q: &TradeQuery,
                _l: usize,
                _mr: usize,
                _min_matches: usize,
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                Ok(vec![])
            }
        }
        #[async_trait]
        impl TradeApi for HarvestFake {
            async fn search(
                &self,
                q: &TradeQuery,
                _s: &TradeSession,
            ) -> anyhow::Result<SearchResponse> {
                let band = q.min_price_divine.unwrap_or(0.0);
                Ok(SearchResponse {
                    id: format!("q{band}"),
                    total: 1,
                    hashes: vec![format!("h{band}")],
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
                    .map(|h| Listing {
                        price: Money {
                            amount: 1.0,
                            currency: Currency::Divine,
                        },
                        price_divine: 1.0,
                        explicit_count: 1,
                        id: h.clone(),
                        base_type: Some("Chiming Staff".into()),
                        mods: vec![crate::trade::model::ListingMod {
                            stat_id: "explicit.stat_1".into(),
                            tier: Some(1),
                            roll: Some(50.0),
                        }],
                        indexed: None,
                    })
                    .collect())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obs.jsonl");
        let pricer = TradePricer::new(
            HarvestFake,
            crate::trade::pseudo::PseudoMap::load(),
            crate::trade::stats::StatCatalog::default(),
            crate::observe::ObservationLog::new(&path),
            std::sync::Arc::new(std::sync::RwLock::new(
                crate::trade::value::ValueModel::default(),
            )),
        );
        let n = pricer
            .harvest(
                "weapon.staff",
                "Staff",
                "Standard",
                &TradeSession::for_test(),
            )
            .await
            .unwrap();
        assert_eq!(n, PRICE_BANDS.len()); // one listing per band
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), PRICE_BANDS.len());
        assert!(body.contains("\"source\":\"harvest\""));
        assert!(body.contains("\"category\":\"Staff\""));
        assert!(body.contains("Chiming Staff"));
    }

    #[tokio::test]
    async fn harvest_dedupes_listings_shared_across_bands() {
        use crate::trade::client::TradeApi;
        use crate::trade::model::SearchResponse;

        // Every band's search returns one shared listing id plus one band-unique
        // one — simulating sparse categories where the lower min-only band
        // re-fetches higher-band items. The shared id must be logged once.
        struct OverlapFake;
        #[async_trait]
        impl Comparables for OverlapFake {
            async fn comparables(
                &self,
                _q: &TradeQuery,
                _l: usize,
                _mr: usize,
                _min_matches: usize,
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                Ok(vec![])
            }
        }
        #[async_trait]
        impl TradeApi for OverlapFake {
            async fn search(
                &self,
                q: &TradeQuery,
                _s: &TradeSession,
            ) -> anyhow::Result<SearchResponse> {
                let band = q.min_price_divine.unwrap_or(0.0);
                Ok(SearchResponse {
                    id: format!("q{band}"),
                    total: 2,
                    hashes: vec!["shared".into(), format!("u{band}")],
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
                    .map(|h| Listing {
                        price: Money {
                            amount: 1.0,
                            currency: Currency::Divine,
                        },
                        price_divine: 1.0,
                        explicit_count: 1,
                        id: h.clone(),
                        base_type: Some("Chiming Staff".into()),
                        mods: vec![crate::trade::model::ListingMod {
                            stat_id: "explicit.stat_x".into(),
                            tier: None,
                            roll: None,
                        }],
                        indexed: None,
                    })
                    .collect())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obs.jsonl");
        let pricer = TradePricer::new(
            OverlapFake,
            crate::trade::pseudo::PseudoMap::load(),
            crate::trade::stats::StatCatalog::default(),
            crate::observe::ObservationLog::new(&path),
            std::sync::Arc::new(std::sync::RwLock::new(
                crate::trade::value::ValueModel::default(),
            )),
        );
        let n = pricer
            .harvest(
                "weapon.staff",
                "Staff",
                "Standard",
                &TradeSession::for_test(),
            )
            .await
            .unwrap();
        // 1 shared (logged once) + one unique per band.
        assert_eq!(n, 1 + PRICE_BANDS.len());
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 1 + PRICE_BANDS.len());
    }

    #[test]
    fn stride_sample_returns_all_when_not_larger_than_n() {
        let v = vec![1, 2, 3];
        assert_eq!(stride_sample(&v, 5), vec![1, 2, 3]);
        assert_eq!(stride_sample(&v, 3), vec![1, 2, 3]);
    }

    #[test]
    fn stride_sample_spreads_evenly_across_range() {
        let v: Vec<usize> = (0..100).collect();
        let s = stride_sample(&v, 10);
        assert_eq!(s, vec![0, 11, 22, 33, 44, 55, 66, 77, 88, 99]);
        // spans both ends: the cheap floor AND the most expensive item in the band
        assert_eq!(s[0], 0);
        assert_eq!(*s.last().unwrap(), 99);
    }

    #[test]
    fn stride_sample_zero_is_empty() {
        assert!(stride_sample::<i32>(&[1, 2, 3], 0).is_empty());
    }

    // ──────────────────────────────────────────────────────────────────────────
    // learned_estimate helpers and tests
    // ──────────────────────────────────────────────────────────────────────────

    /// Build a `ValueModel` with ≥`TRUST_MIN_SAMPLE` observations for the "Staff"
    /// category (item class "Staves") in league "Standard". Items carry `stat_id`
    /// with a roll that varies linearly from 0 to 1 across the corpus, and price
    /// is set to `price_divine * (1 + roll)` so it tracks the roll. Each item
    /// also carries a unique tag mod so every mod-set is distinct (self-exclusion
    /// removes only the probe itself, not a whole group). The roll-price correlation
    /// gives the k-NN genuine signal to beat the category-median baseline, producing
    /// skill > 0 and making `is_trusted()` return true.
    fn trusted_staff_model(stat_id: &str, price_divine: f64) -> crate::trade::value::ValueModel {
        use crate::observe::{Observation, Source};
        use crate::trade::model::ListingMod;
        use crate::trade::value::TRUST_MIN_SAMPLE;

        let n = TRUST_MIN_SAMPLE + 10; // 90 observations
        let obs: Vec<Observation> = (0..n)
            .map(|i| {
                let roll = i as f64 / (n - 1) as f64; // 0.0 .. 1.0
                Observation {
                    timestamp_unix: i as u64,
                    league: "Standard".into(),
                    base_type: Some("Chiming Staff".into()),
                    // "Staves" is the clipboard plural → canonical_category gives "Staff"
                    category: Some("Staves".into()),
                    mods: vec![
                        ListingMod {
                            stat_id: stat_id.into(),
                            tier: None,
                            roll: Some(roll * 100.0), // roll 0..100
                        },
                        // unique tag mod per item so self-exclusion removes only this item
                        ListingMod {
                            stat_id: format!("explicit.tag{i}"),
                            tier: None,
                            roll: None,
                        },
                    ],
                    price_divine: price_divine * (1.0 + roll), // price tracks roll
                    source: Source::Harvest,
                    indexed: None,
                }
            })
            .collect();
        crate::trade::value::ValueModel::build(&obs, &crate::trade::stats::StatCatalog::default())
    }

    fn staff_item_with_stat(stat_raw: &str, stat_value: f64) -> ParsedItem {
        ParsedItem {
            rarity: Rarity::Rare,
            name: "Onslaught Spell".into(),
            base_type: Some("Chiming Staff".into()),
            item_class: Some("Staves".into()),
            item_level: Some(80),
            quality: None,
            corrupted: false,
            energy_shield: None,
            armour: None,
            evasion: None,
            implicits: vec![],
            enchants: vec![],
            runes: vec![],
            explicits: vec![ItemStat {
                raw: stat_raw.into(),
                value: Some(stat_value),
                affix: None,
                tier: None,
            }],
        }
    }

    #[test]
    fn learned_estimate_trusted_category_returns_estimate_near_seeded_price() {
        use crate::trade::stats::StatCatalog;
        // The stat catalog must match the raw stat line to a stat_id; use the
        // sample fixture for "80% increased Spell Damage" → "explicit.stat_spell_dmg".
        let catalog = StatCatalog::from_json(include_str!("fixtures/stats_sample.json")).unwrap();
        let model = trusted_staff_model("explicit.stat_spell_dmg", 10.0);
        let pricer = TradePricer::new(
            Flat(0.0), // not used by learned_estimate
            crate::trade::pseudo::PseudoMap::load(),
            catalog,
            crate::observe::ObservationLog::new(
                tempfile::tempdir().unwrap().path().join("obs.jsonl"),
            ),
            std::sync::Arc::new(std::sync::RwLock::new(model)),
        );
        let item = staff_item_with_stat("80% increased Spell Damage", 80.0);
        let est = pricer.learned_estimate(&item, "Standard");
        assert!(
            est.is_some(),
            "trusted category should return an estimate; got None"
        );
        let est = est.unwrap();
        // Seeded price is 10.0 div; estimate should be in a reasonable range.
        assert!(
            est.value_divine > 0.0,
            "estimate value should be positive, got {}",
            est.value_divine
        );
    }

    #[test]
    fn learned_estimate_untrusted_category_returns_none() {
        use crate::observe::{Observation, Source};
        use crate::trade::model::ListingMod;
        use crate::trade::stats::StatCatalog;
        use crate::trade::value::TRUST_MIN_SAMPLE;

        // Build a model with fewer than TRUST_MIN_SAMPLE (80) observations.
        let n = TRUST_MIN_SAMPLE - 1; // 79 — below trust bar
        let obs: Vec<Observation> = (0..n as u64)
            .map(|i| Observation {
                timestamp_unix: i,
                league: "Standard".into(),
                base_type: Some("Chiming Staff".into()),
                category: Some("Staves".into()),
                mods: vec![ListingMod {
                    stat_id: "explicit.stat_spell_dmg".into(),
                    tier: None,
                    roll: None,
                }],
                price_divine: 10.0,
                source: Source::Harvest,
                indexed: None,
            })
            .collect();
        let model = crate::trade::value::ValueModel::build(
            &obs,
            &crate::trade::stats::StatCatalog::default(),
        );

        let catalog = StatCatalog::from_json(include_str!("fixtures/stats_sample.json")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pricer = TradePricer::new(
            Flat(0.0),
            crate::trade::pseudo::PseudoMap::load(),
            catalog,
            crate::observe::ObservationLog::new(dir.path().join("obs.jsonl")),
            std::sync::Arc::new(std::sync::RwLock::new(model)),
        );
        let item = staff_item_with_stat("80% increased Spell Damage", 80.0);
        let est = pricer.learned_estimate(&item, "Standard");
        assert!(
            est.is_none(),
            "thin category (n={n} < TRUST_MIN_SAMPLE) should return None"
        );
    }

    #[test]
    fn learned_estimate_no_item_class_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let pricer = TradePricer::new(
            Flat(0.0),
            crate::trade::pseudo::PseudoMap::load(),
            crate::trade::stats::StatCatalog::default(),
            crate::observe::ObservationLog::new(dir.path().join("obs.jsonl")),
            std::sync::Arc::new(std::sync::RwLock::new(
                crate::trade::value::ValueModel::default(),
            )),
        );
        let mut item = staff_item_with_stat("80% increased Spell Damage", 80.0);
        item.item_class = None; // no item class → canonical_category can't run
        let est = pricer.learned_estimate(&item, "Standard");
        assert!(est.is_none(), "missing item_class must return None");
    }

    #[test]
    fn learned_estimate_zero_skill_returns_none() {
        // Exercises the RIGHT-HAND branch of the trust bar: a category that CLEARS
        // the sample-size gate (sample_size >= TRUST_MIN_SAMPLE) but whose calibration
        // shows no positive skill over the category-median baseline must return None.
        // Without this, a model that fits no better than guessing the median could be trusted.
        use crate::trade::stats::StatCatalog;
        use crate::trade::value::backtest::Calibration;
        use crate::trade::value::estimate::SimWeights;
        use crate::trade::value::itemvec::ItemVector;
        use crate::trade::value::{CategoryModel, TRUST_MIN_SAMPLE};

        // Plenty of neighbours sharing the query stat, so estimate() WOULD succeed
        // if the trust check were bypassed — proving None comes from zero skill, not
        // a missing-neighbour shortfall.
        let items: Vec<ItemVector> = (0..20)
            .map(|i| ItemVector {
                mods: vec![("explicit.stat_spell_dmg".to_string(), Some(0.5))],
                price_divine: 10.0 + i as f64,
            })
            .collect();
        let cat = CategoryModel {
            category: "Staff".to_string(),
            sample_size: TRUST_MIN_SAMPLE + 10, // clears the sample-size gate
            base_median: 10.0,
            items,
            weights: SimWeights {
                jaccard: 1.0,
                roll: 0.0,
            },
            // skill = 0.0 → not trusted (no positive skill over baseline)
            calibration: Calibration {
                model_err: Some(0.7),
                baseline_err: Some(0.7),
                skill: Some(0.0),
            },
            ..Default::default()
        };
        let model = crate::trade::value::ValueModel::with_category("Standard", cat);

        let catalog = StatCatalog::from_json(include_str!("fixtures/stats_sample.json")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pricer = TradePricer::new(
            Flat(0.0),
            crate::trade::pseudo::PseudoMap::load(),
            catalog,
            crate::observe::ObservationLog::new(dir.path().join("obs.jsonl")),
            std::sync::Arc::new(std::sync::RwLock::new(model)),
        );
        let item = staff_item_with_stat("80% increased Spell Damage", 80.0);
        let est = pricer.learned_estimate(&item, "Standard");
        assert!(
            est.is_none(),
            "zero-skill calibration must return None (model no better than median baseline)"
        );
    }

    #[tokio::test]
    async fn harvest_subdivides_dense_value_bands() {
        use crate::trade::client::TradeApi;
        use crate::trade::model::SearchResponse;
        use std::sync::Mutex;

        // A value-range band that reports far more matches than the API returns
        // (incomplete) is bisected until each fetched leaf is covered or the depth cap
        // is hit; cheap (<20 div) and already-complete bands are harvested once. Each
        // leaf yields listings priced at its band floor, so subdivision shows up as
        // price points strictly between a top band's edges.
        struct DenseFake {
            searches: Mutex<Vec<(f64, Option<f64>)>>,
        }
        #[async_trait]
        impl Comparables for DenseFake {
            async fn comparables(
                &self,
                _q: &TradeQuery,
                _l: usize,
                _mr: usize,
                _mm: usize,
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                Ok(vec![])
            }
        }
        #[async_trait]
        impl TradeApi for DenseFake {
            async fn search(
                &self,
                q: &TradeQuery,
                _s: &TradeSession,
            ) -> anyhow::Result<SearchResponse> {
                let lo = q.min_price_divine.unwrap_or(0.0);
                let hi = q.max_price_divine;
                self.searches.lock().unwrap().push((lo, hi));
                let width = hi.map(|h| h - lo).unwrap_or(f64::INFINITY);
                // More matches than the 100-cap inside the value range while the slice
                // is still wide; otherwise fully covered.
                let incomplete = (20.0..200.0).contains(&lo) && width > 5.0;
                let total: u64 = if incomplete { 500 } else { 3 };
                let n = (total as usize).min(100);
                let hashes = (0..n).map(|i| format!("{lo}_{i}")).collect();
                Ok(SearchResponse {
                    id: "qid".into(),
                    total,
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
                    .map(|h| {
                        let price = h.split('_').next().unwrap().parse::<f64>().unwrap();
                        Listing {
                            price: Money {
                                amount: price,
                                currency: Currency::Divine,
                            },
                            price_divine: price,
                            explicit_count: 1,
                            id: h.clone(),
                            base_type: Some("Chiming Staff".into()),
                            mods: vec![crate::trade::model::ListingMod {
                                stat_id: "explicit.stat_1".into(),
                                tier: Some(1),
                                roll: Some(50.0),
                            }],
                            indexed: None,
                        }
                    })
                    .collect())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obs.jsonl");
        let pricer = TradePricer::new(
            DenseFake {
                searches: Mutex::new(vec![]),
            },
            crate::trade::pseudo::PseudoMap::load(),
            crate::trade::stats::StatCatalog::default(),
            crate::observe::ObservationLog::new(&path),
            std::sync::Arc::new(std::sync::RwLock::new(
                crate::trade::value::ValueModel::default(),
            )),
        );
        let n = pricer
            .harvest(
                "weapon.staff",
                "Staff",
                "Standard",
                &TradeSession::for_test(),
            )
            .await
            .unwrap();
        assert!(n > 0);

        let n_searches = pricer.comparables.searches.lock().unwrap().len();
        assert!(
            n_searches > PRICE_BANDS.len(),
            "dense value bands were subdivided ({n_searches} searches)"
        );
        assert!(
            n_searches < 80,
            "subdivision is bounded by the depth cap ({n_searches} searches)"
        );

        // Subdivision reached above the [20,50) floor: some listing priced in (20,50).
        let body = std::fs::read_to_string(&path).unwrap();
        let prices: Vec<f64> = body
            .lines()
            .map(|l| serde_json::from_str::<Observation>(l).unwrap().price_divine)
            .collect();
        assert!(
            prices.iter().any(|&p| p > 20.0 && p < 50.0),
            "fetched a sub-band floor strictly inside (20,50), not just the band edge"
        );
    }

    /// `harvest_mod` must include the pinned stat id in every band query's `stats`
    /// vec. Uses a fake `TradeApi` that records every `TradeQuery` it receives so
    /// we can assert post-hoc that each query carries the expected `StatFilter`.
    #[tokio::test]
    async fn harvest_mod_pins_stat_id_in_every_band_query() {
        use crate::trade::client::TradeApi;
        use crate::trade::model::{SearchResponse, StatFilter};
        use std::sync::Mutex;

        struct ModHarvestFake {
            queries: Mutex<Vec<TradeQuery>>,
        }
        #[async_trait]
        impl Comparables for ModHarvestFake {
            async fn comparables(
                &self,
                _q: &TradeQuery,
                _l: usize,
                _mr: usize,
                _mm: usize,
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                Ok(vec![])
            }
        }
        #[async_trait]
        impl TradeApi for ModHarvestFake {
            async fn search(
                &self,
                q: &TradeQuery,
                _s: &TradeSession,
            ) -> anyhow::Result<SearchResponse> {
                self.queries.lock().unwrap().push(q.clone());
                let band = q.min_price_divine.unwrap_or(0.0);
                Ok(SearchResponse {
                    id: format!("q{band}"),
                    total: 1,
                    hashes: vec![format!("h{band}")],
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
                    .map(|h| Listing {
                        price: Money {
                            amount: 1.0,
                            currency: Currency::Divine,
                        },
                        price_divine: 1.0,
                        explicit_count: 1,
                        id: h.clone(),
                        base_type: Some("Chiming Staff".into()),
                        mods: vec![crate::trade::model::ListingMod {
                            stat_id: "explicit.stat_gate".into(),
                            tier: Some(1),
                            roll: Some(50.0),
                        }],
                        indexed: None,
                    })
                    .collect())
            }
        }

        const TARGET_STAT: &str = "explicit.stat_gate";

        let dir = tempfile::tempdir().unwrap();
        let pricer = TradePricer::new(
            ModHarvestFake {
                queries: Mutex::new(vec![]),
            },
            crate::trade::pseudo::PseudoMap::load(),
            crate::trade::stats::StatCatalog::default(),
            crate::observe::ObservationLog::new(dir.path().join("obs.jsonl")),
            std::sync::Arc::new(std::sync::RwLock::new(
                crate::trade::value::ValueModel::default(),
            )),
        );
        let n = pricer
            .harvest_mod(
                "weapon.staff",
                "Staff",
                "Standard",
                TARGET_STAT,
                &TradeSession::for_test(),
            )
            .await
            .unwrap();

        // Exactly one observation logged per band (the fake returns one per band).
        assert_eq!(n, PRICE_BANDS.len(), "expected exactly one obs per band");

        // Every search query must carry exactly the pinned stat filter.
        let queries = pricer.comparables.queries.lock().unwrap();
        assert_eq!(
            queries.len(),
            PRICE_BANDS.len(),
            "expected one search per band"
        );
        for q in queries.iter() {
            assert_eq!(
                q.stats.len(),
                1,
                "each band query must carry exactly one StatFilter"
            );
            assert_eq!(
                q.stats[0],
                StatFilter {
                    id: TARGET_STAT.to_string(),
                    label: String::new(),
                    min: None,
                    max: None,
                },
                "pinned stat filter must match the requested stat_id"
            );
        }
    }
}
