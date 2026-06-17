//! Currency → Divine-Orb rate table, refreshed from poe.ninja economy data.

use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct RateTable(HashMap<String, f64>);

impl RateTable {
    pub fn new(map: HashMap<String, f64>) -> Self {
        RateTable(map)
    }

    /// Divine value of `amount` units of `code`, or None if the currency is
    /// unknown (so the caller can drop the listing rather than mis-price it).
    pub fn to_divine(&self, amount: f64, code: &str) -> Option<f64> {
        self.0.get(code).map(|per_unit| amount * per_unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_divine_known_currencies() {
        let table = RateTable::new(HashMap::from([
            ("divine".to_string(), 1.0),
            ("chaos".to_string(), 0.1),
        ]));
        assert_eq!(table.to_divine(5.0, "chaos"), Some(0.5));
        assert_eq!(table.to_divine(2.0, "divine"), Some(2.0));
        assert_eq!(table.to_divine(1.0, "aug"), None);
    }
}
