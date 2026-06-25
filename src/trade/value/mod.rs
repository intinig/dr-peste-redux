//! Descriptive market ValueModel mined from the observation corpus: per-category
//! value-drivers (lift, top-decile frequency, co-occurrence) plus a deconfounded
//! ranking for `/insights`, and learned value fed back into the price-check.
//! Market data only — never any member secret.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::observe::{Observation, ObservationLog};
use crate::trade::age::{is_fresh_at, now_unix, MAX_LISTING_AGE_DAYS};

/// A category needs at least this many listings before it is "trusted" for
/// pricing feedback (insights still renders a thin-data note below it).
pub const MIN_CATEGORY_SAMPLE: usize = 50;
/// Periodic ValueModel rebuild interval, minutes.
pub const VALUE_REFRESH_MINS: u64 = 60;
/// A stat needs at least this many listings before its lift is trusted (drives
/// pricing; gates the conditional-lift computation).
pub const MIN_STAT_SAMPLE: usize = 15;
/// A stat needs at least this many samples before its magnitude curve is trusted
/// (used by undersampled-gate detection).
pub const MAGNITUDE_MIN_SAMPLE: usize = 15;
/// A trusted stat with lift at or above this is a value-driver.
pub const DRIVER_LIFT: f64 = 1.5;
/// How many co-occurrence pairs to retain per category.
const TOP_COOCCURRENCE: usize = 8;
/// Number of evenly-spaced quantile knots stored per mod for roll-magnitude normalization.
pub const ROLL_QUANTILES: usize = 21;
/// Maximum number of nearest neighbours to consider for k-NN value estimate.
pub const K_NEIGHBORS: usize = 15;
/// Minimum neighbours required to emit a `ValueEstimate` (otherwise `None`).
pub const MIN_NEIGHBORS: usize = 5;
/// Minimum `sample_size` for a `CategoryModel` to be trusted by `learned_estimate`.
pub const TRUST_MIN_SAMPLE: usize = 80;
/// Relative divergence threshold between the learned corpus estimate and the
/// live trade price above which the embed flags a warning.
pub const DIVERGENCE_FLAG: f64 = 0.50;
/// Minimum comparable-pool size for a corpus range; below it, abstain.
#[allow(dead_code)] // Phase 1: used by CategoryModel::range_estimate; wired to /paste in Task 2.
pub const MIN_POOL: usize = 8;
/// When the exact-mod-set pool is thinner than MIN_POOL, relax to neighbours with at
/// least this Jaccard overlap of mod-sets.
#[allow(dead_code)] // Phase 1: used by CategoryModel::range_estimate; wired to /paste in Task 2.
pub const RELAX_JACCARD: f64 = 0.6;

pub mod backtest;
pub mod estimate;
pub mod gates;
pub mod itemvec;
pub mod magnitude;

/// Per-stat value signal within a category.
#[derive(Debug, Default, Clone)]
pub struct StatValue {
    pub stat_id: String,
    pub label: Option<String>,
    pub count: usize,
    pub median_with: f64,
    /// Marginal lift = median_with / median_without (falls back to base_median
    /// only when every listing carries the stat). Used by pricing feedback.
    pub lift: f64,
    /// Lift conditioned on the higher-ranked drivers being absent — deconfounded.
    /// `None` when the driver-free subset was too thin to compute. Insights only.
    pub conditional_lift: Option<f64>,
    pub top_decile_freq: f64,
}

/// A pair of stats frequently co-occurring on the expensive tail.
#[derive(Debug, Default, Clone)]
pub struct ModPair {
    pub a: String,
    pub b: String,
    pub count: usize,
}

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

/// Descriptive value model, partitioned by league then canonical trade2 category.
/// League keying keeps a prior-league or Standard harvest from polluting the
/// active league's drivers — observations carry their league, and pricing/insights
/// always look up the active league.
#[derive(Debug, Default, Clone)]
pub struct ValueModel {
    leagues: HashMap<String, HashMap<String, CategoryModel>>,
}

