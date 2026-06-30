//! Directed rate graph + bounded profitable-cycle enumeration (triangulation).
//! Pure, no I/O. A profitable cycle's leg ratios compound to > 1.
//!
//! ## Math
//!
//! For a cycle `c0 -> c1 -> ... -> c0` with leg ratios `r_i = get_i / pay_i`,
//! the gross multiplier is `M = Π r_i`.
//!
//! Per unit of `c0` input, the amount entering leg `i` is
//! `P_i = r_0 · … · r_{i-1}` (so `P_0 = 1`).
//! Leg `i` produces `P_i · X · r_i` units of `c_{i+1}`, capped by `stock_i`
//! → `X ≤ stock_i / (P_i · r_i) = stock_i / P_{i+1}`.
//! Therefore `feasible_volume = min_i ( stock_i / P_{i+1} )`, in units of `c0`.

use crate::arb::model::{Currency, Edge, Leg, RatioQuote};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct CycleResult {
    pub legs: Vec<Leg>,
    pub multiplier: f64,
    pub feasible_volume: f64,
}

pub struct RateGraph {
    /// from -> [(to, quote)]
    adj: HashMap<Currency, Vec<(Currency, RatioQuote)>>,
    nodes: Vec<Currency>,
}

impl RateGraph {
    pub fn from_edges(edges: &[Edge]) -> RateGraph {
        let mut adj: HashMap<Currency, Vec<(Currency, RatioQuote)>> = HashMap::new();
        let mut nodes: Vec<Currency> = Vec::new();
        for e in edges {
            for c in [&e.from, &e.to] {
                if !nodes.iter().any(|n| n == c) {
                    nodes.push(c.clone());
                }
            }
            adj.entry(e.from.clone())
                .or_default()
                .push((e.to.clone(), e.quote.clone()));
        }
        RateGraph { adj, nodes }
    }

    pub fn profitable_cycles(&self, max_len: usize, min_profit: f64) -> Vec<CycleResult> {
        let mut found: Vec<CycleResult> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Start a DFS from each node; only keep cycles that return to the start.
        for start in &self.nodes {
            let mut path: Vec<Leg> = Vec::new();
            self.dfs(
                start, start, max_len, min_profit, &mut path, &mut found, &mut seen,
            );
        }
        found.sort_by(|a, b| {
            b.multiplier
                .partial_cmp(&a.multiplier)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        found
    }

    #[allow(clippy::too_many_arguments)]
    fn dfs(
        &self,
        start: &str,
        current: &str,
        max_len: usize,
        min_profit: f64,
        path: &mut Vec<Leg>,
        out: &mut Vec<CycleResult>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        if path.len() >= max_len {
            return;
        }
        let Some(neighbors) = self.adj.get(current) else {
            return;
        };
        for (to, quote) in neighbors {
            // Closing the cycle back to start.
            if to.as_str() == start {
                if path.len() + 1 < 3 {
                    continue; // triangulation is length >= 3
                }
                let mut legs = path.clone();
                legs.push(Leg {
                    from: current.to_string(),
                    to: to.clone(),
                    quote: quote.clone(),
                });
                if let Some(res) = evaluate_cycle(&legs) {
                    if res.multiplier > 1.0 + min_profit {
                        let key = canonical_key(&legs);
                        if seen.insert(key) {
                            out.push(res);
                        }
                    }
                }
                continue;
            }
            // Avoid revisiting a node already on the path (simple cycles only).
            if path.iter().any(|l| l.from.as_str() == to.as_str()) {
                continue;
            }
            path.push(Leg {
                from: current.to_string(),
                to: to.clone(),
                quote: quote.clone(),
            });
            self.dfs(start, to, max_len, min_profit, path, out, seen);
            path.pop();
        }
    }
}

/// Multiplier and stock-bottleneck feasible volume for a closed cycle.
fn evaluate_cycle(legs: &[Leg]) -> Option<CycleResult> {
    if legs.len() < 3 {
        return None;
    }
    let mut multiplier = 1.0f64;
    let mut feasible = f64::INFINITY;
    // P_{i+1} = product of ratios up to and including leg i.
    for leg in legs {
        let r = leg.quote.ratio();
        multiplier *= r;
        let p_next = multiplier; // P_{i+1}
        let cap = leg.quote.stock as f64 / p_next;
        if cap < feasible {
            feasible = cap;
        }
    }
    Some(CycleResult {
        legs: legs.to_vec(),
        multiplier,
        feasible_volume: feasible,
    })
}

/// Rotation-invariant key so the same cycle discovered from different start
/// nodes is deduped. Uses the currency sequence rotated to its lexicographic min.
fn canonical_key(legs: &[Leg]) -> String {
    let seq: Vec<&str> = legs.iter().map(|l| l.from.as_str()).collect();
    let n = seq.len();
    let min_idx = (0..n).min_by_key(|&i| seq[i]).unwrap_or(0);
    let rotated: Vec<&str> = (0..n).map(|k| seq[(min_idx + k) % n]).collect();
    rotated.join(">")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arb::model::Freshness;

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
    fn finds_profitable_triangle() {
        // A->B 1:2, B->C 1:2, C->A 1:0.3 => M = 2*2*0.3 = 1.2 (+20%)
        let edges = vec![
            edge("A", "B", 1, 2, 1000),
            edge("B", "C", 1, 2, 1000),
            edge("C", "A", 10, 3, 1000),
        ];
        let g = RateGraph::from_edges(&edges);
        let cycles = g.profitable_cycles(4, 0.0);
        assert_eq!(cycles.len(), 1);
        assert!((cycles[0].multiplier - 1.2).abs() < 1e-9);
    }

    #[test]
    fn ignores_unprofitable_and_two_cycles() {
        // A<->B round trip loses (no triangle); must yield nothing.
        let edges = vec![edge("A", "B", 1, 2, 100), edge("B", "A", 2, 1, 100)];
        let g = RateGraph::from_edges(&edges);
        assert!(g.profitable_cycles(4, 0.0).is_empty());
    }

    #[test]
    fn feasible_volume_is_bottleneck() {
        // Same +20% triangle but C->A stock limits throughput.
        // P after legs: P1=2, P2=4, P3=1.2. caps: 1000/2=500, 1000/4=250, stock/1.2.
        // Set C->A stock=120 => cap=100 => bottleneck 100.
        let edges = vec![
            edge("A", "B", 1, 2, 1000),
            edge("B", "C", 1, 2, 1000),
            edge("C", "A", 10, 3, 120),
        ];
        let g = RateGraph::from_edges(&edges);
        let c = &g.profitable_cycles(4, 0.0)[0];
        assert!(
            (c.feasible_volume - 100.0).abs() < 1e-6,
            "got {}",
            c.feasible_volume
        );
    }

    #[test]
    fn dedups_rotations() {
        let edges = vec![
            edge("A", "B", 1, 2, 1000),
            edge("B", "C", 1, 2, 1000),
            edge("C", "A", 10, 3, 1000),
        ];
        let g = RateGraph::from_edges(&edges);
        // Even though DFS starts from A, B, and C, the cycle appears once.
        assert_eq!(g.profitable_cycles(4, 0.0).len(), 1);
    }
}
