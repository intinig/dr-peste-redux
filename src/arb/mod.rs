pub mod graph;
pub mod model;
pub mod source;
pub mod spread;

use crate::arb::model::{Freshness, Opportunity};
use crate::arb::source::CandidateSource;
use anyhow::Result;
use std::cmp::Ordering;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct ArbConfig {
    pub max_cycle_len: usize,
    pub min_profit_pct: f64,
    pub min_spread_pct: f64,
    /// Plausibility caps: a cycle above `max_profit_pct` or a flip above
    /// `max_spread_pct` is almost certainly a stale/one-sided thin book, not real
    /// arbitrage, so it is dropped rather than surfaced.
    pub max_profit_pct: f64,
    pub max_spread_pct: f64,
    pub min_volume: f64,
    pub top_n: usize,
}

pub struct ArbEngine {
    source: Arc<dyn CandidateSource>,
    cfg: ArbConfig,
}

impl ArbEngine {
    pub fn new(source: Arc<dyn CandidateSource>, cfg: ArbConfig) -> Self {
        ArbEngine { source, cfg }
    }

    pub async fn opportunities(&self, league: &str) -> Result<Vec<Opportunity>> {
        let edges = self.source.edges(league).await?;
        let graph = graph::RateGraph::from_edges(&edges);
        let cycles = graph.profitable_cycles(self.cfg.max_cycle_len, self.cfg.min_profit_pct);
        let flips = spread::scan(&edges, self.cfg.min_spread_pct, self.cfg.min_volume);

        let mut opps: Vec<Opportunity> = Vec::new();
        for c in cycles {
            // Require real throughput AND a plausible profit: a cycle above the
            // cap is a stale/erroneous thin-book quote (e.g. a fat-fingered
            // listing), not executable arbitrage.
            if c.feasible_volume >= self.cfg.min_volume
                && (c.multiplier - 1.0) <= self.cfg.max_profit_pct
            {
                opps.push(Opportunity::from_cycle(c, Freshness::Live));
            }
        }
        for f in flips {
            // Drop implausibly wide spreads — one-sided/stale books, not a real
            // maker opportunity.
            if f.spread_pct <= self.cfg.max_spread_pct {
                opps.push(Opportunity::from_flip(f, Freshness::Live));
            }
        }
        opps.sort_by(|a, b| b.score().partial_cmp(&a.score()).unwrap_or(Ordering::Equal));
        opps.truncate(self.cfg.top_n);
        Ok(opps)
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;
    use crate::arb::model::{Edge, RatioQuote};
    use async_trait::async_trait;

    struct Fixed(Vec<Edge>);

    #[async_trait]
    impl CandidateSource for Fixed {
        async fn edges(&self, _l: &str) -> Result<Vec<Edge>> {
            Ok(self.0.clone())
        }
    }

    fn e(from: &str, to: &str, pay: u32, get: u32, stock: u64) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            quote: RatioQuote {
                pay,
                get,
                stock,
                freshness: Freshness::Live,
            },
        }
    }

    #[tokio::test]
    async fn surfaces_and_ranks() {
        // One +20% triangle: A→B (×2), B→C (×2), C→A (×0.3) = ×1.2 net
        let edges = vec![
            e("A", "B", 1, 2, 1000),
            e("B", "C", 1, 2, 1000),
            e("C", "A", 10, 3, 1000),
        ];
        let eng = ArbEngine::new(
            Arc::new(Fixed(edges)),
            ArbConfig {
                max_cycle_len: 4,
                min_profit_pct: 0.0,
                min_spread_pct: 0.5,
                max_profit_pct: 1.0,
                max_spread_pct: 1.0,
                min_volume: 0.0,
                top_n: 10,
            },
        );
        let opps = eng.opportunities("X").await.unwrap();
        assert!(matches!(opps[0], Opportunity::Triangulation { .. }));
    }

    #[tokio::test]
    async fn abstains_when_nothing_clears() {
        let edges = vec![e("A", "B", 1, 2, 100), e("B", "A", 2, 1, 100)];
        let eng = ArbEngine::new(
            Arc::new(Fixed(edges)),
            ArbConfig {
                max_cycle_len: 4,
                min_profit_pct: 0.5,
                min_spread_pct: 0.5,
                max_profit_pct: 1.0,
                max_spread_pct: 1.0,
                min_volume: 0.0,
                top_n: 10,
            },
        );
        assert!(eng.opportunities("X").await.unwrap().is_empty());
    }

    fn cfg(max_profit_pct: f64, max_spread_pct: f64) -> ArbConfig {
        ArbConfig {
            max_cycle_len: 4,
            min_profit_pct: 0.0,
            min_spread_pct: 0.01,
            max_profit_pct,
            max_spread_pct,
            min_volume: 0.0,
            top_n: 10,
        }
    }

    #[tokio::test]
    async fn cap_drops_implausible_cycle() {
        // ×1000 cycle (stale/fat-fingered book) must be dropped by max_profit_pct.
        let edges = vec![
            e("A", "B", 1, 10, 1000),
            e("B", "C", 1, 10, 1000),
            e("C", "A", 1, 10, 1000),
        ];
        let eng = ArbEngine::new(Arc::new(Fixed(edges)), cfg(0.5, 0.5));
        assert!(eng.opportunities("X").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn cap_drops_wide_flip() {
        // 50% spread exceeds max_spread_pct (0.25) → dropped.
        let edges = vec![e("A", "B", 2, 1, 500), e("B", "A", 1, 1, 500)];
        let eng = ArbEngine::new(Arc::new(Fixed(edges)), cfg(0.5, 0.25));
        assert!(eng.opportunities("X").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn keeps_plausible_flip() {
        // 10% spread, within caps → surfaced as a Flip.
        let edges = vec![e("A", "B", 10, 9, 500), e("B", "A", 1, 1, 500)];
        let eng = ArbEngine::new(Arc::new(Fixed(edges)), cfg(0.5, 0.25));
        let opps = eng.opportunities("X").await.unwrap();
        assert!(opps.iter().any(|o| matches!(o, Opportunity::Flip { .. })));
    }
}
