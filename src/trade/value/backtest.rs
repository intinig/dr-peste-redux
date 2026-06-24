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

fn predict_one(items: &[ItemVector], skip: usize, w: SimWeights) -> Option<f64> {
    let q: Vec<(String, Option<f64>)> = items[skip].mods.clone();
    let mut scored: Vec<(f64, f64)> = items
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != skip)
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

pub fn loo_median_error(items: &[ItemVector], w: SimWeights) -> Option<f64> {
    let n = items.len();
    let probes = loo_probe_count(n);
    if probes == 0 {
        return None;
    }
    // Evenly-spaced probe indices across [0, n): `k·n/probes`. Each prediction still
    // searches all other items for neighbours; only the number of held-out probes we
    // measure error over is bounded (to `probes`), spread across the whole corpus.
    let mut errs: Vec<f64> = Vec::with_capacity(probes);
    for k in 0..probes {
        let i = k * n / probes;
        let actual = items[i].price_divine;
        if actual > 0.0 {
            if let Some(pred) = predict_one(items, i, w) {
                errs.push((pred - actual).abs() / actual);
            }
        }
    }
    if errs.len() < MIN_NEIGHBORS {
        return None;
    }
    errs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(errs[errs.len() / 2])
}

pub fn tune_weights(items: &[ItemVector]) -> (SimWeights, Option<f64>) {
    let mut best = (
        SimWeights {
            jaccard: 1.0,
            roll: 0.0,
        },
        None::<f64>,
    );
    for (j, r) in WEIGHT_GRID {
        let w = SimWeights {
            jaccard: j,
            roll: r,
        };
        if let Some(e) = loo_median_error(items, w) {
            if best.1.map(|b| e < b).unwrap_or(true) {
                best = (w, Some(e));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let (w, err) = tune_weights(&items);
        assert!(
            w.roll > w.jaccard,
            "magnitude-dominant → roll weight wins (w={:?})",
            w
        );
        assert!(err.unwrap() < 0.3, "calibrated");
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
        let (w, _) = tune_weights(&items);
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
