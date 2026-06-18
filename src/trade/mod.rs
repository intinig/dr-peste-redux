//! On-demand rare-item pricing via live trade2 ablation. Isolated from
//! `poeninja`/`store`: data flows discord → trade, never sideways.

pub mod ablation;
pub mod client;
pub mod model;
pub mod pseudo;
pub mod query;
pub mod rates;
pub mod session;
pub mod stats;

use anyhow::Result;

use crate::itemtext::ParsedItem;
use crate::pricelog::ProbeLog;
use crate::trade::ablation::{estimate, Comparables};
use crate::trade::model::{Breakdown, PriceEstimate, Probe};
use crate::trade::pseudo::PseudoMap;
use crate::trade::query::build_baseline;
use crate::trade::session::TradeSession;
use crate::trade::stats::StatCatalog;

/// Number of cheapest listings to fetch per query before craftability filtering.
/// Widened so craft-tier comparables aren't crowded out by a deep junk floor before
/// the filter runs. (A fuller fix — paginating further when no craft-tier survivors
/// are found — is tracked as a follow-up.)
const COMPARABLE_SAMPLE: usize = 50;
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
        let max_explicit = item.craftability().map(|c| c.explicit_count as usize);
        let est = estimate(
            &self.comparables,
            &query,
            COMPARABLE_SAMPLE,
            session,
            max_explicit,
        )
        .await?;
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
    use crate::trade::model::{Currency, Listing, Money, TradeQuery};
    use crate::trade::session::TradeSession;
    use async_trait::async_trait;

    struct Flat(f64);
    #[async_trait]
    impl Comparables for Flat {
        async fn comparables(
            &self,
            _q: &TradeQuery,
            _l: usize,
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
}
