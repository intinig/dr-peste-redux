//! Core value types for the currency-arbitrage engine. Pure, no I/O.

use crate::arb::graph::CycleResult;
use crate::arb::spread::FlipResult;

/// A trade2 currency-exchange id (e.g. "divine", "exalted").
pub type Currency = String;

/// Where a quote came from: a live trade2 order book, or a cxapi hourly digest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Freshness {
    Live,
    #[allow(dead_code)] // Phase 2: constructed by the Phase 2 cxapi source
    Aggregated,
}

/// One executable conversion: give `pay` units of `from` to receive `get`
/// units of `to`. Kept as integers to match the exchange's discrete ratios.
#[derive(Clone, Debug)]
pub struct RatioQuote {
    pub pay: u32,
    pub get: u32,
    /// Units of `to` available at this quote (taker depth).
    pub stock: u64,
    #[allow(dead_code)] // Phase 2: read by the Phase 2 confirm stage
    pub freshness: Freshness,
}

impl RatioQuote {
    /// Units of `to` received per 1 unit of `from`.
    pub fn ratio(&self) -> f64 {
        self.get as f64 / self.pay as f64
    }
}

/// A directed conversion edge `from -> to` carrying its best quote.
#[derive(Clone, Debug)]
pub struct Edge {
    pub from: Currency,
    pub to: Currency,
    pub quote: RatioQuote,
}

/// One hop of a realised cycle.
#[derive(Clone, Debug)]
pub struct Leg {
    pub from: Currency,
    pub to: Currency,
    pub quote: RatioQuote,
}

#[derive(Clone, Debug)]
pub enum Opportunity {
    Triangulation {
        legs: Vec<Leg>,
        multiplier: f64,
        feasible_volume: f64,
        #[allow(dead_code)] // Phase 2: read by the Phase 2 confirm stage
        confidence: Freshness,
    },
    Flip {
        market: (Currency, Currency),
        spread_pct: f64,
        volume: f64,
        #[allow(dead_code)] // Phase 2: read by the Phase 2 confirm stage
        confidence: Freshness,
    },
}

impl Opportunity {
    pub fn from_cycle(c: CycleResult, confidence: Freshness) -> Opportunity {
        Opportunity::Triangulation {
            legs: c.legs,
            multiplier: c.multiplier,
            feasible_volume: c.feasible_volume,
            confidence,
        }
    }
    pub fn from_flip(f: FlipResult, confidence: Freshness) -> Opportunity {
        Opportunity::Flip {
            market: f.market,
            spread_pct: f.spread_pct,
            volume: f.volume,
            confidence,
        }
    }
    pub fn score(&self) -> f64 {
        match self {
            Opportunity::Triangulation {
                multiplier,
                feasible_volume,
                ..
            } => (multiplier - 1.0) * feasible_volume,
            Opportunity::Flip {
                spread_pct, volume, ..
            } => spread_pct * volume,
        }
    }
    #[allow(dead_code)] // Phase 2: called by the Phase 2 confirm stage
    pub fn confidence(&self) -> Freshness {
        match self {
            Opportunity::Triangulation { confidence, .. } => *confidence,
            Opportunity::Flip { confidence, .. } => *confidence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_is_get_over_pay() {
        let q = RatioQuote {
            pay: 2,
            get: 5,
            stock: 100,
            freshness: Freshness::Live,
        };
        assert!((q.ratio() - 2.5).abs() < 1e-9);
    }

    #[test]
    fn triangulation_score_is_profit_times_volume() {
        let c = crate::arb::graph::CycleResult {
            legs: vec![],
            multiplier: 1.2,
            feasible_volume: 50.0,
        };
        let opp = Opportunity::from_cycle(c, Freshness::Live);
        assert!((opp.score() - 10.0).abs() < 1e-9); // 0.2 * 50
    }
}
