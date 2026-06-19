//! Pure marginal-contribution (hedonic) price model: fit `ln(price)` on which of
//! our mods each comparable has, then predict the full item. No I/O; the caller
//! (`ablation::marginal_estimate`) does the sampling.

use crate::trade::model::Listing;

/// Minimum pooled comparables required to fit; below this, return `None`.
const MIN_FIT: usize = 20;
/// Fraction trimmed from each end of the price distribution before fitting.
const TRIM_FRAC: f64 = 0.10;

#[derive(Clone, Debug, PartialEq)]
pub struct Prediction {
    pub p20: f64,
    pub p50: f64,
    pub p80: f64,
    pub sample: usize,
    pub kept_features: usize,
}

/// A comparable's hedonic feature vector plus its observed price (in Divine).
/// `features[i]` is 1.0 iff our mod `i` is present on the listing; built with
/// provenance by the caller (`ablation::marginal_estimate`) so that pseudo
/// aggregates — which never appear in a listing's `explicit_stat_ids` — still
/// carry signal from the sub-query that searched on them.
#[derive(Clone, Debug, PartialEq)]
pub struct FeatureRow {
    pub features: Vec<f64>,
    pub price_divine: f64,
}

/// 1.0 if `listing` carries each of `our_ids` *as an explicit mod*, else 0.0
/// (parallel to `our_ids`). Pseudo ids never match here by design — the caller
/// supplies their positives via per-mod sub-query provenance.
pub fn explicit_features(listing: &Listing, our_ids: &[String]) -> Vec<f64> {
    our_ids
        .iter()
        .map(|id| {
            if listing.explicit_stat_ids.iter().any(|s| s == id) {
                1.0
            } else {
                0.0
            }
        })
        .collect()
}