/// Aggregated value signal for one category. Includes per-stat driver metrics.
#[derive(Debug, Default, Clone)]
pub struct CategoryModel {
    pub category: String,
    pub sample_size: usize,
    pub base_median: f64,
    /// Stats in deconfounded rank order (drivers first).
    pub stats: Vec<StatValue>,
    pub cooccurrences: Vec<ModPair>,
    #[allow(dead_code)] // Phase 1: used by CategoryModel::estimate (learned k-NN path).
    pub mod_rolls: HashMap<String, magnitude::RollStats>,
    #[allow(dead_code)] // Phase 1: used by CategoryModel::estimate (learned k-NN path).
    pub items: Vec<itemvec::ItemVector>,
    #[allow(dead_code)] // Phase 1: used by CategoryModel::estimate (learned k-NN path).
    pub weights: estimate::SimWeights,
    pub undersampled_gates: Vec<gates::GateCandidate>,
    pub calibration: backtest::Calibration,
    /// p90 of this category's prices; the range estimator abstains when a query's
    /// `fair` lands at/above it (the corpus underprices the expensive tail).
    #[allow(dead_code)] // Phase 1: read by CategoryModel::range_estimate; surfaced in Task 2.
    pub top_decile_price: Option<f64>,
}

impl CategoryModel {
    /// Trusted value-drivers (high lift, enough samples), in deconfounded order.
    pub fn drivers(&self) -> impl Iterator<Item = &StatValue> {
        self.stats
            .iter()
            .filter(|s| s.count >= MIN_STAT_SAMPLE && s.lift >= DRIVER_LIFT)
    }

    /// A category's learned layer is trusted iff it has enough samples AND demonstrates
    /// positive skill over the no-feature (category-median) baseline. Replaces the old
    /// `loo_error <= 0.50` gate (which scored leaky, individual-listing error).
    pub fn is_trusted(&self) -> bool {
        self.sample_size >= TRUST_MIN_SAMPLE && self.calibration.skill.is_some_and(|s| s > 0.0)
    }
}

impl ValueModel {
    /// The model for one category within a league, if present.
    pub fn category(&self, league: &str, canon: &str) -> Option<&CategoryModel> {
        self.leagues.get(league)?.get(canon)
    }

    /// A league's categories ordered by descending sample size (largest first).
    /// Empty if the league has no observations yet.
    pub fn categories_sorted(&self, league: &str) -> Vec<&CategoryModel> {
        let mut v: Vec<&CategoryModel> = match self.leagues.get(league) {
            Some(cats) => cats.values().collect(),
            None => return Vec::new(),
        };
        v.sort_by_key(|c| std::cmp::Reverse(c.sample_size));
        v
    }

    pub fn build(
        observations: &[Observation],
        catalog: &crate::trade::stats::StatCatalog,
    ) -> ValueModel {
        // Group by league, then canonical category.
        let mut by_league: HashMap<String, HashMap<String, Vec<&Observation>>> = HashMap::new();
        for o in observations {
            let Some(raw) = o.category.as_deref() else {
                continue;
            };
            by_league
                .entry(o.league.clone())
                .or_default()
                .entry(canonical_category(raw))
                .or_default()
                .push(o);
        }
        let mut leagues = HashMap::new();
        for (league, by_cat) in by_league {
            let mut categories = HashMap::new();
            for (category, obs) in by_cat {
                categories.insert(category.clone(), build_category(category, &obs, catalog));
            }
            leagues.insert(league, categories);
        }
        ValueModel { leagues }
    }

    /// Test-only: build a model holding a single pre-constructed `CategoryModel`,
    /// keyed by `(league, canonical_category(category))`. Lets tests exercise the
    /// trust bar directly (e.g. a category that clears the sample-size gate but
    /// shows no positive skill in its `calibration`) without round-tripping a synthetic corpus.
    #[cfg(test)]
    pub(crate) fn with_category(league: &str, cat: CategoryModel) -> ValueModel {
        let canon = canonical_category(&cat.category);
        let mut categories = HashMap::new();
        categories.insert(canon, cat);
        let mut leagues = HashMap::new();
        leagues.insert(league.to_string(), categories);
        ValueModel { leagues }
    }
}

pub fn rebuild_into(
    log: &ObservationLog,
    slot: &RwLock<ValueModel>,
    catalog: &crate::trade::stats::StatCatalog,
) {
    let now = now_unix();
    let fresh: Vec<Observation> = log
        .read_all()
        .into_iter()
        // Learn only from rows that are (a) in the priceable band — sub-1-div dust
        // and absurd trolls carry no signal — and (b) positively dated as fresh.
        // Unlike the live path, the model treats an absent/unparseable timestamp as
        // NOT learnable (legacy pre-timestamp rows are cheap-biased), so a present,
        // parseable, in-window `indexed` is required.
        .filter(|o| {
            crate::trade::quality::is_priceable(o.price_divine)
                && o.indexed.as_deref().is_some_and(|t| {
                    crate::trade::age::parse_indexed(t).is_some()
                        && is_fresh_at(Some(t), now, MAX_LISTING_AGE_DAYS)
                })
        })
        .collect();
    let model = ValueModel::build(&fresh, catalog);
    let n: usize = model.leagues.values().map(HashMap::len).sum();
    *slot.write().unwrap_or_else(|e| e.into_inner()) = model;
    tracing::info!(categories = n, "value model rebuilt");
}

