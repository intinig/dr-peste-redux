//! Domain types for trade pricing. Amounts in `*_divine` are normalized to
//! Divine Orbs, the common comparison unit.

use serde::Serialize;

#[derive(Clone, Debug, PartialEq)]
pub enum Currency {
    Chaos,
    Exalted,
    Divine,
    Other(String),
}

impl Currency {
    /// Rate-table lookup key.
    pub fn code(&self) -> &str {
        match self {
            Currency::Chaos => "chaos",
            Currency::Exalted => "exalted",
            Currency::Divine => "divine",
            Currency::Other(s) => s,
        }
    }
    /// Short label for display.
    pub fn short(&self) -> &str {
        match self {
            Currency::Chaos => "chaos",
            Currency::Exalted => "ex",
            Currency::Divine => "div",
            Currency::Other(s) => s,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Money {
    pub amount: f64,
    pub currency: Currency,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Listing {
    pub price: Money,
    /// Price normalized to Divine Orbs for comparison/ranking.
    pub price_divine: f64,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct StatFilter {
    /// trade2 stat id, e.g. "explicit.stat_..." or "pseudo.pseudo_total_elemental_resistance".
    pub id: String,
    /// Human label for the breakdown UI.
    pub label: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct MiscFilters {
    pub item_level_min: Option<u32>,
    pub quality_min: Option<u32>,
    pub corrupted: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TradeQuery {
    pub league: String,
    /// trade2 category, e.g. "weapon.staff".
    pub category: Option<String>,
    /// Exact base type ("type"), e.g. "Expert Crackling Staff".
    pub type_line: Option<String>,
    pub stats: Vec<StatFilter>,
    pub misc: MiscFilters,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchResponse {
    pub id: String,
    pub total: u64,
    pub hashes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    /// High ≥ 10 listings, Medium ≥ 3, else Low.
    pub fn from_count(n: usize) -> Self {
        if n >= 10 {
            Confidence::High
        } else if n >= 3 {
            Confidence::Medium
        } else {
            Confidence::Low
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PriceEstimate {
    pub low: f64,
    pub typical: f64,
    pub high: f64,
    pub listing_count: usize,
    pub confidence: Confidence,
    pub modal_currency: Currency,
}

/// Describes how a stat filter was ablated in a breakdown probe.
/// v1 supports only `Drop` (remove the filter entirely); relaxing a bound
/// (e.g. lowering the min) is a documented future variant.
#[derive(Clone, Debug, PartialEq)]
pub enum AblationKind {
    Drop,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Contribution {
    pub characteristic: String,
    pub kind: AblationKind,
    /// How many divine the price drops when this characteristic is removed.
    pub delta_divine: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SynergyNote {
    pub a: String,
    pub b: String,
    /// Extra divine beyond the sum of the two individual contributions.
    pub extra_divine: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Breakdown {
    pub baseline: PriceEstimate,
    pub ranked: Vec<Contribution>,
    pub synergy: Option<SynergyNote>,
    pub trade_url: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Probe {
    pub query: TradeQuery,
    pub listing_count: usize,
    pub typical_divine: f64,
    pub timestamp_unix: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_to_divine_field_is_independent_of_currency() {
        let l = Listing {
            price: Money {
                amount: 5.0,
                currency: Currency::Exalted,
            },
            price_divine: 0.5,
        };
        assert_eq!(l.price_divine, 0.5);
        assert!(matches!(l.price.currency, Currency::Exalted));
    }

    #[test]
    fn confidence_from_count_buckets() {
        assert_eq!(Confidence::from_count(15), Confidence::High);
        assert_eq!(Confidence::from_count(5), Confidence::Medium);
        assert_eq!(Confidence::from_count(1), Confidence::Low);
    }
}
