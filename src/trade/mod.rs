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
use crate::pricelog::ProbeLog;
use crate::trade::ablation::{price_check, Comparables};
use crate::trade::model::{Breakdown, PriceEstimate, Probe};
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
    log: ProbeLog,
}

impl<C: Comparables> TradePricer<C> {
    pub fn new(comparables: C, pseudo: PseudoMap, catalog: StatCatalog, log: ProbeLog) -> Self {
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
        let est = price_check(&self.comparables, &query, PRICE_SAMPLE, max_relax, session).await?;
        self.record(&query, &est);
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
        self.record(&query, &bd.baseline);
        Ok(bd)
    }

    fn record(&self, query: &crate::trade::model::TradeQuery, est: &PriceEstimate) {
        let timestamp_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let probe = Probe {
            query: query.clone(),
            listing_count: est.listing_count,
            typical_divine: est.typical,
            timestamp_unix,
        };
        if let Err(e) = self.log.append(&probe) {
            tracing::warn!(error = %e, "failed to append probe to price log");
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
            Ok(vec![
                Listing {
                    price: Money {
                        amount: self.0,
                        currency: Currency::Divine
                    },
                    price_divine: self.0,
                    explicit_count: 0,
                    id: String::new(),
                    explicit_stat_ids: vec![],
                };
                8
            ])
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

    #[tokio::test]
    async fn price_logs_a_probe_and_returns_estimate() {
        let dir = tempfile::tempdir().unwrap();
        let log = crate::pricelog::ProbeLog::new(dir.path().join("p.jsonl"));
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
        let contents = std::fs::read_to_string(dir.path().join("p.jsonl")).unwrap();
        assert_eq!(contents.lines().count(), 1);
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
            explicit_stat_ids: vec![],
        }
    }

    fn make_pricer<C: Comparables>(c: C) -> TradePricer<C> {
        let dir = tempfile::tempdir().unwrap();
        TradePricer::new(
            c,
            crate::trade::pseudo::PseudoMap::load(),
            crate::trade::stats::StatCatalog::default(),
            crate::pricelog::ProbeLog::new(dir.path().join("p.jsonl")),
        )
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
        let pricer = make_pricer(Comps);
        let est = pricer
            .price(&ring(), "Standard", &TradeSession::for_test())
            .await
            .unwrap();
        assert!(est.typical > 0.0 && est.typical <= 12.0);
        assert!(est.low <= est.typical && est.typical <= est.high);
        assert_eq!(est.listing_count, 12);
        assert_eq!(est.basis, EstimateBasis::CraftTier); // >= MIN_COMPARABLES found
    }
}
