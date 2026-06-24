//! Leave-one-out calibration: report per-category prediction error and pick the
//! similarity weights that minimize it (so each category self-selects whether
//! combination or roll-magnitude drives value).
use super::estimate::{similarity, weighted_median, SimWeights};
use super::itemvec::ItemVector;
use super::{K_NEIGHBORS, MIN_NEIGHBORS};

const WEIGHT_GRID: [(f64, f64); 5] = [
    (1.0, 0.0),
    (0.75, 0.25),
    (0.5, 0.5),
    (0.25, 0.75),
    (0.0, 1.0),
];

/// Cap on leave-one-out held-out probes per category. The neighbour search still
/// scans ALL items; evaluating error on an evenly-spaced subset keeps calibration at
/// O(grid × probes × n) instead of O(grid × n²) — bounding rebuild cost (notably the
/// synchronous startup rebuild) as the corpus grows. Categories with ≤ this many
/// items probe every item, so small-category behaviour is unchanged.
const LOO_MAX_PROBES: usize = 400;

/// How many held-out probes to evaluate for a corpus of `n` items: all of them up to
/// the cap, then exactly `LOO_MAX_PROBES`. The probes are spread evenly across the
/// whole corpus (see `loo_median_error`), so we never drop nearly half the data at the
/// cap boundary the way a `ceil(n / cap)` stride would (e.g. n=401 → stride 2 → 201).
fn loo_probe_count(n: usize) -> usize {
    n.min(LOO_MAX_PROBES)
}

/// A stable signature of an item's mod-SET (the set of stat_ids, order-independent),
/// used to leave the probe's whole exact-mod-set group out of its own evaluation.
fn mod_keys(items: &[ItemVector]) -> Vec<String> {
    items
        .iter()
        .map(|it| {
            let mut ids: Vec<&str> = it.mods.iter().map(|(s, _)| s.as_str()).collect();
            ids.sort_unstable();
            ids.join("\u{1}")
        })
        .collect()
}

fn median_sorted(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(v[v.len() / 2])
}

/// k-NN prediction for the probe, EXCLUDING every item sharing the probe's exact mod-set
/// (self-exclusion: `keys[i] != keys[skip]`), not just the probe itself.
fn predict_one(items: &[ItemVector], keys: &[String], skip: usize, w: SimWeights) -> Option<f64> {
    let q: Vec<(String, Option<f64>)> = items[skip].mods.clone();
    let mut scored: Vec<(f64, f64)> = items
        .iter()
        .enumerate()
        .filter(|(i, _)| keys[*i] != keys[skip])
        .map(|(_, it)| (similarity(&q, it, w), it.price_divine))
        .filter(|(s, _)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(K_NEIGHBORS);
    if scored.len() < MIN_NEIGHBORS {
        return None;
    }
    Some(weighted_median(&scored))
}

/// Evenly-spaced probe indices across [0, n): `k·n/probes` (bounded by LOO_MAX_PROBES,
/// spread across the whole corpus — see the original loo_median_error note).
fn probe_indices(n: usize) -> Vec<usize> {
    let probes = loo_probe_count(n);
    (0..probes).map(|k| k * n / probes).collect()
}

/// Median self-excluded relative error of the k-NN over the probe set.
fn model_error(items: &[ItemVector], keys: &[String], w: SimWeights) -> Option<f64> {
    let mut errs: Vec<f64> = Vec::new();
    for &i in &probe_indices(items.len()) {
        let actual = items[i].price_divine;
        if actual > 0.0 {
            if let Some(pred) = predict_one(items, keys, i, w) {
                errs.push((pred - actual).abs() / actual);
            }
        }
    }
    if errs.len() < MIN_NEIGHBORS {
        return None;
    }
    median_sorted(&mut errs)
}

/// Median relative error of the NO-FEATURE baseline: predict each probe by the median
/// price of all items EXCEPT the probe's mod-set group (same self-exclusion as the model).
fn baseline_error(items: &[ItemVector], keys: &[String]) -> Option<f64> {
    let mut errs: Vec<f64> = Vec::new();
    for &i in &probe_indices(items.len()) {
        let actual = items[i].price_divine;
        if actual <= 0.0 {
            continue;
        }
        let mut others: Vec<f64> = items
            .iter()
            .enumerate()
            .filter(|(j, _)| keys[*j] != keys[i])
            .map(|(_, it)| it.price_divine)
            .collect();
        if let Some(m) = median_sorted(&mut others) {
            errs.push((m - actual).abs() / actual);
        }
    }
    if errs.is_empty() {
        return None;
    }
    median_sorted(&mut errs)
}

/// Per-category calibration: model error, no-feature baseline error, and skill =
/// fraction of baseline error the model removes (`> 0` ⇒ beats guessing the median).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Calibration {
    pub model_err: Option<f64>,
    pub baseline_err: Option<f64>,
    pub skill: Option<f64>,
}

