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

/// 1.0 if `listing` carries each of `our_ids`, else 0.0 (parallel to `our_ids`).
fn features(listing: &Listing, our_ids: &[String]) -> Vec<f64> {
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

/// Fit `ln(price) ~ intercept + Σ present_i` over `listings`, then predict the
/// item that has ALL our mods. Drops zero-variance feature columns (folded into
/// the intercept), clamps coefficients ≥ 0, and builds the interval from residual
/// quantiles. `None` when too few comparables or the system is singular.
pub fn model_price(listings: &[Listing], our_ids: &[String]) -> Option<Prediction> {
    // Trim both price tails.
    let mut by_price: Vec<&Listing> = listings.iter().filter(|l| l.price_divine > 0.0).collect();
    by_price.sort_by(|a, b| a.price_divine.partial_cmp(&b.price_divine).unwrap());
    let drop = ((by_price.len() as f64) * TRIM_FRAC).floor() as usize;
    let kept: Vec<&Listing> = if by_price.len() > 2 * drop {
        by_price[drop..by_price.len() - drop].to_vec()
    } else {
        by_price
    };
    if kept.len() < MIN_FIT {
        return None;
    }

    // Keep only feature columns that vary in the sample (others are unidentifiable
    // and would make the system singular; their effect lives in the intercept).
    let raw: Vec<Vec<f64>> = kept.iter().map(|l| features(l, our_ids)).collect();
    let keep_cols: Vec<usize> = (0..our_ids.len())
        .filter(|&c| {
            let first = raw[0][c];
            raw.iter().any(|r| r[c] != first)
        })
        .collect();

    let x: Vec<Vec<f64>> = raw
        .iter()
        .map(|r| {
            let mut row = vec![1.0];
            row.extend(keep_cols.iter().map(|&c| r[c]));
            row
        })
        .collect();
    let y: Vec<f64> = kept.iter().map(|l| l.price_divine.ln()).collect();

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
    // Floor at the trimmed base median (full item is never worth less than base).
    let base_median = {
        let mut p: Vec<f64> = kept.iter().map(|l| l.price_divine).collect();
        p.sort_by(|a, b| a.partial_cmp(b).unwrap());
        quantile(&p, 0.50)
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
    use crate::trade::model::{Currency, Money};

    fn lst(price: f64, ids: &[&str]) -> Listing {
        Listing {
            price: Money {
                amount: price,
                currency: Currency::Divine,
            },
            price_divine: price,
            explicit_count: ids.len(),
            id: format!("{price}-{}", ids.join("+")),
            explicit_stat_ids: ids.iter().map(|s| s.to_string()).collect(),
        }
    }

    // base≈2, mod A multiplies ×2, mod B ×3 (log-additive). Build a varied sample
    // including 5-of-6-style partials but NEVER the full A+B together.
    fn sample() -> Vec<Listing> {
        let mut v = Vec::new();
        for i in 0..15 {
            let j = (i % 5) as f64 * 0.1;
            v.push(lst(2.0 + j, &[])); // base
            v.push(lst(4.0 + j, &["A"])); // +A  (×2)
            v.push(lst(6.0 + j, &["B"])); // +B  (×3)
        }
        v
    }

    #[test]
    fn predicts_full_from_partials() {
        let ids = vec!["A".to_string(), "B".to_string()];
        let p = model_price(&sample(), &ids).expect("fits");
        // ln2 + ln2 + ln3 = ln12 → ~12, though no comparable had both A and B.
        assert!(p.p50 > 9.0 && p.p50 < 16.0, "p50={}", p.p50);
        assert!(p.p20 <= p.p50 && p.p50 <= p.p80);
        assert_eq!(p.kept_features, 2);
    }

    #[test]
    fn too_few_comparables_returns_none() {
        let ids = vec!["A".to_string()];
        let few = vec![lst(2.0, &[]), lst(4.0, &["A"])];
        assert!(model_price(&few, &ids).is_none());
    }

    #[test]
    fn zero_variance_feature_dropped_not_singular() {
        // Feature C present on EVERY comparable → unidentifiable; must be dropped,
        // not crash the solve.
        let ids = vec!["A".to_string(), "C".to_string()];
        let mut v = Vec::new();
        for i in 0..12 {
            let j = i as f64 * 0.01;
            v.push(lst(2.0 + j, &["C"]));
            v.push(lst(4.0 + j, &["A", "C"]));
        }
        let p = model_price(&v, &ids).expect("fits with C dropped");
        assert_eq!(p.kept_features, 1); // only A varies
    }
}
