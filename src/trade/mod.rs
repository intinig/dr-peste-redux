//! On-demand rare-item pricing via live trade2 ablation. Isolated from
//! `poeninja`/`store`: data flows discord → trade, never sideways.

pub mod ablation;
pub mod categories;
pub mod client;
pub mod limiter;
pub mod model;
pub mod pseudo;
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
/// Min-price bands (Divine) for a harvest sweep. Each band fetches the cheapest
/// HARVEST_SAMPLE listings at or above it, so together they span the price
/// spectrum (cheapest-first search otherwise hides the expensive end).
const PRICE_BANDS: [f64; 4] = [0.0, 5.0, 20.0, 50.0];
/// Cheapest listings fetched per band.
const HARVEST_SAMPLE: usize = 100;

pub struct TradePricer<C: Comparables> {
    comparables: C,
    pseudo: PseudoMap,
    catalog: StatCatalog,
    log: ObservationLog,
}

impl<C: Comparables> TradePricer<C> {
    pub fn new(
        comparables: C,
        pseudo: PseudoMap,
        catalog: StatCatalog,
        log: ObservationLog,
    ) -> Self {
        TradePricer {
            comparables,
            pseudo,
            catalog,
            log,
        }
    }

    pub async fn price(
        &self,
        item: &ParsedItem,
        league: &str,
        session: &TradeSession,
    ) -> Result<PriceEstimate> {
        let query = build_baseline(item, &self.pseudo, &self.catalog, league);
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
        let query = build_baseline(item, &self.pseudo, &self.catalog, league);
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

    /// Append one `Observation { source: Paste }` per fetched comparable. Best-
    /// effort: a write failure is logged, never fatal.
    fn log_observations(&self, item: &ParsedItem, league: &str, listings: &[Listing]) {
        let timestamp_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        for l in listings {
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
            };
            if let Err(e) = self.log.append(&obs) {
                tracing::warn!(error = %e, "failed to append observation");
            }
        }
    }
}

impl<C: Comparables + crate::trade::client::TradeApi> TradePricer<C> {
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
        let mut logged = 0usize;
        // Bands overlap when a category has fewer than HARVEST_SAMPLE listings
        // below the next threshold (the min-only lower band then re-fetches the
        // higher band's items). Dedup by listing id across bands so the corpus
        // isn't skewed toward the same high-priced items. Empty ids can't be
        // deduped, so they're always logged.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for band in PRICE_BANDS {
            let q = crate::trade::model::TradeQuery {
                league: league.to_string(),
                category: Some(category_id.to_string()),
                type_line: None,
                stats: vec![],
                misc: crate::trade::model::MiscFilters::default(),
                equipment: vec![],
                min_price_divine: if band > 0.0 { Some(band) } else { None },
            };
            let resp = match self.comparables.search(&q, session).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, band, "harvest band search failed; skipping");
                    continue;
                }
            };
            let take = resp.hashes.len().min(HARVEST_SAMPLE);
            let listings = match self
                .comparables
                .fetch(&resp.id, &resp.hashes[..take], session)
                .await
            {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(error = %e, band, "harvest band fetch failed; skipping");
                    continue;
                }
            };
            let timestamp_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            for l in &listings {
                if !l.id.is_empty() && !seen.insert(l.id.clone()) {
                    continue; // already logged this listing from an earlier band
                }
                let obs = Observation {
                    timestamp_unix,
                    league: league.to_string(),
                    base_type: l.base_type.clone(),
                    category: Some(category_text.to_string()),
                    mods: l.mods.clone(),
                    price_divine: l.price_divine,
                    source: Source::Harvest,
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
                    mods: vec![],
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
            mods: vec![],
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
                        mods: vec![],
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
}
