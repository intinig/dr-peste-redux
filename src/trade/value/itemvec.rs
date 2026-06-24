//! Per-category corpus item-vectors retained in the model for k-NN: each mod's
//! stat_id paired with its roll normalized to a percentile (None when the mod has
//! no rolled value).
use super::magnitude::RollStats;
use crate::observe::Observation;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ItemVector {
    pub mods: Vec<(String, Option<f64>)>,
    pub price_divine: f64,
}

pub fn build_item_vectors(
    obs: &[&Observation],
    mod_rolls: &HashMap<String, RollStats>,
) -> Vec<ItemVector> {
    obs.iter()
        .map(|o| ItemVector {
            mods: o
                .mods
                .iter()
                .map(|m| {
                    let norm = m
                        .roll
                        .and_then(|r| mod_rolls.get(&m.stat_id).map(|rs| rs.normalize(r)));
                    (m.stat_id.clone(), norm)
                })
                .collect(),
            price_divine: o.price_divine,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_vectors_carry_normalized_rolls() {
        use crate::observe::{Observation, Source};
        use crate::trade::model::ListingMod;
        let mk = |roll: f64, price: f64| Observation {
            timestamp_unix: 0,
            league: "L".into(),
            base_type: None,
            category: Some("Ring".into()),
            mods: vec![ListingMod {
                stat_id: "explicit.a".into(),
                tier: None,
                roll: Some(roll),
            }],
            price_divine: price,
            source: Source::Harvest,
            indexed: None,
        };
        let obs = [mk(10.0, 1.0), mk(30.0, 2.0), mk(50.0, 3.0)];
        let refs: Vec<&Observation> = obs.iter().collect();
        let mr = super::super::magnitude::build_mod_rolls(&refs);
        let vecs = build_item_vectors(&refs, &mr);
        assert_eq!(vecs.len(), 3);
        let hi = vecs.iter().find(|v| v.price_divine == 3.0).unwrap();
        assert!(
            (hi.mods[0].1.unwrap() - 1.0).abs() < 1e-9,
            "roll 50 → norm 1.0"
        );
    }
}
