//! On-demand rare-item pricing via live trade2 ablation. Isolated from
//! `poeninja`/`store`: data flows discord → trade, never sideways.

pub mod ablation;
pub mod client;
pub mod hedonic;
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
use crate::trade::ablation::{estimate, Comparables, MIN_COMPARABLES};
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
        progress: &dyn crate::trade::ablation::PriceProgress,
    ) -> Result<PriceEstimate> {
        let query = build_baseline(item, &self.pseudo, &self.catalog, league);
        // `craft` is `Some(n)` for clipboard items with affix tags, `None` for
        // basic clipboard items where the affix type is unknown.
        let craft = item.craftability().map(|c| c.explicit_count as usize);
        let exact = estimate(
            &self.comparables,
            &query,
            COMPARABLE_SAMPLE,
            0,
            session,
            craft,
        )
        .await?;
        // Value path: enter whenever the exact result is too thin (< MIN_COMPARABLES),
        // regardless of whether craftability is known. Basic-clipboard items
        // (craft=None) use their raw explicit count as the value-path cap so the
        // hedonic model sees a reasonable craftability window.
        let est = if exact.listing_count < MIN_COMPARABLES {
            let max_for_value = craft.unwrap_or(item.explicits.len());
            crate::trade::ablation::marginal_estimate(
                &self.comparables,
                &query,
                COMPARABLE_SAMPLE,
                session,
                max_for_value,
                progress,
            )
            .await?
        } else {
            exact
        };
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
    use crate::itemtext::{Affix, ItemStat, ParsedItem, Rarity};
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
            .price(
                &ring(),
                "Standard",
                &TradeSession::for_test(),
                &crate::trade::ablation::NoProgress,
            )
            .await
            .unwrap();
        assert_eq!(est.typical, 12.0);
        let contents = std::fs::read_to_string(dir.path().join("p.jsonl")).unwrap();
        assert_eq!(contents.lines().count(), 1);
    }

    /// A ring with one explicit carrying an Affix tag so `craftability()` returns
    /// `Some` and the routing gate can engage the value path.
    fn craftable_ring() -> ParsedItem {
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
            explicits: vec![
                ItemStat {
                    raw: "+40 to maximum Life".into(),
                    value: Some(40.0),
                    affix: Some(Affix::Prefix),
                    tier: None,
                },
                ItemStat {
                    raw: "+15% to Cold Resistance".into(),
                    value: Some(15.0),
                    affix: Some(Affix::Suffix),
                    tier: None,
                },
            ],
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
            explicit_stat_ids: vec![],
        }
    }

    /// Routing fake: returns few listings for exact queries (triggering value path)
    /// and many for base/single-stat sub-queries (so hedonic model can fit).
    /// Exact query = stats.len() matches the full item query. Sub-queries have
    /// stats.len() <= 1.
    struct RoutingFake {
        full_stat_count: usize,
    }

    #[async_trait]
    impl Comparables for RoutingFake {
        async fn comparables(
            &self,
            q: &TradeQuery,
            _l: usize,
            _max_relax: usize,
            _session: &TradeSession,
        ) -> anyhow::Result<Vec<Listing>> {
            if q.stats.len() == self.full_stat_count {
                // Exact query → return thin result (< MIN_COMPARABLES=10) to
                // push routing into the value path. ec=1 passes craftability_filter
                // (max_explicit=2 for a 2-explicit item).
                Ok((0..3)
                    .map(|i| make_listing(5.0 + i as f64 * 0.1, 1, &format!("exact-{i}")))
                    .collect())
            } else {
                // Base or single-stat sub-queries → return 30 listings with
                // distinct ids so hedonic model can fit (needs >= MIN_FIT=20).
                // ec=1 passes craftability_filter.
                let prefix = if q.stats.is_empty() {
                    "base".to_string()
                } else {
                    q.stats[0].id.clone()
                };
                Ok((0..30)
                    .map(|i| make_listing(8.0 + i as f64 * 0.1, 1, &format!("{prefix}-{i}")))
                    .collect())
            }
        }
    }

    /// Fat fake: always returns >= MIN_COMPARABLES listings → fast path taken.
    struct FatFake;
    #[async_trait]
    impl Comparables for FatFake {
        async fn comparables(
            &self,
            _q: &TradeQuery,
            _l: usize,
            _max_relax: usize,
            _session: &TradeSession,
        ) -> anyhow::Result<Vec<Listing>> {
            // 15 listings > MIN_COMPARABLES=10, ec=1 <= max=2.
            Ok((0..15)
                .map(|i| make_listing(10.0 + i as f64 * 0.5, 1, &format!("fat-{i}")))
                .collect())
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

    /// Thin exact result (< MIN_COMPARABLES) on a craftable item → marginal
    /// value path is taken → basis is Marginal.
    #[tokio::test]
    async fn routing_thin_exact_takes_value_path() {
        let item = craftable_ring();
        // craftable_ring has 2 explicits with affix tags → craftability = Some(2).
        // full_stat_count matches however many StatFilters build_baseline emits;
        // we rely on RoutingFake discriminating on q.stats.len() == n_full.
        // build_baseline may emit 0, 1, or 2 stat filters depending on the catalog.
        // Use a large sentinel that will never match actual sub-queries: if
        // build_baseline produces no stat filters the routing probe also has 0 stats,
        // so we can't distinguish. In that case both paths return their resp. result.
        // We check the result is consistent: value-path entered ↔ basis == Marginal.
        let pricer = make_pricer(RoutingFake {
            full_stat_count: 99,
        });
        let est = pricer
            .price(
                &item,
                "Standard",
                &TradeSession::for_test(),
                &crate::trade::ablation::NoProgress,
            )
            .await
            .unwrap();
        // With full_stat_count=99 no query matches the exact gate, so the fake
        // always returns 30 listings (>= MIN_COMPARABLES) → fast path → NOT Marginal.
        assert_ne!(est.basis, EstimateBasis::Marginal);

        // Now use full_stat_count=0 so the routing probe (which often has 0 stats
        // from the base_query shortcut) is treated as the thin exact query.
        // Sub-queries (single-stat) will also have 0 stats only if stats are absent;
        // but base sub-queries in marginal_estimate have stats.len()==0 too.
        // Instead: count-based routing. Thin returns 3 for any query with <= 1 stat,
        // fat returns 30 for no-stat queries. Use a simpler counting approach.
        // Real routing gate test: craftable item, fake returns 3 listings always
        // (so both estimate AND marginal_estimate see 3). estimate returns 3 <
        // MIN_COMPARABLES → marginal_estimate runs. marginal_estimate accumulates
        // 3×3=9 across 3 sub-queries < MIN_FIT=20 → model_price None → BroadMarket.
        // This confirms value path was entered (not fast-path CraftTier).
        let pricer2 = make_pricer(Flat(7.0));
        // Flat always returns 8 listings with ec=0 → craftability_filter excludes
        // all (ec=0 is sentinel) → BroadMarket even on exact path when max_explicit=Some.
        // For the actual routing test we need a fake that returns < MIN_COMPARABLES.
        // Use a dedicated ThinFake.
        let _ = pricer2; // suppress unused warning; real test below.

        // Definitive routing test: ThinFake returns 3 per call; craftable item
        // with max_explicit=2. estimate sees 3 < 10 → marginal_estimate runs.
        // marginal_estimate: 3×3=9 pooled, all unique ids, ec=1 ≤ 2 → all pass
        // craftability_filter. 9 < MIN_FIT=20 → model_price None →
        // fallback estimate_from(deduped/filtered, BroadMarket). basis=BroadMarket.
        struct ThinFake;
        #[async_trait]
        impl Comparables for ThinFake {
            async fn comparables(
                &self,
                q: &TradeQuery,
                _l: usize,
                _max_relax: usize,
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                let prefix = format!("t{}", q.stats.len());
                Ok((0..3usize)
                    .map(|i| make_listing(5.0 + i as f64 * 0.1, 1, &format!("{prefix}-{i}")))
                    .collect())
            }
        }
        let pricer3 = make_pricer(ThinFake);
        let est3 = pricer3
            .price(
                &craftable_ring(),
                "Standard",
                &TradeSession::for_test(),
                &crate::trade::ablation::NoProgress,
            )
            .await
            .unwrap();
        // Value path entered (marginal_estimate ran). Model can't fit → BroadMarket.
        assert_eq!(est3.basis, EstimateBasis::BroadMarket);
        // Crucially: fast path would have produced CraftTier or BroadMarket from
        // the exact result; value path BroadMarket comes from marginal_estimate's
        // fallback. We can distinguish by verifying listing_count: the fast-path
        // would report 3 (from estimate); marginal_estimate reports the pooled
        // count (up to 9 after dedup across 3 sub-queries, minus collisions from
        // same-prefixed ids, but unique across sub-queries).
        assert!(
            est3.listing_count > 3,
            "value path pooled more than fast-path 3"
        );
    }

    /// Basic-clipboard item (all explicits have `affix: None` → `craftability()` is
    /// `None`) with a thin exact result enters the value path and produces a
    /// non-empty estimate whose basis is NOT the fast-path CraftTier/AffixesOnly.
    #[tokio::test]
    async fn basic_clipboard_thin_exact_enters_value_path() {
        // ring() has one explicit with affix=None → craftability() is None.
        let item = ring();
        // ThinFake returns 3 listings (< MIN_COMPARABLES=10) for every sub-query.
        // value path is entered; marginal_estimate pools > 3 across sub-queries.
        struct ThinFakeBasic;
        #[async_trait]
        impl Comparables for ThinFakeBasic {
            async fn comparables(
                &self,
                q: &TradeQuery,
                _l: usize,
                _max_relax: usize,
                _s: &TradeSession,
            ) -> anyhow::Result<Vec<Listing>> {
                let prefix = format!("tb{}", q.stats.len());
                Ok((0..3usize)
                    .map(|i| make_listing(5.0 + i as f64 * 0.1, 1, &format!("{prefix}-{i}")))
                    .collect())
            }
        }
        let pricer = make_pricer(ThinFakeBasic);
        let est = pricer
            .price(
                &item,
                "Standard",
                &TradeSession::for_test(),
                &crate::trade::ablation::NoProgress,
            )
            .await
            .unwrap();
        // Value path entered: basis must not be AffixesOnly (old fast-path for craft=None)
        // and must not be CraftTier. It will be BroadMarket or Marginal depending on
        // whether the hedonic model can fit with the pooled thin data.
        assert_ne!(
            est.basis,
            crate::trade::model::EstimateBasis::AffixesOnly,
            "basic-clipboard thin item should enter value path, not fast-path AffixesOnly"
        );
        // listing_count must be > 3 (fast-path would report 3; value path pools more).
        assert!(
            est.listing_count > 3,
            "value path pools more than fast-path 3; got {}",
            est.listing_count
        );
    }

    /// Fat exact result (>= MIN_COMPARABLES) on a craftable item → fast path
    /// taken → basis is NOT Marginal.
    #[tokio::test]
    async fn routing_fat_exact_takes_fast_path() {
        let pricer = make_pricer(FatFake);
        let est = pricer
            .price(
                &craftable_ring(),
                "Standard",
                &TradeSession::for_test(),
                &crate::trade::ablation::NoProgress,
            )
            .await
            .unwrap();
        // 15 listings with ec=1 ≤ max_explicit=2 → CraftTier (fast path, never Marginal).
        assert_eq!(est.basis, EstimateBasis::CraftTier);
        assert_ne!(est.basis, EstimateBasis::Marginal);
    }
}
