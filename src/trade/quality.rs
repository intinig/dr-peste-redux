//! Price-quality predicate shared by the capture path (`client::parse_fetch`) and
//! the value-model rebuild (`value::rebuild_into`): one source of truth for "is this
//! divine price worth pricing and learning from".

/// Floor on what we bother pricing. Items below this are reported as "under 1
/// divine" rather than estimated, and corpus rows below it carry no signal for the
/// value model. This is a product decision about what we care about — NOT a tuned
/// model parameter; it never moves with observed or target prices.
pub const MIN_PRICEABLE_DIVINE: f64 = 1.0;

/// Backstop upper bound for absurd troll listings (e.g. 1,111,111 div) in the rare
/// case the mirror-tier filter can't run (mirror conversion unavailable). Set far
/// above any legitimate single-item price in a league.
pub const ABSURD_DIVINE_CAP: f64 = 100_000.0;

/// True if a divine price is in the band we price and learn from:
/// `MIN_PRICEABLE_DIVINE <= price_divine < ABSURD_DIVINE_CAP`.
pub fn is_priceable(price_divine: f64) -> bool {
    (MIN_PRICEABLE_DIVINE..ABSURD_DIVINE_CAP).contains(&price_divine)
}

/// Capture-time ceiling check: drop only absurd troll prices. Unlike `is_priceable`,
/// this imposes NO 1-div floor — sub-1-div listings must reach the live `/paste`
/// pricer so it can detect and report a genuinely cheap item. The 1-div floor is a
/// corpus-learning concern only, applied in `value::rebuild_into` via `is_priceable`.
pub fn is_below_absurd_cap(price_divine: f64) -> bool {
    price_divine < ABSURD_DIVINE_CAP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_is_inclusive_at_one_div() {
        assert!(is_priceable(1.0), "exactly 1 div is priceable");
        assert!(!is_priceable(0.999));
        assert!(!is_priceable(0.0));
        assert!(!is_priceable(0.0015), "currency dust");
    }

    #[test]
    fn absurd_cap_is_exclusive_upper_bound() {
        assert!(is_priceable(99_999.0));
        assert!(!is_priceable(ABSURD_DIVINE_CAP));
        assert!(!is_priceable(1_111_111.0));
    }

    #[test]
    fn typical_rare_prices_are_priceable() {
        for p in [1.0, 5.0, 30.0, 300.0, 1200.0] {
            assert!(is_priceable(p), "{p} div should be priceable");
        }
    }

    #[test]
    fn below_absurd_cap_allows_sub_one_div() {
        assert!(
            is_below_absurd_cap(0.5),
            "sub-1-div listing must pass the capture ceiling check"
        );
        assert!(
            is_below_absurd_cap(30.0),
            "in-band price must pass the capture ceiling check"
        );
        assert!(
            !is_below_absurd_cap(ABSURD_DIVINE_CAP),
            "at the cap boundary should be dropped"
        );
        assert!(
            !is_below_absurd_cap(1_111_111.0),
            "far above the cap should be dropped"
        );
    }
}