/// Pick similarity weights by minimizing self-excluded model error, then compute the
/// baseline error and skill over the same probe set.
pub fn tune_and_calibrate(items: &[ItemVector]) -> (SimWeights, Calibration) {
    let keys = mod_keys(items);
    let mut best_w = SimWeights {
        jaccard: 1.0,
        roll: 0.0,
    };
    let mut model_err: Option<f64> = None;
    for (j, r) in WEIGHT_GRID {
        let w = SimWeights {
            jaccard: j,
            roll: r,
        };
        if let Some(e) = model_error(items, &keys, w) {
            if model_err.map(|b| e < b).unwrap_or(true) {
                model_err = Some(e);
                best_w = w;
            }
        }
    }
    let baseline_err = baseline_error(items, &keys);
    let skill = match (model_err, baseline_err) {
        (Some(m), Some(b)) if b > 0.0 => Some((b - m) / b),
        _ => None,
    };
    (
        best_w,
        Calibration {
            model_err,
            baseline_err,
            skill,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_exclusion_drops_same_mod_set_siblings() {
        // Probe at index 0 (mods {a}, price 100). One sibling (also {a}, price 100) would
        // make a self-included predictor return 100 (0 error). All OTHER items share NO mods
        // with the probe (mods {b}, similarity 0) → after self-excluding the {a} group there
        // are no positive-similarity neighbours, so predict_one returns None and the probe
        // contributes nothing. Proves siblings are excluded, not used.
        let mut items = vec![
            ItemVector {
                mods: vec![("a".into(), None)],
                price_divine: 100.0,
            },
            ItemVector {
                mods: vec![("a".into(), None)],
                price_divine: 100.0,
            },
        ];
        for _ in 0..20 {
            items.push(ItemVector {
                mods: vec![("b".into(), None)],
                price_divine: 1.0,
            });
        }
        let keys = mod_keys(&items);
        assert!(
            predict_one(
                &items,
                &keys,
                0,
                SimWeights {
                    jaccard: 1.0,
                    roll: 0.0
                }
            )
            .is_none(),
            "same-mod-set siblings must be excluded, leaving no similar neighbours"
        );
    }

    #[test]
    fn skill_positive_when_model_beats_median() {
        // Two well-separated mod-set groups at very different prices, each with enough members
        // that leave-the-group-out still leaves the OTHER group as neighbours? No — the k-NN
        // would have no same-set neighbour. Instead: price is a smooth function of a shared
        // mod's roll, so roll-proximity (within the kept neighbours) predicts far better than
        // the global median. The grid will pick roll weight and skill must be > 0.
        let items: Vec<ItemVector> = (0..60)
            .map(|i| {
                let r = i as f64 / 59.0;
                // distinct mod-set per item (so self-exclusion removes only itself), shared mod "a"
                // carries the price signal via roll; a unique tag mod makes each mod-set unique.
                ItemVector {
                    mods: vec![("a".into(), Some(r)), (format!("tag{i}"), None)],
                    price_divine: 10.0 + 100.0 * r,
                }
            })
            .collect();
        let (_w, cal) = tune_and_calibrate(&items);
        assert!(
            cal.skill.unwrap() > 0.0,
            "model tracks roll → beats median baseline; skill={:?}",
            cal.skill
        );
    }

    #[test]
    fn skill_non_positive_when_no_signal() {
        // Price is independent of mods (random-ish but deterministic), each item a unique
        // mod-set. The k-NN cannot beat predicting the median → skill <= 0 (or None).
        let items: Vec<ItemVector> = (0..60)
            .map(|i| ItemVector {
                mods: vec![(format!("m{i}"), None)],
                price_divine: 1.0 + (i % 7) as f64,
            })
            .collect();
        let (_w, cal) = tune_and_calibrate(&items);
        assert!(
            cal.skill.map(|s| s <= 0.0).unwrap_or(true),
            "no signal → skill<=0/None; got {:?}",
            cal.skill
        );
    }

    #[test]
    fn tune_picks_roll_weight_for_magnitude_dominant_corpus() {
        // Price depends ONLY on mod "a"'s roll. Even-indexed items additionally
        // carry a price-independent mod "b" (pure noise): because price is
        // monotonic in index, "b"-presence is spread across the whole price range
        // and predicts nothing. This makes Jaccard vary across pairs without
        // tracking price — so jaccard-heavy weights group by the useless {a} vs
        // {a,b} split (higher LOO error) while roll-heavy weights select by a's
        // roll proximity (low error), and the grid self-selects roll-dominant.
        let items: Vec<ItemVector> = (0..40)
            .map(|i| {
                let r = i as f64 / 39.0;
                let mods = if i % 2 == 0 {
                    vec![("a".into(), Some(r)), ("b".into(), None)]
                } else {
                    vec![("a".into(), Some(r))]
                };
                ItemVector {
                    mods,
                    price_divine: 10.0 + 100.0 * r,
                }
            })
            .collect();
        let (w, cal) = tune_and_calibrate(&items);
        assert!(
            w.roll > w.jaccard,
            "magnitude-dominant → roll weight wins (w={:?})",
            w
        );
        assert!(cal.model_err.unwrap() < 0.3, "calibrated");
    }

    #[test]
    fn tune_picks_jaccard_for_combination_dominant_corpus() {
        // price determined by how many of {a,b,c} are present; rolls absent.
        let mk = |present: &[&str], price: f64| ItemVector {
            mods: present.iter().map(|s| (s.to_string(), None)).collect(),
            price_divine: price,
        };
        let mut items = Vec::new();
        for _ in 0..15 {
            items.push(mk(&["a"], 10.0));
        }
        for _ in 0..15 {
            items.push(mk(&["a", "b"], 50.0));
        }
        for _ in 0..15 {
            items.push(mk(&["a", "b", "c"], 200.0));
        }
        let (w, _) = tune_and_calibrate(&items);
        assert!(
            w.jaccard >= w.roll,
            "combination-dominant → jaccard not beaten (w={:?})",
            w
        );
    }

    #[test]
    fn loo_probe_count_caps_and_covers_evenly() {
        assert_eq!(loo_probe_count(40), 40, "small corpus probes every item");
        assert_eq!(
            loo_probe_count(LOO_MAX_PROBES),
            LOO_MAX_PROBES,
            "at the cap, probe all"
        );
        // Just over the cap: still ~the full cap of probes — no ceil-stride cliff that
        // drops nearly half the data at the boundary (n=401 must NOT collapse to ~200).
        assert_eq!(loo_probe_count(LOO_MAX_PROBES + 1), LOO_MAX_PROBES);
        assert_eq!(loo_probe_count(5000), LOO_MAX_PROBES, "large corpus capped");
        // Evenly-spaced indices `k·n/probes` span the corpus: start at 0, stay in range,
        // strictly increasing (distinct + evenly spread, not one modulo class).
        let n = LOO_MAX_PROBES + 1;
        let probes = loo_probe_count(n);
        let idx: Vec<usize> = (0..probes).map(|k| k * n / probes).collect();
        assert_eq!(idx.len(), probes);
        assert_eq!(idx[0], 0);
        assert!(*idx.last().unwrap() < n, "indices in range");
        assert!(
            idx.windows(2).all(|w| w[1] > w[0]),
            "strictly increasing → distinct, evenly spread"
        );
    }
}
