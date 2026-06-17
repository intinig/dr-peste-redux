//! Maps individual stat lines into market "pseudo" aggregates (e.g. total
//! elemental resistance), which is how buyers actually search. Seeded from
//! `data/pseudo_map.json`; re-check each major patch.

use serde::Deserialize;

use crate::itemtext::ItemStat;

#[derive(Debug, Clone, Deserialize)]
pub struct PseudoRule {
    pub pseudo_id: String,
    pub label: String,
    /// Substrings; a stat line matching any contributes its value to the sum.
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PseudoMap {
    pub rules: Vec<PseudoRule>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PseudoStat {
    pub id: String,
    pub label: String,
    pub total: f64,
}

impl PseudoMap {
    /// Loads the committed seed map. Panics only on a malformed committed file
    /// (a build-time bug, caught by tests), never at runtime on user input.
    pub fn load() -> Self {
        let rules: Vec<PseudoRule> = serde_json::from_str(include_str!("data/pseudo_map.json"))
            .expect("pseudo_map.json is valid");
        PseudoMap { rules }
    }

    /// Sums each pseudo over all stat lines that match its patterns. Pseudos
    /// with no matching lines (total == 0.0) are omitted from the result.
    pub fn resolve(&self, stats: &[ItemStat]) -> Vec<PseudoStat> {
        self.rules
            .iter()
            .filter_map(|rule| {
                let total: f64 = stats
                    .iter()
                    .filter(|s| rule.patterns.iter().any(|p| s.raw.contains(p.as_str())))
                    .filter_map(|s| s.value)
                    .sum();
                if total > 0.0 {
                    Some(PseudoStat {
                        id: rule.pseudo_id.clone(),
                        label: rule.label.clone(),
                        total,
                    })
                } else {
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::itemtext::ItemStat;

    fn stat(raw: &str, v: f64) -> ItemStat {
        ItemStat {
            raw: raw.to_string(),
            value: Some(v),
        }
    }

    #[test]
    fn sums_elemental_resistances_across_lines() {
        let map = PseudoMap::load();
        let stats = vec![
            stat("+32% to Fire Resistance", 32.0),
            stat("+18% to Lightning Resistance", 18.0),
            stat("+40 to maximum Life", 40.0),
        ];
        let resolved = map.resolve(&stats);
        let ele = resolved
            .iter()
            .find(|p| p.id == "pseudo.pseudo_total_elemental_resistance")
            .unwrap();
        assert_eq!(ele.total, 50.0);
        let life = resolved
            .iter()
            .find(|p| p.id == "pseudo.pseudo_total_life")
            .unwrap();
        assert_eq!(life.total, 40.0);
    }

    #[test]
    fn omits_pseudos_with_no_matching_lines() {
        let map = PseudoMap::load();
        let resolved = map.resolve(&[stat("+10 to Strength", 10.0)]);
        assert!(resolved.iter().all(|p| p.id != "pseudo.pseudo_total_life"));
        assert!(resolved
            .iter()
            .any(|p| p.id == "pseudo.pseudo_total_attributes"));
    }
}
