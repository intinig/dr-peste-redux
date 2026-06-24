//! Similarity-weight parameters for the k-NN estimate (Task 6).
//! Stub: fields populated to zero by Default; training fills them.

#[derive(Debug, Clone, Copy, Default)]
pub struct SimWeights {
    pub jaccard: f64,
    pub roll: f64,
}

use super::itemvec::ItemVector;

impl SimWeights {
    pub fn normalized(self) -> SimWeights {
        let s = self.jaccard + self.roll;
        if s <= 0.0 {
            SimWeights {
                jaccard: 1.0,
                roll: 0.0,
            }
        } else {
            SimWeights {
                jaccard: self.jaccard / s,
                roll: self.roll / s,
            }
        }
    }
}

pub fn similarity(query: &[(String, Option<f64>)], item: &ItemVector, w: SimWeights) -> f64 {
    if query.is_empty() || item.mods.is_empty() {
        return 0.0;
    }
    // PoE invariant: mod names are unique within an item/query, so set sizes equal
    // slice lengths and a linear scan is exact. Items carry ≤6 mods, so this beats
    // allocating two HashSets + a HashMap on every call — and `similarity` is the
    // k-NN + LOO-backtest hot path (grid × probes × n calls per rebuild).
    let mut inter = 0usize;
    for (q, _) in query {
        if item.mods.iter().any(|(m, _)| m == q) {
            inter += 1;
        }
    }
    let union = query.len() + item.mods.len() - inter;
    let jac = if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    };

    let mut sum = 0.0;
    let mut n = 0usize;
    for (s, r) in &item.mods {
        if let Some(item_roll) = r {
            if let Some((_, Some(query_roll))) = query.iter().find(|(qs, _)| qs == s) {
                sum += 1.0 - (query_roll - item_roll).abs();
                n += 1;
            }
        }
    }
    let roll = if n == 0 { 0.0 } else { sum / n as f64 };
    let w = w.normalized();
    w.jaccard * jac + w.roll * roll
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone)]
pub struct ValueEstimate {
    pub value_divine: f64,
    pub confidence: Confidence,
    pub neighbors: usize,
}

/// Median of prices weighted by similarity. `scored` is (sim, price), sim>0.
/// `pub(crate)` so Task 6 (backtest) can reuse it directly.
pub(crate) fn weighted_median(scored: &[(f64, f64)]) -> f64 {
    let mut v: Vec<(f64, f64)> = scored.to_vec();
    v.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let total: f64 = v.iter().map(|(s, _)| *s).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let mut acc = 0.0;
    for (s, p) in &v {
        acc += s;
        if acc >= total / 2.0 {
            return *p;
        }
    }
    v.last().map(|(_, p)| *p).unwrap_or(0.0)
}

fn relative_spread(prices: &[f64], center: f64) -> f64 {
    if center <= 0.0 || prices.is_empty() {
        return f64::INFINITY;
    }
    let mut dev: Vec<f64> = prices.iter().map(|p| (p - center).abs()).collect();
    dev.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    dev[dev.len() / 2] / center // median abs deviation / center
}

impl crate::trade::value::CategoryModel {
    pub fn query_from_stats(&self, stats: &[(String, Option<f64>)]) -> Vec<(String, Option<f64>)> {
        stats
            .iter()
            .map(|(id, roll)| {
                let norm = roll.and_then(|r| self.mod_rolls.get(id).map(|rs| rs.normalize(r)));
                (id.clone(), norm)
            })
            .collect()
    }

