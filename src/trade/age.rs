//! Listing-age (staleness) helpers. Live trade2 listings carry an `indexed`
//! RFC3339 timestamp, and corpus observations persist the same string. EDA on
//! the Staff corpus showed listings older than ~2 weeks are weakly/stale priced
//! (cheap dregs: median 20 div, p90 100), so both live pricing and the value
//! model exclude them.

use chrono::DateTime;

/// Listings (or observations) older than this many days are treated as stale
/// and excluded from pricing and value-model learning. Derived from the Staff
/// corpus age EDA: the active market is the <2-week window; older postings are
/// cheap dregs that drag and mislead.
pub const MAX_LISTING_AGE_DAYS: f64 = 14.0;

/// Current wall-clock time as a Unix timestamp (seconds). Returns 0 if the
/// system clock is before the epoch (never, in practice).
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Parses an RFC3339 / ISO-8601 timestamp (trade2 `listing.indexed`, e.g.
/// `"2026-06-15T12:34:56Z"`) into a Unix timestamp in seconds. Returns `None`
/// if the string can't be parsed.
pub fn parse_indexed(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

/// Whether a listing/observation is fresh enough to use, given its `indexed`
/// timestamp and the current time.
///
/// Conservative by design: an absent or unparseable timestamp is treated as
/// fresh (kept). We only *exclude* listings we can positively date as older
/// than `max_age_days` — never drop data we can't date. Future timestamps
/// (clock skew) are also kept.
pub fn is_fresh_at(indexed: Option<&str>, now_unix: u64, max_age_days: f64) -> bool {
    let Some(s) = indexed else { return true };
    let Some(ts) = parse_indexed(s) else {
        return true;
    };
    let age_secs = now_unix as i64 - ts;
    if age_secs <= 0 {
        return true; // not yet indexed / clock skew → keep
    }
    (age_secs as f64) <= max_age_days * 86_400.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const REF: i64 = 1_609_459_200; // 2021-01-01T00:00:00Z

    #[test]
    fn parses_rfc3339_z() {
        assert_eq!(parse_indexed("2021-01-01T00:00:00Z"), Some(REF));
    }

    #[test]
    fn parses_with_offset_to_same_instant() {
        assert_eq!(parse_indexed("2021-01-01T00:00:00+00:00"), Some(REF));
        assert_eq!(parse_indexed("2021-01-01T01:00:00+01:00"), Some(REF));
    }

    #[test]
    fn unparseable_returns_none() {
        assert_eq!(parse_indexed("not a date"), None);
        assert_eq!(parse_indexed(""), None);
    }

    #[test]
    fn fresh_within_window() {
        let now = (REF + 10 * 86_400) as u64;
        assert!(is_fresh_at(Some("2021-01-01T00:00:00Z"), now, 14.0));
    }

    #[test]
    fn stale_past_window() {
        let now = (REF + 20 * 86_400) as u64;
        assert!(!is_fresh_at(Some("2021-01-01T00:00:00Z"), now, 14.0));
    }

    #[test]
    fn boundary_exactly_max_is_fresh() {
        let now = (REF + 14 * 86_400) as u64;
        assert!(is_fresh_at(Some("2021-01-01T00:00:00Z"), now, 14.0));
    }

    #[test]
    fn absent_timestamp_is_kept() {
        assert!(is_fresh_at(None, (REF + 999 * 86_400) as u64, 14.0));
    }

    #[test]
    fn unparseable_timestamp_is_kept() {
        assert!(is_fresh_at(
            Some("garbage"),
            (REF + 999 * 86_400) as u64,
            14.0
        ));
    }

    #[test]
    fn future_timestamp_is_kept() {
        assert!(is_fresh_at(Some("2099-01-01T00:00:00Z"), REF as u64, 14.0));
    }
}
