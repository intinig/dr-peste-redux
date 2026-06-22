//! On-demand rare-item pricing via live trade2 ablation. Isolated from
//! `poeninja`/`store`: data flows discord → trade, never sideways.

pub mod ablation;
pub mod client;
pub mod limiter;
pub mod model;
pub mod pseudo;
pub mod query;
pub mod rates;
pub mod session;
pub mod stats;

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
                base_type: item.base_type.clone(),
                category: item.item_class.clone(),
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
}
