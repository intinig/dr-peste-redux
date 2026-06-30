//! Flip detection: a maker captures the round-trip deficit on a single market.
//! For market {A,B}, taking both directions returns ratio(A->B)*ratio(B->A) < 1;
//! the deficit (1 - product) is the spread a maker can earn. Pure, no I/O.

use crate::arb::model::{Currency, Edge, RatioQuote};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct FlipResult {
    pub market: (Currency, Currency),
    pub spread_pct: f64,
    pub volume: f64,
    /// Executable top-of-book quotes for each direction of the market, so the
    /// display can show concrete buy/sell ratios. `ab` is `market.0 -> market.1`,
    /// `ba` is `market.1 -> market.0`.
    pub ab: RatioQuote,
    pub ba: RatioQuote,
}

pub fn scan(edges: &[Edge], min_spread: f64, min_volume: f64) -> Vec<FlipResult> {
    // Index directed edges by (from,to).
    let mut by_pair: HashMap<(Currency, Currency), &Edge> = HashMap::new();
    for e in edges {
        by_pair.insert((e.from.clone(), e.to.clone()), e);
    }
    let mut out: Vec<FlipResult> = Vec::new();
    let mut done: std::collections::HashSet<(Currency, Currency)> =
        std::collections::HashSet::new();
    for e in edges {
        let (a, b) = if e.from <= e.to {
            (e.from.clone(), e.to.clone())
        } else {
            (e.to.clone(), e.from.clone())
        };
        if !done.insert((a.clone(), b.clone())) {
            continue;
        }
        let (Some(ab), Some(ba)) = (
            by_pair.get(&(a.clone(), b.clone())),
            by_pair.get(&(b.clone(), a.clone())),
        ) else {
            continue;
        };
        let product = ab.quote.ratio() * ba.quote.ratio();
        let spread_pct = (1.0 - product).max(0.0);
        // NOTE: stock(A→B) is denominated in B and stock(B→A) is denominated in A,
        // so this `min` is a deliberate Phase-1 fill-likelihood PROXY, not a true
        // tradeable quantity. Phase 2 should normalize both sides to a common
        // currency before using this value for ranking.
        let volume = ab.quote.stock.min(ba.quote.stock) as f64;
        if spread_pct >= min_spread && volume >= min_volume {
            out.push(FlipResult {
                market: (a, b),
                spread_pct,
                volume,
                ab: ab.quote.clone(),
                ba: ba.quote.clone(),
            });
        }
    }
    out.sort_by(|x, y| {
        (y.spread_pct * y.volume)
            .partial_cmp(&(x.spread_pct * x.volume))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arb::model::{Freshness, RatioQuote};

    fn edge(from: &str, to: &str, pay: u32, get: u32, stock: u64) -> Edge {
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

    #[test]
    fn detects_spread() {
        // A->B ratio 0.9, B->A ratio 1.0 => product 0.9 => 10% spread.
        let edges = vec![edge("A", "B", 10, 9, 500), edge("B", "A", 1, 1, 300)];
        let flips = scan(&edges, 0.01, 0.0);
        assert_eq!(flips.len(), 1);
        assert!((flips[0].spread_pct - 0.1).abs() < 1e-9);
        assert!((flips[0].volume - 300.0).abs() < 1e-9);
    }

    #[test]
    fn filters_below_thresholds() {
        let edges = vec![edge("A", "B", 10, 9, 5), edge("B", "A", 1, 1, 5)];
        // volume 5 below min_volume 100 => filtered.
        assert!(scan(&edges, 0.01, 100.0).is_empty());
    }

    #[test]
    fn needs_both_directions() {
        let edges = vec![edge("A", "B", 10, 9, 500)];
        assert!(scan(&edges, 0.0, 0.0).is_empty());
    }
}
