//! Per-(category, mod) roll-magnitude normalization. Maps a rolled value to its
//! percentile within the corpus distribution of that mod, so similarity can treat
//! "high roll" comparably across mods with different scales.
use super::ROLL_QUANTILES;
use crate::observe::Observation;
use std::collections::HashMap;

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct RollStats {
    /// Evenly-spaced quantile knots (ROLL_QUANTILES of them), ascending.
    pub quantiles: Vec<f64>,
}

impl RollStats {
    pub fn from_rolls(rolls: &[f64]) -> RollStats {
        let mut v: Vec<f64> = rolls.iter().copied().filter(|r| r.is_finite()).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if v.len() < 2 {
            return RollStats { quantiles: v };
        }
        let n = v.len();
        let quantiles = (0..ROLL_QUANTILES)
            .map(|i| {
                let p = i as f64 / (ROLL_QUANTILES - 1) as f64;
                let pos = p * (n - 1) as f64;
                let lo = pos.floor() as usize;
                let hi = (lo + 1).min(n - 1);
                let frac = pos - lo as f64;
                v[lo] + frac * (v[hi] - v[lo])
            })
            .collect();
        RollStats { quantiles }
    }

    /// Percentile of `roll` in [0,1]; 0.0 if the distribution is degenerate.
    #[allow(dead_code)]
    pub fn normalize(&self, roll: f64) -> f64 {
        let q = &self.quantiles;
        if q.len() < 2 || q[q.len() - 1] <= q[0] {
            return 0.0;
        }
        if roll <= q[0] {
            return 0.0;
        }
        if roll >= q[q.len() - 1] {
            return 1.0;
        }
        // linear interp between the bracketing knots
        for w in q.windows(2).enumerate() {
            let (i, pair) = w;
            if roll <= pair[1] {
                let frac = if pair[1] > pair[0] {
                    (roll - pair[0]) / (pair[1] - pair[0])
                } else {
                    0.0
                };
                return (i as f64 + frac) / (q.len() - 1) as f64;
            }
        }
        1.0
    }
}

#[allow(dead_code)]
pub fn build_mod_rolls(obs: &[&Observation]) -> HashMap<String, RollStats> {
    let mut rolls: HashMap<&str, Vec<f64>> = HashMap::new();
    for o in obs {
        for m in &o.mods {
            if let Some(r) = m.roll {
                rolls.entry(m.stat_id.as_str()).or_default().push(r);
            }
        }
    }
    rolls
        .into_iter()
        .map(|(k, v)| (k.to_string(), RollStats::from_rolls(&v)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_maps_roll_to_percentile() {
        let rs = RollStats::from_rolls(&[10.0, 20.0, 30.0, 40.0, 50.0]);
        assert!((rs.normalize(10.0) - 0.0).abs() < 1e-9, "min → 0");
        assert!((rs.normalize(50.0) - 1.0).abs() < 1e-9, "max → 1");
        assert!((rs.normalize(30.0) - 0.5).abs() < 0.05, "median ≈ 0.5");
        assert_eq!(rs.normalize(5.0), 0.0, "below min clamps to 0");
        assert_eq!(rs.normalize(99.0), 1.0, "above max clamps to 1");
    }

    #[test]
    fn single_value_normalizes_to_zero() {
        let rs = RollStats::from_rolls(&[7.0]);
        assert_eq!(rs.normalize(7.0), 0.0); // degenerate range → no magnitude signal
    }
}