    pub fn estimate(&self, query: &[(String, Option<f64>)]) -> Option<ValueEstimate> {
        if self.items.is_empty() {
            return None;
        }
        let mut scored: Vec<(f64, f64)> = self
            .items
            .iter()
            .map(|it| (similarity(query, it, self.weights), it.price_divine))
            .filter(|(s, _)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(super::K_NEIGHBORS);
        if scored.len() < super::MIN_NEIGHBORS {
            return None;
        }
        let value_divine = weighted_median(&scored);
        let top_sim = scored[0].0;
        let prices: Vec<f64> = scored.iter().map(|(_, p)| *p).collect();
        let spread = relative_spread(&prices, value_divine);
        let confidence = if scored.len() >= super::K_NEIGHBORS && top_sim >= 0.6 && spread <= 0.5 {
            Confidence::High
        } else if scored.len() >= super::MIN_NEIGHBORS * 2 && spread <= 1.0 {
            Confidence::Medium
        } else {
            Confidence::Low
        };
        Some(ValueEstimate {
            value_divine,
            confidence,
            neighbors: scored.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::value::itemvec::ItemVector;

    #[test]
    fn jaccard_weight_rewards_mod_overlap() {
        let item = ItemVector {
            mods: vec![("a".into(), None), ("b".into(), None)],
            price_divine: 1.0,
        };
        let w = SimWeights {
            jaccard: 1.0,
            roll: 0.0,
        };
        let full = similarity(&[("a".into(), None), ("b".into(), None)], &item, w);
        let half = similarity(&[("a".into(), None), ("c".into(), None)], &item, w);
        assert!((full - 1.0).abs() < 1e-9);
        assert!(full > half && half > 0.0);
    }

    #[test]
    fn roll_weight_rewards_roll_proximity_on_shared_mods() {
        let item = ItemVector {
            mods: vec![("a".into(), Some(0.9))],
            price_divine: 1.0,
        };
        let w = SimWeights {
            jaccard: 0.0,
            roll: 1.0,
        };
        let near = similarity(&[("a".into(), Some(0.85))], &item, w);
        let far = similarity(&[("a".into(), Some(0.1))], &item, w);
        assert!(near > far);
    }

    #[test]
    fn empty_query_or_no_shared_is_zero() {
        let item = ItemVector {
            mods: vec![("a".into(), Some(0.5))],
            price_divine: 1.0,
        };
        let w = SimWeights {
            jaccard: 0.5,
            roll: 0.5,
        }
        .normalized();
        assert_eq!(similarity(&[], &item, w), 0.0);
    }

    #[test]
    fn estimate_returns_weighted_median_of_neighbors() {
        use crate::trade::value::CategoryModel;
        let items = (0..10)
            .map(|i| ItemVector {
                mods: vec![("a".into(), Some(0.5)), ("b".into(), None)],
                price_divine: 100.0 + i as f64, // 100..109
            })
            .collect();
        let cat = CategoryModel {
            items,
            weights: SimWeights {
                jaccard: 1.0,
                roll: 0.0,
            },
            ..Default::default()
        };
        let est = cat
            .estimate(&[("a".into(), Some(0.5)), ("b".into(), None)])
            .expect("estimate");
        assert!(est.value_divine >= 100.0 && est.value_divine <= 109.0);
        assert!(est.neighbors >= super::super::MIN_NEIGHBORS);
    }

    #[test]
    fn estimate_none_when_too_few_neighbors() {
        use crate::trade::value::CategoryModel;
        let cat = CategoryModel {
            items: vec![ItemVector {
                mods: vec![("a".into(), None)],
                price_divine: 5.0,
            }],
            weights: SimWeights {
                jaccard: 1.0,
                roll: 0.0,
            },
            ..Default::default()
        };
        assert!(
            cat.estimate(&[("a".into(), None)]).is_none(),
            "1 neighbor < MIN_NEIGHBORS"
        );
    }

    #[test]
    fn query_normalizes_raw_rolls_via_mod_rolls() {
        use crate::trade::value::{magnitude::RollStats, CategoryModel};
        let mod_rolls = {
            let mut m = std::collections::HashMap::new();
            m.insert("a".into(), RollStats::from_rolls(&[0.0, 50.0, 100.0]));
            m
        };
        let cat = CategoryModel {
            mod_rolls,
            ..Default::default()
        };
        let q = cat.query_from_stats(&[("a".into(), Some(100.0)), ("b".into(), None)]);
        assert_eq!(q[0].0, "a");
        assert!((q[0].1.unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(q[1], ("b".into(), None)); // unknown mod → roll passes as None
    }
}
