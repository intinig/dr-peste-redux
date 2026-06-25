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

/// Linear-interpolation percentile of an ascending-sorted slice. `p` in [0,1].
/// Matches the live ablation's percentile method so the fallback range reads
/// consistently with live prices.
pub(crate) fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    match sorted.len() {
        0 => 0.0,
        1 => sorted[0],
        n => {
            let rank = p * (n - 1) as f64;
            let lo = rank.floor() as usize;
            let hi = rank.ceil() as usize;
            sorted[lo] + (sorted[hi] - sorted[lo]) * (rank - lo as f64)
        }
    }
}

/// A corpus-derived price range (floor/fair/ask = p20/p50/p80) with band-width
/// confidence. The secondary `/paste` fallback when live ablation yields nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct RangeEstimate {
    pub floor: f64,
    pub fair: f64,
    pub ask: f64,
    pub confidence: Confidence,
    pub pool: usize,
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

    /// Range estimate from an exact-mod-set-first, adaptive-K comparable pool, or
    /// `None` (abstain) on a thin/dissimilar pool or a top-decile result. Pool
    /// membership is by mod-SET only (roll is not a price-shifter).
    pub fn range_estimate(&self, query: &[(String, Option<f64>)]) -> Option<RangeEstimate> {
        use crate::trade::value::{MIN_POOL, RELAX_JACCARD};
        use std::collections::HashSet;
        if self.items.is_empty() || query.is_empty() {
            return None;
        }
        let qset: HashSet<&str> = query.iter().map(|(s, _)| s.as_str()).collect();
        let jaccard = |it: &super::itemvec::ItemVector| -> f64 {
            let iset: HashSet<&str> = it.mods.iter().map(|(s, _)| s.as_str()).collect();
            let inter = qset.iter().filter(|s| iset.contains(**s)).count();
            let union = qset.len() + iset.len() - inter;
            if union == 0 {
                0.0
            } else {
                inter as f64 / union as f64
            }
        };
        // Exact mod-set first (Jaccard == 1.0); relax to Jaccard >= RELAX_JACCARD only
        // if the exact pool is thinner than MIN_POOL. Adaptive K: take ALL that qualify.
        let mut pool: Vec<f64> = self
            .items
            .iter()
            .filter(|it| jaccard(it) >= 1.0)
            .map(|it| it.price_divine)
            .collect();
        if pool.len() < MIN_POOL {
            pool = self
                .items
                .iter()
                .filter(|it| jaccard(it) >= RELAX_JACCARD)
                .map(|it| it.price_divine)
                .collect();
        }
        if pool.len() < MIN_POOL {
            return None; // abstain: no credible comparable pool
        }
        pool.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let floor = percentile_sorted(&pool, 0.20);
        let fair = percentile_sorted(&pool, 0.50);
        let ask = percentile_sorted(&pool, 0.80);
        if let Some(td) = self.top_decile_price {
            if fair >= td {
                return None; // abstain: corpus underprices the expensive tail → live
            }
        }
        let confidence = if floor > 0.0 && ask <= 2.0 * floor {
            Confidence::High
        } else if floor > 0.0 && ask <= 5.0 * floor {
            Confidence::Medium
        } else {
            Confidence::Low
        };
        Some(RangeEstimate {
            floor,
            fair,
            ask,
            confidence,
            pool: pool.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::value::itemvec::ItemVector;

    fn iv(stats: &[&str], price: f64) -> ItemVector {
        ItemVector {
            mods: stats.iter().map(|s| ((*s).to_string(), None)).collect(),
            price_divine: price,
        }
    }
    fn model_with(
        items: Vec<ItemVector>,
        top_decile: Option<f64>,
    ) -> crate::trade::value::CategoryModel {
        crate::trade::value::CategoryModel {
            items,
            top_decile_price: top_decile,
            ..Default::default()
        }
    }

    #[test]
    fn range_estimate_uses_exact_mod_set_pool() {
        let mut items: Vec<ItemVector> =
            (1..=10).map(|i| iv(&["a", "b"], i as f64 * 10.0)).collect();
        for _ in 0..10 {
            items.push(iv(&["c"], 1.0));
        }
        let m = model_with(items, None);
        let q = vec![("a".to_string(), None), ("b".to_string(), None)];
        let r = m.range_estimate(&q).expect("exact pool has >= MIN_POOL");
        assert!(
            r.floor >= 10.0 && r.ask <= 100.0 && r.floor < r.fair && r.fair < r.ask,
            "{r:?}"
        );
        assert_eq!(r.pool, 10, "only exact-mod-set items in the pool");
    }

    #[test]
    fn range_estimate_relaxes_when_exact_too_thin() {
        let mut items = vec![iv(&["a", "b", "c"], 50.0), iv(&["a", "b", "c"], 60.0)];
        for i in 0..10 {
            items.push(iv(&["a", "b"], 40.0 + i as f64));
        }
        items.push(iv(&["x"], 999.0));
        let m = model_with(items, None);
        let q = vec![("a".into(), None), ("b".into(), None), ("c".into(), None)];
        let r = m.range_estimate(&q).expect("relaxed pool has >= MIN_POOL");
        assert!(
            r.pool >= 8 && r.ask < 999.0,
            "relaxed pool excludes the J=0 item: {r:?}"
        );
    }

    #[test]
    fn range_estimate_abstains_on_thin_dissimilar_pool() {
        let items: Vec<ItemVector> = (0..20).map(|_| iv(&["a"], 5.0)).collect();
        let m = model_with(items, None);
        let q = vec![("zzz".into(), None)];
        assert!(
            m.range_estimate(&q).is_none(),
            "no credible comparable pool → abstain"
        );
    }

    #[test]
    fn range_estimate_abstains_on_top_decile() {
        let items: Vec<ItemVector> = (0..12).map(|_| iv(&["a", "b"], 500.0)).collect();
        let m = model_with(items, Some(400.0));
        let q = vec![("a".into(), None), ("b".into(), None)];
        assert!(
            m.range_estimate(&q).is_none(),
            "fair >= top decile → abstain, route to live"
        );
    }

    #[test]
    fn range_estimate_confidence_from_band_width() {
        let tight: Vec<ItemVector> = (0..12).map(|i| iv(&["a"], 100.0 + i as f64)).collect();
        let r = model_with(tight, None)
            .range_estimate(&[("a".into(), None)])
            .unwrap();
        assert_eq!(r.confidence, Confidence::High, "narrow band → High: {r:?}");
    }

    #[test]
    fn range_estimate_ignores_roll_for_pool_membership() {
        let items = vec![
            ItemVector {
                mods: vec![("a".into(), Some(0.1))],
                price_divine: 10.0,
            },
            ItemVector {
                mods: vec![("a".into(), Some(0.9))],
                price_divine: 20.0,
            },
        ];
        let mut all = items;
        for i in 0..10 {
            all.push(iv(&["a"], 12.0 + i as f64));
        }
        let m = model_with(all, None);
        let r = m
            .range_estimate(&[("a".into(), Some(0.5))])
            .expect("pool by mod-set, roll ignored");
        assert!(
            r.pool >= 12,
            "all {{a}} items pooled regardless of roll: {r:?}"
        );
    }

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