fn quantile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Solve `(XᵀX) β = Xᵀy` by Gaussian elimination with partial pivoting.
/// `x` rows already include a leading 1.0 (intercept). `None` if singular.
#[allow(clippy::needless_range_loop)]
fn ols(x: &[Vec<f64>], y: &[f64]) -> Option<Vec<f64>> {
    let k = x.first()?.len();
    // Normal equations.
    let mut a = vec![vec![0.0_f64; k + 1]; k]; // augmented [XᵀX | Xᵀy]
    for (row, &yi) in x.iter().zip(y) {
        for i in 0..k {
            for j in 0..k {
                a[i][j] += row[i] * row[j];
            }
            a[i][k] += row[i] * yi;
        }
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..k {
        let mut piv = col;
        for r in (col + 1)..k {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if a[piv][col].abs() < 1e-9 {
            return None; // singular / collinear
        }
        a.swap(col, piv);
        let d = a[col][col];
        for j in col..=k {
            a[col][j] /= d;
        }
        for r in 0..k {
            if r != col {
                let f = a[r][col];
                for j in col..=k {
                    a[r][j] -= f * a[col][j];
                }
            }
        }
    }
    Some((0..k).map(|i| a[i][k]).collect())
}

/// Fit `ln(price) ~ intercept + Σ present_i` over precomputed `rows`, then
/// predict the item that has ALL our mods. Each row's `features` are built by the
/// caller with provenance (so pseudo aggregates carry signal). Drops zero-variance
/// feature columns (folded into the intercept), clamps coefficients ≥ 0, and builds
/// the interval from residual quantiles. The `p50` is floored at the median of the
/// base subset (all-zero feature rows). `None` when too few comparables or singular.
pub fn model_price(rows: &[FeatureRow]) -> Option<Prediction> {
    let n_feat = rows.first()?.features.len();

    // Trim both price tails.
    let mut by_price: Vec<&FeatureRow> = rows.iter().filter(|r| r.price_divine > 0.0).collect();
    by_price.sort_by(|a, b| a.price_divine.partial_cmp(&b.price_divine).unwrap());
    let drop = ((by_price.len() as f64) * TRIM_FRAC).floor() as usize;
    let kept: Vec<&FeatureRow> = if by_price.len() > 2 * drop {
        by_price[drop..by_price.len() - drop].to_vec()
    } else {
        by_price
    };
    if kept.len() < MIN_FIT {
        return None;
    }

    // Keep only feature columns that vary in the sample (others are unidentifiable
    // and would make the system singular; their effect lives in the intercept).
    let keep_cols: Vec<usize> = (0..n_feat)
        .filter(|&c| {
            let first = kept[0].features[c];
            kept.iter().any(|r| r.features[c] != first)
        })
        .collect();

    let x: Vec<Vec<f64>> = kept
        .iter()
        .map(|r| {
            let mut row = vec![1.0];
            row.extend(keep_cols.iter().map(|&c| r.features[c]));
            row
        })
        .collect();
    let y: Vec<f64> = kept.iter().map(|r| r.price_divine.ln()).collect();

    let mut coef = ols(&x, &y)?;
    // Clamp marginal coefficients (not the intercept) to be non-negative.
    for c in coef.iter_mut().skip(1) {
        if *c < 0.0 {
            *c = 0.0;
        }
    }

    // Predict the full item: every kept feature present (= 1).
    let pred: f64 = coef[0] + coef[1..].iter().sum::<f64>();

    // Residual quantiles (log space) → interval, recomputed against clamped coef.
    let mut resid: Vec<f64> = x
        .iter()
        .zip(&y)
        .map(|(row, &yi)| {
            let fit: f64 = row.iter().zip(&coef).map(|(xi, ci)| xi * ci).sum();
            yi - fit
        })
        .collect();
    resid.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let p50 = pred.exp();
    let p20 = (pred + quantile(&resid, 0.20)).exp();
    let p80 = (pred + quantile(&resid, 0.80)).exp();
    // Floor at the median of the BASE subset (rows with no mod present); the full
    // item is never worth less than a bare base. Using only all-zero rows avoids
    // inflating the floor with the pricier per-mod samples. Fall back to the
    // overall median if no all-zero row survived the trim.
    let base_median = {
        let mut base: Vec<f64> = kept
            .iter()
            .filter(|r| r.features.iter().all(|&f| f == 0.0))
            .map(|r| r.price_divine)
            .collect();
        if base.is_empty() {
            base = kept.iter().map(|r| r.price_divine).collect();
        }
        base.sort_by(|a, b| a.partial_cmp(b).unwrap());
        quantile(&base, 0.50)
    };
    let p50 = p50.max(base_median);
    Some(Prediction {
        p20: p20.max(0.0).min(p50),
        p50,
        p80: p80.max(p50),
        sample: kept.len(),
        kept_features: keep_cols.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(price: f64, features: &[f64]) -> FeatureRow {
        FeatureRow {
            features: features.to_vec(),
            price_divine: price,
        }
    }

    // base≈2, mod A multiplies ×2, mod B ×3 (log-additive). Build a varied sample
    // of rows that carry A alone, B alone, or neither — but NEVER both together.
    fn sample() -> Vec<FeatureRow> {
        let mut v = Vec::new();
        for i in 0..15 {
            let j = (i % 5) as f64 * 0.1;
            v.push(row(2.0 + j, &[0.0, 0.0])); // base
            v.push(row(4.0 + j, &[1.0, 0.0])); // +A  (×2)
            v.push(row(6.0 + j, &[0.0, 1.0])); // +B  (×3)
        }
        v
    }

    #[test]
    fn predicts_full_from_partials() {
        let p = model_price(&sample()).expect("fits");
        // ln2 + ln2 + ln3 = ln12 → ~12, though no row had both A and B.
        assert!(p.p50 > 9.0 && p.p50 < 16.0, "p50={}", p.p50);
        assert!(p.p20 <= p.p50 && p.p50 <= p.p80);
        assert_eq!(p.kept_features, 2);
    }

    #[test]
    fn too_few_comparables_returns_none() {
        let few = vec![row(2.0, &[0.0]), row(4.0, &[1.0])];
        assert!(model_price(&few).is_none());
    }

    #[test]
    fn zero_variance_feature_dropped_not_singular() {
        // Feature C present on EVERY row → unidentifiable; must be dropped, not
        // crash the solve. Features are [A, C]; C is always 1.0.
        let mut v = Vec::new();
        for i in 0..12 {
            let j = i as f64 * 0.01;
            v.push(row(2.0 + j, &[0.0, 1.0]));
            v.push(row(4.0 + j, &[1.0, 1.0]));
        }
        let p = model_price(&v).expect("fits with C dropped");
        assert_eq!(p.kept_features, 1); // only A varies
    }

    #[test]
    fn floor_uses_base_subset_not_pooled_median() {
        // Reproduce Bug #5: the per-mod samples are pricier and OUTNUMBER the base,
        // so the POOLED median sits among the expensive rows and would inflate the
        // floor. The correct floor reads only the base (all-zero) subset.
        //
        // 20 cheap base rows (~2.0)          → base median ≈ 2.0
        // 10 cheap mod rows (~2.1) + 35 pricey mod rows (~20.0), all feature [1.0]:
        //   the OLS log-mean of the mod group lands at pred ≈ 11 (between the two
        //   clusters), but the mod rows' COUNT puts the pooled median at ≈ 20.
        // Base-only floor: p50 = max(pred≈11, base_median≈2) = ≈11 (model value).
        // Pooled floor:    p50 = max(pred≈11, pooled_median≈20) = ≈20 (overpriced).
        let mut v = Vec::new();
        for i in 0..20 {
            v.push(row(2.0 + (i % 4) as f64 * 0.01, &[0.0]));
        }
        for i in 0..10 {
            v.push(row(2.1 + (i % 4) as f64 * 0.01, &[1.0]));
        }
        for i in 0..35 {
            v.push(row(20.0 + (i % 5) as f64 * 0.1, &[1.0]));
        }
        let p = model_price(&v).expect("fits");
        // The base-only floor leaves p50 at the model prediction (~11), well below
        // the pricey pooled median (~20) a pooled-median floor would have produced.
        assert!(
            p.p50 < 16.0,
            "floor inflated p50={} toward pooled median",
            p.p50
        );
        assert!(p.p50 > 5.0, "p50={}", p.p50);
    }
}
