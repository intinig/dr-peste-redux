//! Detect "undersampled gate" mods: build-defining mods the model can't yet learn
//! a magnitude curve for (too few samples). Surfaced for operator-triggered
//! targeted sampling.
use super::{DRIVER_LIFT, MAGNITUDE_MIN_SAMPLE};
use crate::trade::value::StatValue;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct GateCandidate {
    pub stat_id: String,
    pub label: Option<String>,
    pub count: usize,
}

pub fn detect_gates(stats: &[StatValue]) -> Vec<GateCandidate> {
    let mut out: Vec<GateCandidate> = stats
        .iter()
        .filter(|s| s.count < MAGNITUDE_MIN_SAMPLE)
        .filter(|s| {
            let cornerstone = s
                .label
                .as_deref()
                .map(crate::trade::query::is_cornerstone)
                .unwrap_or(false);
            let high_signal = s.lift >= DRIVER_LIFT;
            cornerstone || high_signal
        })
        .map(|s| GateCandidate {
            stat_id: s.stat_id.clone(),
            label: s.label.clone(),
            count: s.count,
        })
        .collect();
    out.sort_by(|a, b| a.count.cmp(&b.count));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_cornerstone_and_high_signal_low_count() {
        use crate::trade::value::StatValue;
        let sv = |label: &str, count: usize, lift: f64| StatValue {
            stat_id: format!("id.{label}"),
            label: Some(label.into()),
            count,
            median_with: 0.0,
            lift,
            conditional_lift: None,
            top_decile_freq: 0.0,
        };
        let stats = vec![
            sv("+1 to Level of all Projectile Skills", 12, 1.0), // cornerstone, low count → flagged
            sv("# to maximum Life", 400, 1.1),                   // common, not a gate → not flagged
            sv("#% increased Rare Mechanic", 10, 2.0),           // high lift, low count → flagged
            sv("+1 to Level of all Spell Skills", 200, 1.4), // cornerstone but well-sampled → not flagged
        ];
        let gates = detect_gates(&stats);
        let names: Vec<&str> = gates.iter().filter_map(|g| g.label.as_deref()).collect();
        assert!(names.iter().any(|n| n.contains("Projectile")));
        assert!(names.iter().any(|n| n.contains("Rare Mechanic")));
        assert!(!names.iter().any(|n| n.contains("maximum Life")));
        assert!(!names.iter().any(|n| n.contains("all Spell")));
    }
}