fn build_category(
    category: String,
    obs: &[&Observation],
    catalog: &crate::trade::stats::StatCatalog,
) -> CategoryModel {
    let sample_size = obs.len();
    let prices: Vec<f64> = obs.iter().map(|o| o.price_divine).collect();
    let base_median = median(&prices);

    // Distinct stats and the prices of listings carrying each.
    let mut prices_with: HashMap<&str, Vec<f64>> = HashMap::new();
    for o in obs {
        let mut seen = std::collections::HashSet::new();
        for m in &o.mods {
            if seen.insert(m.stat_id.as_str()) {
                prices_with
                    .entry(m.stat_id.as_str())
                    .or_default()
                    .push(o.price_divine);
            }
        }
    }

    // Top decile (most expensive ~10%, at least 1) for frequency + co-occurrence.
    let mut by_price: Vec<&&Observation> = obs.iter().collect();
    by_price.sort_by(|a, b| {
        b.price_divine
            .partial_cmp(&a.price_divine)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let decile_n = (sample_size as f64 * 0.10).ceil() as usize;
    let decile_n = decile_n.max(1).min(sample_size);
    let top: Vec<&&Observation> = by_price.into_iter().take(decile_n).collect();

    let mut top_count: HashMap<&str, usize> = HashMap::new();
    for o in &top {
        let mut seen = std::collections::HashSet::new();
        for m in &o.mods {
            if seen.insert(m.stat_id.as_str()) {
                *top_count.entry(m.stat_id.as_str()).or_default() += 1;
            }
        }
    }

    // Build a map of prices for listings WITHOUT each stat, for the "without"
    // denominator of lift. This computes lift as median(with) / median(without),
    // which correctly reflects the marginal value of having the stat.
    let mut prices_without: HashMap<&str, Vec<f64>> = HashMap::new();
    for o in obs {
        let present: std::collections::HashSet<&str> =
            o.mods.iter().map(|m| m.stat_id.as_str()).collect();
        for id in prices_with.keys() {
            if !present.contains(*id) {
                prices_without.entry(id).or_default().push(o.price_divine);
            }
        }
    }

    let mut stats: Vec<StatValue> = prices_with
        .iter()
        .map(|(id, with)| {
            let median_with = median(with);
            let without = prices_without.get(*id).map(|v| v.as_slice()).unwrap_or(&[]);
            let denom = if without.is_empty() {
                base_median
            } else {
                median(without)
            };
            let lift = if denom > 0.0 {
                median_with / denom
            } else {
                1.0
            };
            let top_decile_freq = *top_count.get(*id).unwrap_or(&0) as f64 / decile_n as f64;
            StatValue {
                stat_id: (*id).to_string(),
                label: catalog.label_for(id).map(str::to_owned),
                count: with.len(),
                median_with,
                lift,
                conditional_lift: None,
                top_decile_freq,
            }
        })
        .collect();

    // Co-occurrence pairs among the top decile (unordered, stable key order).
    let mut pair_count: HashMap<(String, String), usize> = HashMap::new();
    for o in &top {
        let mut ids: Vec<&str> = o.mods.iter().map(|m| m.stat_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                *pair_count
                    .entry((ids[i].to_string(), ids[j].to_string()))
                    .or_default() += 1;
            }
        }
    }
    let mut cooccurrences: Vec<ModPair> = pair_count
        .into_iter()
        .map(|((a, b), count)| ModPair { a, b, count })
        .collect();
    cooccurrences.sort_by(|x, y| {
        y.count
            .cmp(&x.count)
            .then(x.a.cmp(&y.a))
            .then(x.b.cmp(&y.b))
    });
    cooccurrences.truncate(TOP_COOCCURRENCE);

    // Deconfounded ranking fills conditional_lift + final order.
    rank_deconfounded(&mut stats, obs);

    let mod_rolls = magnitude::build_mod_rolls(obs);
    let items = itemvec::build_item_vectors(obs, &mod_rolls);
    let (weights, calibration) = backtest::tune_and_calibrate(&items);
    let undersampled_gates = gates::detect_gates(&stats);
    let top_decile_price = {
        let mut ps: Vec<f64> = items.iter().map(|it| it.price_divine).collect();
        if ps.is_empty() {
            None
        } else {
            ps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            Some(estimate::percentile_sorted(&ps, 0.90))
        }
    };

    CategoryModel {
        category,
        sample_size,
        base_median,
        stats,
        cooccurrences,
        mod_rolls,
        items,
        weights,
        undersampled_gates,
        calibration,
        top_decile_price,
    }
}

/// Greedy deconfounding: rank drivers so a mod that only co-travels with a
/// stronger driver is demoted. Picks the highest-lift trusted stat, then
/// recomputes remaining stats' lift restricted to listings carrying none of the
/// already-picked drivers, and repeats. Fills `conditional_lift` and reorders
/// `stats` (drivers first, in deconfounded order; the rest by raw lift after).
/// Used for /insights ranking only — pricing reads raw `lift`.
fn rank_deconfounded(stats: &mut Vec<StatValue>, obs: &[&Observation]) {
    let trusted = |s: &StatValue| s.count >= MIN_STAT_SAMPLE && s.lift >= DRIVER_LIFT;
    let mut picked: Vec<String> = Vec::new();
    let mut ordered: Vec<StatValue> = Vec::new();
    let mut remaining: Vec<StatValue> = std::mem::take(stats);

    loop {
        // Listings carrying none of the already-picked drivers.
        let subset: Vec<&&Observation> = obs
            .iter()
            .filter(|o| {
                !picked
                    .iter()
                    .any(|d| o.mods.iter().any(|m| &m.stat_id == d))
            })
            .collect();
        let subset_median = median(&subset.iter().map(|o| o.price_divine).collect::<Vec<_>>());

        // Best remaining trusted stat by conditional lift over the subset.
        // cl = median(subset_with_s) / median(subset_without_s), which measures
        // the stat's marginal contribution given the already-removed drivers.
        let mut best: Option<(usize, f64)> = None;
        for (i, s) in remaining.iter().enumerate() {
            if !trusted(s) {
                continue;
            }
            let with: Vec<f64> = subset
                .iter()
                .filter(|o| o.mods.iter().any(|m| m.stat_id == s.stat_id))
                .map(|o| o.price_divine)
                .collect();
            if with.len() < MIN_STAT_SAMPLE {
                continue;
            }
            let without: Vec<f64> = subset
                .iter()
                .filter(|o| !o.mods.iter().any(|m| m.stat_id == s.stat_id))
                .map(|o| o.price_divine)
                .collect();
            let denom = if without.is_empty() {
                subset_median
            } else {
                median(&without)
            };
            if denom <= 0.0 {
                continue;
            }
            let cl = median(&with) / denom;
            if best.is_none_or(|(_, bcl)| cl > bcl) {
                best = Some((i, cl));
            }
        }

        match best {
            Some((i, cl)) if cl >= DRIVER_LIFT => {
                let mut s = remaining.remove(i);
                s.conditional_lift = Some(cl);
                picked.push(s.stat_id.clone());
                ordered.push(s);
            }
            _ => break, // no remaining trusted stat clears the bar over the subset
        }
    }

    // Final subset after all drivers are extracted — used to assign conditional_lift
    // to any remaining trusted-by-raw-lift stats (co-travelers) so callers can see
    // their collapsed independent contribution.
    let final_subset: Vec<&&Observation> = obs
        .iter()
        .filter(|o| {
            !picked
                .iter()
                .any(|d| o.mods.iter().any(|m| &m.stat_id == d))
        })
        .collect();
    let final_median = median(
        &final_subset
            .iter()
            .map(|o| o.price_divine)
            .collect::<Vec<_>>(),
    );

    for s in &mut remaining {
        if !trusted(s) {
            continue;
        }
        let with: Vec<f64> = final_subset
            .iter()
            .filter(|o| o.mods.iter().any(|m| m.stat_id == s.stat_id))
            .map(|o| o.price_divine)
            .collect();
        if !with.is_empty() {
            // Marginal lift in the driver-free residual: median(with) / median(without),
            // consistent with the greedy loop (not divided by the whole-subset median).
            let without_final: Vec<f64> = final_subset
                .iter()
                .filter(|o| !o.mods.iter().any(|m| m.stat_id == s.stat_id))
                .map(|o| o.price_divine)
                .collect();
            let denom = if without_final.is_empty() {
                final_median
            } else {
                median(&without_final)
            };
            if denom > 0.0 {
                s.conditional_lift = Some(median(&with) / denom);
            }
        } else if !picked.is_empty() {
            // Stat only co-appears with confirmed drivers — its independent
            // contribution is effectively 1 (no evidence it adds value alone).
            s.conditional_lift = Some(1.0);
        }
    }

    // Append the rest by descending raw lift.
    remaining.sort_by(|a, b| {
        b.lift
            .partial_cmp(&a.lift)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ordered.extend(remaining);
    *stats = ordered;
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
            indexed: None,
        }
    }

    #[test]
    fn rebuild_into_drops_stale_observations() {
        // 20 fresh + 5 ancient rows in one category. rebuild_into must learn only
        // from the fresh ones, so the ancient (stale-priced) rows can't bias drivers
        // or inflate the sample.
        let dir = tempfile::tempdir().unwrap();
        let log = ObservationLog::new(dir.path().join("obs.jsonl"));
        for _ in 0..20 {
            log.append(&Observation {
                indexed: Some("2099-01-01T00:00:00Z".into()), // future → always fresh
                ..ob("Staff", 10.0, &["explicit.a"])
            })
            .unwrap();
        }
        for _ in 0..5 {
            log.append(&Observation {
                indexed: Some("2000-01-01T00:00:00Z".into()), // ancient → always stale
                ..ob("Staff", 999.0, &["explicit.a"])
            })
            .unwrap();
        }
        let slot = RwLock::new(ValueModel::default());
        rebuild_into(&log, &slot, &crate::trade::stats::StatCatalog::default());
        let model = slot.read().unwrap();
        let cat = model
            .category("Standard", "Staff")
            .expect("Staff category present");
        assert_eq!(
            cat.sample_size, 20,
            "only fresh rows counted; ancient dropped"
        );
        assert_eq!(cat.base_median, 10.0, "ancient 999-div outliers excluded");
    }

    #[test]
    fn build_groups_by_canonical_category_with_base_median() {
        // "Staves" (paste) and "Staff" (harvest) must fold to one "Staff" group.
        let corpus = vec![
            ob("Staves", 1.0, &["explicit.a"]),
            ob("Staff", 3.0, &["explicit.a"]),
            ob("Staff", 5.0, &["explicit.b"]),
        ];
        let model = ValueModel::build(&corpus, &crate::trade::stats::StatCatalog::default());
        let cat = model
            .category("Standard", "Staff")
            .expect("Staff category present");
        assert_eq!(cat.sample_size, 3);
        assert_eq!(cat.base_median, 3.0); // median of [1,3,5]
        assert!(model.category("Standard", "Staves").is_none()); // folded, not a separate key
    }

    #[test]
    fn build_partitions_by_league() {
        // Same category, two leagues: a driver in one league must not leak to the
        // other. League "Old" has a strong driver "drv"; league "New" does not.
        let mut corpus = Vec::new();
        for _ in 0..30 {
            corpus.push(Observation {
                league: "Old".into(),
                ..ob("Staff", 10.0, &["drv"])
            });
        }
        for _ in 0..30 {
            corpus.push(Observation {
                league: "New".into(),
                ..ob("Staff", 1.0, &["other"])
            });
        }
        let model = ValueModel::build(&corpus, &crate::trade::stats::StatCatalog::default());
        // Each league has its own Staff model.
        assert_eq!(model.category("Old", "Staff").unwrap().sample_size, 30);
        assert_eq!(model.category("New", "Staff").unwrap().sample_size, 30);
        // The "Old" driver does not appear in "New".
        assert!(model
            .category("Old", "Staff")
            .unwrap()
            .stats
            .iter()
            .any(|s| s.stat_id == "drv"));
        assert!(!model
            .category("New", "Staff")
            .unwrap()
            .stats
            .iter()
            .any(|s| s.stat_id == "drv"));
        // categories_sorted is league-scoped.
        assert_eq!(model.categories_sorted("Old").len(), 1);
        assert!(model.categories_sorted("Nonexistent").is_empty());
    }

    #[test]
    fn build_recovers_a_planted_driver() {
        // Category base is cheap; listings carrying "drv" are expensive.
        let mut corpus = Vec::new();
        for _ in 0..40 {
            corpus.push(ob("Staff", 1.0, &["filler"]));
        }
        for _ in 0..40 {
            corpus.push(ob("Staff", 10.0, &["drv", "filler"]));
        }
        let model = ValueModel::build(&corpus, &crate::trade::stats::StatCatalog::default());
        let cat = model.category("Standard", "Staff").unwrap();
        let drv = cat.stats.iter().find(|s| s.stat_id == "drv").unwrap();
        assert_eq!(drv.count, 40);
        assert!(
            drv.lift > 1.5,
            "driver lift should be well above 1: {}",
            drv.lift
        );
        assert!(
            drv.top_decile_freq > 0.9,
            "driver should dominate the expensive tail"
        );
        // "drv" is a value-driver; "filler" (on everything) is not.
        assert!(cat.drivers().any(|s| s.stat_id == "drv"));
        assert!(!cat.drivers().any(|s| s.stat_id == "filler"));
    }

    #[test]
    fn rebuild_into_drops_sub_one_div_and_undated_observations() {
        let dir = tempfile::tempdir().unwrap();
        let log = ObservationLog::new(dir.path().join("obs.jsonl"));
        // 15 clean, fresh, in-band rows → kept.
        for _ in 0..15 {
            log.append(&Observation {
                indexed: Some("2099-01-01T00:00:00Z".into()), // future → always fresh
                ..ob("Staff", 30.0, &["explicit.a"])
            })
            .unwrap();
        }
        // 5 fresh but sub-1-div rows → dropped by is_priceable.
        for _ in 0..5 {
            log.append(&Observation {
                indexed: Some("2099-01-01T00:00:00Z".into()),
                ..ob("Staff", 0.5, &["explicit.a"])
            })
            .unwrap();
        }
        // 7 undated rows (ob() defaults indexed: None) → dropped by timestamp-required rule.
        for _ in 0..7 {
            log.append(&ob("Staff", 30.0, &["explicit.a"])).unwrap();
        }
        let slot = RwLock::new(ValueModel::default());
        rebuild_into(&log, &slot, &crate::trade::stats::StatCatalog::default());
        let model = slot.read().unwrap();
        let cat = model
            .category("Standard", "Staff")
            .expect("Staff category present");
        assert_eq!(
            cat.sample_size, 15,
            "only clean, fresh, in-band, dated rows are learned from"
        );
    }

    #[test]
    fn deconfounding_collapses_a_co_traveler() {
        // A: genuine driver (expensive with or without B). B: rides A only.
        let mut corpus = Vec::new();
        for _ in 0..30 {
            corpus.push(ob("Staff", 1.0, &["base"])); // cheap baseline
        }
        for _ in 0..30 {
            corpus.push(ob("Staff", 10.0, &["A", "B", "base"])); // A and B together, expensive
        }
        for _ in 0..30 {
            corpus.push(ob("Staff", 10.0, &["A", "base"])); // A alone, still expensive
        }
        // B never appears without A, and contributes nothing on its own.
        let model = ValueModel::build(&corpus, &crate::trade::stats::StatCatalog::default());
        let cat = model.category("Standard", "Staff").unwrap();
        let a = cat.stats.iter().find(|s| s.stat_id == "A").unwrap();
        let b = cat.stats.iter().find(|s| s.stat_id == "B").unwrap();
        // Both look strong univariately…
        assert!(a.lift > 1.5 && b.lift > 1.5);
        // …but B's independent (conditional) lift collapses to ~1, A's stays high.
        assert!(a.conditional_lift.unwrap() > 5.0);
        assert!(
            b.conditional_lift.unwrap() < 1.3,
            "co-traveler should deconfound to ~1: {:?}",
            b.conditional_lift
        );
        // Deconfounded ranking puts A ahead of B.
        let pos = |id: &str| cat.stats.iter().position(|s| s.stat_id == id).unwrap();
        assert!(pos("A") < pos("B"));
    }

    #[test]
    fn is_trusted_requires_sample_and_positive_skill() {
        let mk = |n: usize, skill: Option<f64>| CategoryModel {
            sample_size: n,
            calibration: backtest::Calibration {
                model_err: Some(0.7),
                baseline_err: Some(0.8),
                skill,
            },
            ..Default::default()
        };
        assert!(
            mk(100, Some(0.15)).is_trusted(),
            "enough samples + positive skill"
        );
        assert!(
            !mk(100, Some(0.0)).is_trusted(),
            "zero skill is not trusted"
        );
        assert!(
            !mk(100, Some(-0.2)).is_trusted(),
            "negative skill is not trusted"
        );
        assert!(
            !mk(10, Some(0.5)).is_trusted(),
            "under-sampled is not trusted"
        );
        assert!(!mk(100, None).is_trusted(), "no calibration is not trusted");
    }
}
