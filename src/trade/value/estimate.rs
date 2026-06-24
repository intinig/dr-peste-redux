//! Similarity-weight parameters for the k-NN estimate (Task 6).
//! Stub: fields populated to zero by Default; training fills them.

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SimWeights {
    pub jaccard: f64,
    pub roll: f64,
}

use super::itemvec::ItemVector;
use std::collections::{HashMap, HashSet};

#[allow(dead_code)]
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

#[allow(dead_code)]
pub fn similarity(query: &[(String, Option<f64>)], item: &ItemVector, w: SimWeights) -> f64 {
    if query.is_empty() || item.mods.is_empty() {
        return 0.0;
    }
    let qset: HashSet<&str> = query.iter().map(|(s, _)| s.as_str()).collect();
    let iset: HashSet<&str> = item.mods.iter().map(|(s, _)| s.as_str()).collect();
    let inter = qset.intersection(&iset).count();
    let union = qset.union(&iset).count();
    let jac = if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    };

    // mod names are unique within an item/query (PoE invariant); last-wins is moot
    let qroll: HashMap<&str, f64> = query
        .iter()
        .filter_map(|(s, r)| r.map(|r| (s.as_str(), r)))
        .collect();
    let mut sum = 0.0;
    let mut n = 0usize;
    for (s, r) in &item.mods {
        if let (Some(item_roll), Some(query_roll)) = (r, qroll.get(s.as_str())) {
            sum += 1.0 - (query_roll - item_roll).abs();
            n += 1;
        }
    }
    let roll = if n == 0 { 0.0 } else { sum / n as f64 };
    let w = w.normalized();
    w.jaccard * jac + w.roll * roll
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
}
