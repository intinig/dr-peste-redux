//! Descriptive market ValueModel mined from the observation corpus: per-category
//! value-drivers (lift, top-decile frequency, co-occurrence) plus a deconfounded
//! ranking for `/insights`, and learned value fed back into the price-check.
//! Market data only — never any member secret.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::observe::{Observation, ObservationLog};

/// A category needs at least this many listings before it is "trusted" for
/// pricing feedback (insights still renders a thin-data note below it).
pub const MIN_CATEGORY_SAMPLE: usize = 50;
/// Periodic ValueModel rebuild interval, minutes.
pub const VALUE_REFRESH_MINS: u64 = 60;

/// Folds a category string to the canonical trade2 category text. The clipboard
/// `Item Class` is plural ("Staves") while the trade2 category is singular
/// ("Staff"); harvest already logs the trade2 text. The PoE item-class taxonomy
/// is a closed, known set, so this static map is a maintained artifact (re-check
/// after a major PoE2 patch). Unknown input passes through trimmed.
pub fn canonical_category(raw: &str) -> String {
    let key = raw.trim().to_lowercase();
    let canon = match key.as_str() {
        "staves" | "staff" => "Staff",
        "wands" | "wand" => "Wand",
        "sceptres" | "sceptre" => "Sceptre",
        "quarterstaves" | "quarterstaff" => "Quarterstaff",
        "bows" | "bow" => "Bow",
        "crossbows" | "crossbow" => "Crossbow",
        "amulets" | "amulet" => "Amulet",
        "rings" | "ring" => "Ring",
        "belts" | "belt" => "Belt",
        "body armours" | "body armour" => "Body Armour",
        "helmets" | "helmet" => "Helmet",
        "gloves" => "Gloves",
        "boots" => "Boots",
        "shields" | "shield" => "Shield",
        "foci" | "focus" => "Focus",
        "quivers" | "quiver" => "Quiver",
        _ => return raw.trim().to_string(),
    };
    canon.to_string()
}

/// Per-category descriptive value model. Keyed by canonical trade2 category text.
#[derive(Debug, Default, Clone)]
pub struct ValueModel {
    categories: HashMap<String, CategoryModel>,
}

/// Aggregated value signal for one category. Driver metrics are added in Task 2.
#[derive(Debug, Default, Clone)]
pub struct CategoryModel {
    pub category: String,
    pub sample_size: usize,
    pub base_median: f64,
}

impl ValueModel {
    pub fn category(&self, canon: &str) -> Option<&CategoryModel> {
        self.categories.get(canon)
    }

    /// Categories ordered by descending sample size (largest corpus first).
    pub fn categories_sorted(&self) -> Vec<&CategoryModel> {
        let mut v: Vec<&CategoryModel> = self.categories.values().collect();
        v.sort_by(|a, b| b.sample_size.cmp(&a.sample_size));
        v
    }

    pub fn build(observations: &[Observation]) -> ValueModel {
        // Group prices by canonical category (skip observations with no class).
        let mut by_cat: HashMap<String, Vec<f64>> = HashMap::new();
        for o in observations {
            let Some(raw) = o.category.as_deref() else {
                continue;
            };
            by_cat
                .entry(canonical_category(raw))
                .or_default()
                .push(o.price_divine);
        }
        let mut categories = HashMap::new();
        for (category, mut prices) in by_cat {
            prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let sample_size = prices.len();
            let base_median = median(&prices);
            categories.insert(
                category.clone(),
                CategoryModel {
                    category,
                    sample_size,
                    base_median,
                },
            );
        }
        ValueModel { categories }
    }
}

/// Rebuilds the ValueModel from the corpus and swaps it into `slot`. Best-effort:
/// reads are corrupt-line-tolerant; a poisoned lock is recovered, never panicked.
pub fn rebuild_into(log: &ObservationLog, slot: &RwLock<ValueModel>) {
    let model = ValueModel::build(&log.read_all());
    let n = model.categories.len();
    *slot.write().unwrap_or_else(|e| e.into_inner()) = model;
    tracing::info!(categories = n, "value model rebuilt");
}

/// Median of a slice. Sorts a copy; returns 0.0 for an empty slice.
fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_category_folds_clipboard_plurals() {
        assert_eq!(canonical_category("Staves"), "Staff");
        assert_eq!(canonical_category("staves"), "Staff"); // case-insensitive
        assert_eq!(canonical_category("Wands"), "Wand");
        assert_eq!(canonical_category("Amulets"), "Amulet");
        // Already-canonical trade2 text is idempotent.
        assert_eq!(canonical_category("Staff"), "Staff");
        // Unknown passes through trimmed.
        assert_eq!(canonical_category("  Fishing Rod  "), "Fishing Rod");
    }

    use crate::observe::{Observation, Source};
    use crate::trade::model::ListingMod;

    fn ob(category: &str, price: f64, stats: &[&str]) -> Observation {
        Observation {
            timestamp_unix: 0,
            league: "Standard".into(),
            base_type: Some("Chiming Staff".into()),
            category: Some(category.into()),
            mods: stats
                .iter()
                .map(|s| ListingMod {
                    stat_id: (*s).into(),
                    tier: None,
                    roll: None,
                })
                .collect(),
            price_divine: price,
            source: Source::Harvest,
        }
    }

    #[test]
    fn build_groups_by_canonical_category_with_base_median() {
        // "Staves" (paste) and "Staff" (harvest) must fold to one "Staff" group.
        let corpus = vec![
            ob("Staves", 1.0, &["explicit.a"]),
            ob("Staff", 3.0, &["explicit.a"]),
            ob("Staff", 5.0, &["explicit.b"]),
        ];
        let model = ValueModel::build(&corpus);
        let cat = model.category("Staff").expect("Staff category present");
        assert_eq!(cat.sample_size, 3);
        assert_eq!(cat.base_median, 3.0); // median of [1,3,5]
        assert!(model.category("Staves").is_none()); // folded, not a separate key
    }
}
