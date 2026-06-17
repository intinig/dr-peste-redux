//! Maps individual stat lines into market "pseudo" aggregates (e.g. total
//! elemental resistance), which is how buyers actually search. Seeded from
//! `data/pseudo_map.json`; re-check each major patch.

use serde::Deserialize;

use crate::itemtext::ItemStat;

#[derive(Debug, Clone, Deserialize)]
pub struct PseudoPattern {
    pub text: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_weight() -> f64 {
    1.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct PseudoRule {
    pub pseudo_id: String,
    pub label: String,
    /// Substrings; a stat line matching the first pattern found contributes
    /// its value × that pattern's weight to the sum.
    pub patterns: Vec<PseudoPattern>,
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

    /// Sums each pseudo over all stat lines that match its patterns.  For each
    /// stat line the FIRST matching pattern's weight is used to scale the
    /// contribution (`value * weight`).  Pseudos with no matching lines
    /// (total == 0.0) are omitted from the result.
    pub fn resolve(&self, stats: &[ItemStat]) -> Vec<PseudoStat> {
        self.rules
            .iter()
            .filter_map(|rule| {
                let total: f64 = stats
                    .iter()
                    .filter_map(|s| {
                        rule.patterns
                            .iter()
                            .find(|p| s.raw.contains(p.text.as_str()))
                            .and_then(|p| s.value.map(|v| v * p.weight))
                    })
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

    /// True if any pseudo rule's patterns match this stat line — i.e. the line
    /// is already represented by a pseudo aggregate and should not be added as
    /// an individual filter.
    pub fn covers(&self, raw: &str) -> bool {
        self.rules
            .iter()
            .any(|r| r.patterns.iter().any(|p| raw.contains(p.text.as_str())))
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

    #[test]
    fn dual_resist_pattern_counts_double() {
        let map = PseudoMap::load();
        // "+20% to Fire and Lightning Resistance" should contribute 40 to total
        // elemental resistance (weight 2 × value 20).
        let stats = vec![stat("+20% to Fire and Lightning Resistance", 20.0)];
        let resolved = map.resolve(&stats);
        let ele = resolved
            .iter()
            .find(|p| p.id == "pseudo.pseudo_total_elemental_resistance")
            .unwrap();
        assert_eq!(ele.total, 40.0);
    }

    #[test]
    fn all_attributes_pattern_counts_triple() {
        let map = PseudoMap::load();
        // "+10 to all Attributes" should contribute 30 to total attributes
        // (weight 3 × value 10).
        let stats = vec![stat("+10 to all Attributes", 10.0)];
        let resolved = map.resolve(&stats);
        let attrs = resolved
            .iter()
            .find(|p| p.id == "pseudo.pseudo_total_attributes")
            .unwrap();
        assert_eq!(attrs.total, 30.0);
    }
}
