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

/// One explicit mod on a fetched listing, for the observation corpus.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListingMod {
    /// Normalised stat id, e.g. `explicit.stat_2768835289`.
    pub stat_id: String,
    /// Affix tier number (1 = best); parsed from the fetch `tier` string (`"P5"`→5).
    pub tier: Option<u8>,
    /// The displayed rolled value (first number of the mod description).
    pub roll: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Listing {
    pub price: Money,
    /// Price normalized to Divine Orbs for comparison/ranking.
    pub price_divine: f64,
    /// Count of explicit (prefix/suffix) mods on the listed item; the
    /// craftability-tier key. `0` when the fetch response omits mods.
    pub explicit_count: usize,
    /// Trade listing id (dedup key when pooling several searches).
    pub id: String,
    /// The listed item's base type (e.g. "Chiming Staff"), from the fetch
    /// `item.baseType`. The corpus join key across paste and harvest.
    pub base_type: Option<String>,
    /// Per-mod enrichment for the observation corpus: stat id, tier, and roll.
    pub mods: Vec<ListingMod>,
    /// When the listing was posted (`listing.indexed`, ISO-8601). Lets the
    /// corpus/analysis down-weight stale, economy-drifted prices. `None` if absent.
    pub indexed: Option<String>,
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
pub struct EquipFilter {
    pub key: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
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
    pub equipment: Vec<EquipFilter>,
    /// Minimum listing price in Divine for a price-banded search (harvest only;
    /// `None` for normal pricing searches).
    pub min_price_divine: Option<f64>,
    /// Maximum listing price in Divine for a price-banded search (harvest only;
    /// `None` for normal pricing searches). With `min_price_divine` this bounds a
    /// band to `[min, max]` so within-band sampling is representative rather than
    /// skewed to the global cheap end.
    pub max_price_divine: Option<f64>,
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

/// Which comparable set the estimate was computed over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstimateBasis {
    /// Filtered to the item's craftability tier (the normal, sharp path).
    CraftTier,
    /// Craftability known but no comparable bases listed → broad-market sample.
    BroadMarket,
    /// Craftability unknown (basic clipboard) → unfiltered, affixes-only.
    AffixesOnly,
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
    pub basis: EstimateBasis,
}

impl PriceEstimate {
    /// True when we have live listings but the representative (typical/p50) value is
    /// below the priceable floor — too cheap to bother estimating precisely.
    /// `listing_count == 0` (no comparable data) is NOT sub-priceable: absence of
    /// comps is not evidence of cheapness.
    pub fn is_sub_priceable(&self) -> bool {
        self.listing_count > 0 && self.typical < crate::trade::quality::MIN_PRICEABLE_DIVINE
    }
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
            explicit_count: 0,
            id: String::new(),
            base_type: None,
            mods: vec![],
            indexed: None,
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

    #[test]
    fn is_sub_priceable_only_when_listings_and_cheap() {
        let base = PriceEstimate {
            low: 0.1,
            typical: 0.5,
            high: 0.9,
            listing_count: 8,
            confidence: Confidence::Medium,
            modal_currency: Currency::Divine,
            basis: EstimateBasis::CraftTier,
        };
        assert!(
            base.is_sub_priceable(),
            "cheap with listings → sub-priceable"
        );
        assert!(
            !PriceEstimate {
                typical: 1.0,
                ..base.clone()
            }
            .is_sub_priceable(),
            "exactly 1 div → priceable"
        );
        assert!(
            !PriceEstimate {
                typical: 5.0,
                ..base.clone()
            }
            .is_sub_priceable(),
            "above floor → not sub-priceable"
        );
        assert!(
            !PriceEstimate {
                listing_count: 0,
                ..base.clone()
            }
            .is_sub_priceable(),
            "no listings is unknown, not cheap"
        );
    }
}
