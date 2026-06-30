//! Core value types for the currency-arbitrage engine. Pure, no I/O.

/// A trade2 currency-exchange id (e.g. "divine", "exalted").
pub type Currency = String;

/// Where a quote came from: a live trade2 order book, or a cxapi hourly digest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Freshness {
    Live,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_is_get_over_pay() {
        let q = RatioQuote { pay: 2, get: 5, stock: 100, freshness: Freshness::Live };
        assert!((q.ratio() - 2.5).abs() < 1e-9);
    }
}
