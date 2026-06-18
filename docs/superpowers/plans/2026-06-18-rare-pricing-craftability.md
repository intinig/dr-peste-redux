# Rare-Pricing Craftability-Tier Comparables — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make rare-item estimates reflect an item's *craftability tier* — price a 2-open-prefix base against other open-prefix bases, not against the bad-filled junk floor — and report honest, outlier-trimmed percentiles.

**Architecture:** Parse prefix/suffix from the Advanced-Mode clipboard → derive open-slot counts. Fetch a wider cheapest-first sample, capture each listing's explicit-mod count. In the estimate, keep only listings in the same craftability tier (`explicit_count ≤ ours`), trim bottom outliers, and report p20/p50/p80. Fallback ladder when no comparable bases or no affix tags.

**Tech Stack:** Rust, tokio, serde_json, the existing `src/trade/` pricing engine + `src/itemtext.rs` parser.

**Design spec:** `docs/superpowers/specs/2026-06-18-rare-pricing-craftability-design.md` (read for rationale; this plan is self-contained for implementation).

## Global Constraints

- **Value model:** for a fixed base + suffixes, `bad-filled < open < good-filled`. Open slots carry **no inherent premium** — we only ever *remove non-comparable products* from the sample, never add a multiplier.
- **Binary crate, no lib target** — run `cargo test` (never `cargo test --lib`). Tests offline by default; network tests are `#[ignore]`d.
- **Tunable constants (exact defaults):** `COMPARABLE_SAMPLE = 30`, `TRIM_BOTTOM_FRAC = 0.10`, `TRIM_MIN_N = 8`, percentiles **p20 / p50 / p80** = Quick / Fair / Patient.
- **Craftability filter rule:** keep listings with `explicit_count <= our_item_explicit_count`. Rares cap at 3 prefixes + 3 suffixes; `open_* = 3 - filled_*` saturating at 0.
- **Fallback ladder:** craftability known + ≥1 survivor → price survivors (`CraftTier`); known + 0 survivors → price full sample (`BroadMarket`); craftability unknown (basic clipboard) → price full sample (`AffixesOnly`). Never fall back to the broad pool merely for small survivor counts.
- **Regression target:** the reference boot (`RARE_BOOTS_ADVANCED`, 2 open prefixes) prices well above the floor; a bad-filled twin (higher `explicit_count`) is excluded and stays cheap.
- **Commit trailer** (end every commit message, after a blank line):
  ```
  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
  ```
- Stage files by name; never `git add -A`. Run `cargo fmt` + `cargo clippy` (clean) before each commit.

---

## File Structure

| File | Responsibility / change |
|---|---|
| `src/itemtext.rs` | `Affix` enum; `ItemStat.affix`; parse `{ Prefix/Suffix Modifier … }`; `Craftability` + `ParsedItem::craftability()` |
| `src/trade/model.rs` | `Listing.explicit_count`; `EstimateBasis` enum; `PriceEstimate.basis` |
| `src/trade/client.rs` | `parse_fetch` reads each listing's explicit-mod count |
| `src/trade/ablation.rs` | trim+percentile rework in `estimate_from`; craftability filter; `max_explicit` threaded through `estimate`/`breakdown` |
| `src/trade/mod.rs` | `LISTING_LIMIT` → `COMPARABLE_SAMPLE = 30`; `TradePricer::{price,breakdown}` derive + pass `max_explicit` |
| `src/discord/embeds.rs` | tier line in `estimate_embed` |

---

## Task 1: Parser — affix typing + craftability

**Files:**
- Modify: `src/itemtext.rs`

**Interfaces:**
- Produces:
  - `pub enum Affix { Prefix, Suffix }` (derives `Clone, Copy, Debug, PartialEq, Eq`)
  - `ItemStat` gains `pub affix: Option<Affix>`
  - `pub struct Craftability { pub filled_prefixes: u8, pub filled_suffixes: u8, pub open_prefixes: u8, pub open_suffixes: u8, pub explicit_count: u8 }` (derives `Clone, Copy, Debug, PartialEq, Eq`)
  - `impl ParsedItem { pub fn craftability(&self) -> Option<Craftability> }`

- [ ] **Step 1: Add the `Affix` enum and the `ItemStat.affix` field**

In `src/itemtext.rs`, add the enum near `Rarity` and extend `ItemStat`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affix {
    Prefix,
    Suffix,
}

/// One stat line from the clipboard, with the first numeric roll extracted.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemStat {
    pub raw: String,
    pub value: Option<f64>,
    /// Prefix/Suffix when known (Advanced-Mode `{ … Modifier … }` annotation);
    /// `None` for implicit/enchant/rune lines and for basic-clipboard pastes.
    pub affix: Option<Affix>,
}
```

- [ ] **Step 2: Track affix type while parsing, and tag explicit stats**

In `parse`, the per-line loop currently sets `affix` nowhere. Detect the
`{ Prefix Modifier … }` / `{ Suffix Modifier … }` annotation lines (currently
skipped by `is_meta_line` because they start with `{`) and carry the type onto
the following explicit stat line(s). Replace the loop body with:

```rust
    let mut current_affix: Option<Affix> = None;
    for (i, raw_line) in lines.iter().enumerate() {
        if i == idx || i == idx + 1 || i == idx + 2 {
            continue; // rarity, name, base type
        }
        // Advanced-Mode affix annotations set the type for the next stat line(s).
        if raw_line.starts_with('{') {
            let lower = raw_line.to_lowercase();
            current_affix = if lower.contains("prefix modifier") {
                Some(Affix::Prefix)
            } else if lower.contains("suffix modifier") {
                Some(Affix::Suffix)
            } else {
                None // implicit/enchant/rune/other block — not a prefix/suffix
            };
            continue;
        }
        if is_meta_line(raw_line) {
            current_affix = None; // separators/properties break an affix block
            continue;
        }
        let (clean, tag) = split_tag(raw_line);
        let stat = ItemStat {
            value: first_number(&clean),
            raw: clean,
            affix: None,
        };
        match tag.as_deref() {
            Some("implicit") => implicits.push(stat),
            Some("enchant") => enchants.push(stat),
            Some("rune") => runes.push(stat),
            _ => {
                let mut s = stat;
                s.affix = current_affix;
                explicits.push(s);
            }
        }
    }
```

Note: `is_meta_line` already returns `true` for `{`-lines, but the new explicit
`starts_with('{')` branch runs first and `continue`s, so the annotation is
consumed for its type and never reaches `is_meta_line`. Resetting
`current_affix = None` on separators prevents a later explicit from inheriting a
stale type.

- [ ] **Step 3: Add `Craftability` + `ParsedItem::craftability()`**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Craftability {
    pub filled_prefixes: u8,
    pub filled_suffixes: u8,
    pub open_prefixes: u8,
    pub open_suffixes: u8,
    /// Number of filled prefix+suffix explicit mods (the craftability-filter key).
    pub explicit_count: u8,
}

impl ParsedItem {
    /// Prefix/suffix slot usage, derived from Advanced-Mode affix tags.
    /// Returns `None` when no explicit carries an affix tag (basic clipboard),
    /// so callers can fall back to affix-content-only pricing.
    pub fn craftability(&self) -> Option<Craftability> {
        if !self.explicits.iter().any(|s| s.affix.is_some()) {
            return None;
        }
        let filled_prefixes = self
            .explicits
            .iter()
            .filter(|s| s.affix == Some(Affix::Prefix))
            .count() as u8;
        let filled_suffixes = self
            .explicits
            .iter()
            .filter(|s| s.affix == Some(Affix::Suffix))
            .count() as u8;
        Some(Craftability {
            filled_prefixes,
            filled_suffixes,
            open_prefixes: 3u8.saturating_sub(filled_prefixes),
            open_suffixes: 3u8.saturating_sub(filled_suffixes),
            explicit_count: filled_prefixes + filled_suffixes,
        })
    }
}
```

- [ ] **Step 4: Write the failing tests**

Add to the `tests` module in `src/itemtext.rs` (reuses the existing
`RARE_BOOTS_ADVANCED` and `RARE_RING` consts):

```rust
    #[test]
    fn craftability_of_advanced_boots() {
        let p = parse(RARE_BOOTS_ADVANCED).unwrap();
        let c = p.craftability().expect("advanced-mode tags present");
        assert_eq!(c.filled_prefixes, 1); // 35% Movement Speed
        assert_eq!(c.filled_suffixes, 3); // 2 resists + rarity
        assert_eq!(c.open_prefixes, 2); // the two empty prefixes
        assert_eq!(c.open_suffixes, 0);
        assert_eq!(c.explicit_count, 4);
        // affix tags landed on the right explicits
        let ms = p.explicits.iter().find(|s| s.raw.contains("Movement Speed")).unwrap();
        assert_eq!(ms.affix, Some(Affix::Prefix));
        let rarity = p.explicits.iter().find(|s| s.raw.contains("Rarity of Items found")).unwrap();
        assert_eq!(rarity.affix, Some(Affix::Suffix));
    }

    #[test]
    fn craftability_none_for_basic_clipboard() {
        // RARE_RING has no `{ … Modifier … }` annotations → no affix tags.
        let p = parse(RARE_RING).unwrap();
        assert!(p.explicits.iter().all(|s| s.affix.is_none()));
        assert!(p.craftability().is_none());
    }
```

- [ ] **Step 5: Run to verify failure**

Run: `cargo test itemtext::`
Expected: FAIL — `Affix`, `craftability`, `ItemStat.affix` not defined (and any existing `ItemStat { … }` literals missing the new field won't compile yet).

- [ ] **Step 6: Fix any in-module `ItemStat` literals**

The existing `parse` builds `ItemStat` (updated in Step 2). If the `tests`
module constructs `ItemStat` literals directly, add `affix: None`. (As of now it
does not — it parses text — so no change expected; verify by compiling.)

- [ ] **Step 7: Run to green, format, lint, commit**

Run: `cargo test itemtext::` → PASS; `cargo build` clean.

```bash
cargo fmt && cargo clippy
git add src/itemtext.rs
git commit -m "feat(itemtext): parse prefix/suffix affixes + ParsedItem::craftability()"
# + trailer
```

---

## Task 2: `Listing.explicit_count` + `parse_fetch`

**Files:**
- Modify: `src/trade/model.rs`, `src/trade/client.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `Listing` gains `pub explicit_count: usize` (number of explicit mods on the listed item; `0` when unknown).

- [ ] **Step 1: Add the field to `Listing`**

In `src/trade/model.rs`:

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct Listing {
    pub price: Money,
    /// Price normalized to Divine Orbs for comparison/ranking.
    pub price_divine: f64,
    /// Count of explicit (prefix/suffix) mods on the listed item; the
    /// craftability-tier key. `0` when the fetch response omits mods.
    pub explicit_count: usize,
}
```

- [ ] **Step 2: Update the failing `parse_fetch` test fixture**

In `src/trade/client.rs` `tests`, replace `parse_fetch_drops_unconvertible_currency_listings`
so each result carries an `item.explicitMods` array, and assert the counts:

```rust
    #[test]
    fn parse_fetch_drops_unconvertible_currency_listings() {
        let client = test_client();
        let v = serde_json::json!({
            "result": [
                { "listing": { "price": { "amount": 2.0, "currency": "divine" } },
                  "item": { "explicitMods": ["a", "b", "c"] } },
                { "listing": { "price": { "amount": 1.0, "currency": "aug" } },
                  "item": { "explicitMods": ["x"] } },
                { "listing": { "price": { "amount": 50.0, "currency": "chaos" } },
                  "item": { "explicitMods": ["p", "q", "r", "s"] } }
            ]
        });
        let listings = client.parse_fetch(&v);
        // "aug" is unconvertible → dropped; divine + chaos kept, both positive.
        assert_eq!(listings.len(), 2);
        assert!(listings.iter().all(|l| l.price_divine > 0.0));
        // explicit_count comes from item.explicitMods length
        let divine = listings.iter().find(|l| l.price.amount == 2.0).unwrap();
        assert_eq!(divine.explicit_count, 3);
        let chaos = listings.iter().find(|l| l.price.amount == 50.0).unwrap();
        assert_eq!(chaos.explicit_count, 4);
    }
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test parse_fetch_drops`
Expected: FAIL — `Listing` has no `explicit_count`; the asserts don't compile / fail.

- [ ] **Step 4: Populate `explicit_count` in `parse_fetch`**

In `src/trade/client.rs`, inside `parse_fetch`'s `filter_map` closure, after the
`price_divine` guard and before building `Money`, read the explicit-mod count
from the entry's `item`:

```rust
                        let explicit_count = entry
                            .get("item")
                            .and_then(|it| it.get("explicitMods"))
                            .and_then(|m| m.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
```

and add `explicit_count` to the returned `Listing`:

```rust
                        Some(Listing {
                            price: money,
                            price_divine,
                            explicit_count,
                        })
```

**Implementation note:** confirm the trade2 `fetch` response really uses
`item.explicitMods` (it mirrors PoE1). If a live response names it differently,
adjust the key here only; the `unwrap_or(0)` keeps unknown-mod listings harmless.

- [ ] **Step 5: Run to green; fix other `Listing` literals**

Run: `cargo test` — code that builds `Listing { price, price_divine }` now fails to compile. Fix each:
- `src/trade/ablation.rs` tests: the `fn listing(divine: f64) -> Listing` helper — add `explicit_count: 0` to its literal (this covers `FakeApi::fetch`, `FakePricer`, `CountingComparables`, which all go through it).
- Any other `Listing { … }` literal in the suite: add `explicit_count: 0`.
Re-run `cargo test` to green.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt && cargo clippy
git add src/trade/model.rs src/trade/client.rs
# + any test files whose Listing literals you updated
git commit -m "feat(trade): capture per-listing explicit-mod count in parse_fetch"
# + trailer
```

---

## Task 3: Sampling + percentile rework (`EstimateBasis`, trim, p20/p50/p80)

**Files:**
- Modify: `src/trade/model.rs` (`EstimateBasis`, `PriceEstimate.basis`), `src/trade/ablation.rs` (`estimate_from`), `src/trade/mod.rs` (`COMPARABLE_SAMPLE`)

**Interfaces:**
- Produces:
  - `pub enum EstimateBasis { CraftTier, BroadMarket, AffixesOnly }` (derives `Clone, Copy, Debug, PartialEq, Eq`)
  - `PriceEstimate` gains `pub basis: EstimateBasis`
  - `fn estimate_from(listings: &[Listing], basis: EstimateBasis) -> PriceEstimate` (trim + p20/p50/p80)
  - `pub const COMPARABLE_SAMPLE: usize = 30` (replaces `LISTING_LIMIT`)

- [ ] **Step 1: Add `EstimateBasis` and the `PriceEstimate.basis` field**

In `src/trade/model.rs`:

```rust
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
```

Add `pub basis: EstimateBasis,` to the `PriceEstimate` struct (after
`modal_currency`).

- [ ] **Step 2: Write failing tests for the new `estimate_from`**

The ablation tests already define `fn listing(divine: f64) -> Listing`. Add a
**second** helper with an explicit-count (don't rename the existing one):

```rust
    fn listing_ec(divine: f64, explicit_count: usize) -> Listing {
        Listing {
            price: Money { amount: divine, currency: Currency::Divine },
            price_divine: divine,
            explicit_count,
        }
    }

    #[test]
    fn estimate_trims_bottom_and_uses_p20_p50_p80() {
        // 10 listings 1..=10 div. Trim bottom 10% (drop the 1.0), then
        // p20/p50/p80 over [2..10]; listing_count still reports the full 10.
        let ls: Vec<Listing> = (1..=10).map(|i| listing_ec(i as f64, 4)).collect();
        let est = estimate_from(&ls, EstimateBasis::CraftTier);
        assert_eq!(est.basis, EstimateBasis::CraftTier);
        assert_eq!(est.listing_count, 10); // pre-trim comparable count
        assert!(est.low < est.typical && est.typical < est.high);
        assert!((est.typical - 6.0).abs() < 0.001); // median of [2..10] = 6
    }

    #[test]
    fn estimate_no_trim_when_below_min_n() {
        let ls = vec![listing_ec(2.0, 4), listing_ec(4.0, 4), listing_ec(6.0, 4)];
        let est = estimate_from(&ls, EstimateBasis::BroadMarket);
        assert_eq!(est.listing_count, 3); // < TRIM_MIN_N → no trim
        assert!((est.typical - 4.0).abs() < 0.001); // median of [2,4,6] = 4
    }
```

(`Money`, `Currency`, `Listing` are already imported in the ablation tests.)

- [ ] **Step 3: Run to verify failure**

Run: `cargo test ablation::tests::estimate_`
Expected: FAIL — `estimate_from` has the wrong signature / old percentiles; `EstimateBasis` not wired.

- [ ] **Step 4: Rework `estimate_from` (trim + p20/p50/p80 + basis)**

In `src/trade/ablation.rs`, add the trim constants near the top:

```rust
/// Bottom fraction of (sorted-ascending) comparables dropped as dump/troll outliers.
const TRIM_BOTTOM_FRAC: f64 = 0.10;
/// Only trim when at least this many comparables survive the craftability filter.
const TRIM_MIN_N: usize = 8;
```

Replace `estimate_from` with:

```rust
fn estimate_from(listings: &[Listing], basis: EstimateBasis) -> PriceEstimate {
    let mut prices: Vec<f64> = listings.iter().map(|l| l.price_divine).collect();
    prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Trim the cheapest outliers (dump/troll listings) when we have enough.
    let priced: &[f64] = if prices.len() >= TRIM_MIN_N {
        let drop = ((prices.len() as f64) * TRIM_BOTTOM_FRAC).floor() as usize;
        &prices[drop..]
    } else {
        &prices[..]
    };

    let (low, typical, high) = if priced.is_empty() {
        (0.0, 0.0, 0.0)
    } else {
        (
            percentile(priced, 0.20),
            percentile(priced, 0.50),
            percentile(priced, 0.80),
        )
    };
    // listing_count reports the comparable set size (pre-trim) — trimming is an
    // internal outlier guard, not a change to "how many comps we found".
    PriceEstimate {
        low,
        typical,
        high,
        listing_count: listings.len(),
        confidence: Confidence::from_count(listings.len()),
        modal_currency: modal_currency(listings),
        basis,
    }
}
```

This keeps `listing_count` = the comparable count (so the existing
`estimate_reports_typical_and_confidence` test still sees 12); only the
`low/typical/high` numbers shift with trimming + the new percentiles.

- [ ] **Step 5: Rename `LISTING_LIMIT` → `COMPARABLE_SAMPLE` and retune to 30**

In `src/trade/mod.rs`, replace:

```rust
/// Number of cheapest listings to consider per query.
const LISTING_LIMIT: usize = 10;
```

with:

```rust
/// Number of cheapest listings to fetch per query before craftability filtering.
const COMPARABLE_SAMPLE: usize = 30;
```

and update the two references in `TradePricer::price` / `breakdown` (`LISTING_LIMIT` → `COMPARABLE_SAMPLE`). (The `basis` for these calls is finalized in Task 4; for now `estimate`/`breakdown` still pass through to `estimate_from` with a default — temporarily call `estimate_from(&listings, EstimateBasis::AffixesOnly)` inside `estimate`; Task 4 replaces this with the filter+basis logic.)

- [ ] **Step 6: Make the existing call sites compile**

`estimate` currently calls `estimate_from(&listings)`. Update it to
`estimate_from(&listings, EstimateBasis::AffixesOnly)` (placeholder basis;
replaced in Task 4). Any `PriceEstimate { … }` literals in ablation/mod tests now
need `basis: EstimateBasis::AffixesOnly` (or appropriate) — add it.

- [ ] **Step 7: Run to green, format, lint, commit**

Run: `cargo test` → all pass (the two new estimate tests + existing suite, which now reflects p20/p50/p80 — update any existing estimate-value assertions that assumed p10/p25/p75).

```bash
cargo fmt && cargo clippy
git add src/trade/model.rs src/trade/ablation.rs src/trade/mod.rs
git commit -m "feat(trade): honest percentile estimate — wider sample, trim outliers, p20/p50/p80"
# + trailer
```

---

## Task 4: Craftability filter + threading + fallback + pricer wiring

**Files:**
- Modify: `src/trade/ablation.rs` (`estimate`, `breakdown`, new filter), `src/trade/mod.rs` (`TradePricer::{price,breakdown}`)

**Interfaces:**
- Consumes: `Listing.explicit_count` (Task 2), `EstimateBasis`/`estimate_from` (Task 3), `ParsedItem::craftability()` (Task 1).
- Produces:
  - `pub async fn estimate<C: Comparables + ?Sized>(c, query, limit, session, max_explicit: Option<usize>) -> Result<PriceEstimate>`
  - `pub async fn breakdown<C: Comparables + ?Sized>(c, query, limit, k, session, max_explicit: Option<usize>) -> Result<Breakdown>`
  - `fn craftability_filter(listings: &[Listing], max_explicit: usize) -> Vec<Listing>`

- [ ] **Step 1: Write failing tests for the filter + fallback ladder**

In `src/trade/ablation.rs` `tests`. The existing `FakePricer` is a unit struct
that computes prices from the query, so it can't carry arbitrary listings — add a
tiny mock that returns a fixed set (uses `listing_ec` from Task 3):

```rust
    struct FixedListings(Vec<Listing>);
    #[async_trait]
    impl Comparables for FixedListings {
        async fn comparables(
            &self,
            _q: &TradeQuery,
            _limit: usize,
            _session: &TradeSession,
        ) -> anyhow::Result<Vec<Listing>> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn craftability_filter_keeps_same_or_more_open() {
        let ls = vec![
            listing_ec(2.0, 4),  // our tier (clean base, explicit_count == ours)
            listing_ec(0.05, 6), // bad-filled (more mods) → dropped
            listing_ec(1.5, 3),  // cleaner (fewer mods) → kept
            listing_ec(0.04, 5), // more-filled → dropped
        ];
        let kept = craftability_filter(&ls, 4);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|l| l.explicit_count <= 4));
    }

    #[tokio::test]
    async fn estimate_filters_to_craft_tier_not_floor() {
        // Junk floor (cheap, 6 mods) vs open-tier bases (~2 div, 4 mods).
        // Filtering to explicit_count<=4 must ignore the floor.
        let mut ls = vec![listing_ec(0.03, 6), listing_ec(0.04, 6), listing_ec(0.05, 6)];
        ls.extend((0..8).map(|i| listing_ec(1.8 + i as f64 * 0.1, 4))); // ~1.8–2.5
        let c = FixedListings(ls);
        let est = estimate(&c, &two_stat_query(), 30, &TradeSession::for_test(), Some(4))
            .await.unwrap();
        assert_eq!(est.basis, EstimateBasis::CraftTier);
        assert!(est.typical >= 1.5, "fair {} should reflect open tier, not the 0.05 floor", est.typical);
    }

    #[tokio::test]
    async fn estimate_falls_back_to_broad_market_when_no_comparable_bases() {
        // Every listing is more-filled than ours → 0 survivors → BroadMarket.
        let c = FixedListings(vec![listing_ec(0.03, 6), listing_ec(0.04, 6), listing_ec(0.05, 6)]);
        let est = estimate(&c, &two_stat_query(), 30, &TradeSession::for_test(), Some(4))
            .await.unwrap();
        assert_eq!(est.basis, EstimateBasis::BroadMarket);
        assert!(est.typical > 0.0);
    }

    #[tokio::test]
    async fn estimate_affixes_only_when_craftability_unknown() {
        let c = FixedListings(vec![listing_ec(0.03, 6), listing_ec(2.0, 4)]);
        let est = estimate(&c, &two_stat_query(), 30, &TradeSession::for_test(), None)
            .await.unwrap();
        assert_eq!(est.basis, EstimateBasis::AffixesOnly);
    }
```

`two_stat_query()` is the existing query helper; `Comparables`, `async_trait`,
`TradeSession` are already imported in this test module.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test ablation::tests::estimate_ ablation::tests::craftability_filter`
Expected: FAIL — `craftability_filter` undefined; `estimate` arity wrong (no `max_explicit`).

- [ ] **Step 3: Add the filter and rewrite `estimate`**

In `src/trade/ablation.rs`:

```rust
/// Keep listings in the same-or-more-open craftability tier as our item:
/// those with no extra explicit mods beyond the ones the search already pinned.
fn craftability_filter(listings: &[Listing], max_explicit: usize) -> Vec<Listing> {
    listings
        .iter()
        .filter(|l| l.explicit_count <= max_explicit)
        .cloned()
        .collect()
}

pub async fn estimate<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    session: &TradeSession,
    max_explicit: Option<usize>,
) -> Result<PriceEstimate> {
    let listings = c.comparables(query, limit, session).await?;
    let est = match max_explicit {
        None => estimate_from(&listings, EstimateBasis::AffixesOnly),
        Some(max) => {
            let kept = craftability_filter(&listings, max);
            if kept.is_empty() {
                estimate_from(&listings, EstimateBasis::BroadMarket)
            } else {
                estimate_from(&kept, EstimateBasis::CraftTier)
            }
        }
    };
    Ok(est)
}
```

Add `use crate::trade::session::TradeSession;` if not already present (it is, from the session work).

- [ ] **Step 4: Thread `max_explicit` through `breakdown`**

Change `breakdown`'s signature to take `max_explicit: Option<usize>` (after `k`)
and pass it to every internal `estimate(...)` call (baseline, the per-stat loop,
and the pairwise probe):

```rust
pub async fn breakdown<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    k: usize,
    session: &TradeSession,
    max_explicit: Option<usize>,
) -> Result<Breakdown> {
    let baseline = estimate(c, query, limit, session, max_explicit).await?;
    // … per-stat loop:
    let without = estimate(c, &q, limit, session, max_explicit).await?;
    // … pairwise:
    let without_both = estimate(c, &q, limit, session, max_explicit).await?;
    // … rest unchanged
}
```

- [ ] **Step 5: Wire `TradePricer` to derive + pass `max_explicit`**

In `src/trade/mod.rs`, `TradePricer::price` and `breakdown` derive the filter key
from the parsed item's craftability:

```rust
    pub async fn price(
        &self,
        item: &ParsedItem,
        league: &str,
        session: &TradeSession,
    ) -> Result<PriceEstimate> {
        let query = build_baseline(item, &self.pseudo, &self.catalog, league);
        let max_explicit = item.craftability().map(|c| c.explicit_count as usize);
        let est = estimate(&self.comparables, &query, COMPARABLE_SAMPLE, session, max_explicit).await?;
        self.record(&query, &est);
        Ok(est)
    }

    pub async fn breakdown(
        &self,
        item: &ParsedItem,
        league: &str,
        session: &TradeSession,
    ) -> Result<Breakdown> {
        let query = build_baseline(item, &self.pseudo, &self.catalog, league);
        let max_explicit = item.craftability().map(|c| c.explicit_count as usize);
        let bd = crate::trade::ablation::breakdown(
            &self.comparables, &query, COMPARABLE_SAMPLE, TOP_K, session, max_explicit,
        )
        .await?;
        self.record(&query, &bd.baseline);
        Ok(bd)
    }
```

Add `use crate::itemtext::ParsedItem;` if needed (already imported).

- [ ] **Step 6: Update all `estimate`/`breakdown` call sites in tests**

Every existing call to `estimate(...)` / `breakdown(...)` gains a trailing
`max_explicit` arg of `None` (these tests aren't about filtering):
- `estimate_reports_typical_and_confidence` → `estimate(&FakePricer, &two_stat_query(), 10, &TradeSession::for_test(), None)` (still asserts `listing_count == 12`, `typical == 23.0` — all 12 prices are equal so trimming doesn't move the median).
- `breakdown_probes_all_stats_ranks_by_delta` → `breakdown(&fake, &q, 10, 4, &TradeSession::for_test(), None)` (call-count assertion unchanged).
- `breakdown_ranks_contributions_and_flags_synergy` → `breakdown(&FakePricer, &two_stat_query(), 10, 2, &TradeSession::for_test(), None)` (deltas unchanged — equal prices per query).

Also remove the temporary `estimate_from(&listings, EstimateBasis::AffixesOnly)`
placeholder from Task 3 Step 6 — it's now superseded by the `match max_explicit`
in Step 3.

- [ ] **Step 7: Run to green, format, lint, commit**

Run: `cargo test` → all pass (incl. the 4 new filter/fallback tests and the
existing breakdown tests with the new arg).

```bash
cargo fmt && cargo clippy
git add src/trade/ablation.rs src/trade/mod.rs
git commit -m "feat(trade): craftability-tier comparable filter + fallback ladder"
# + trailer
```

---

## Task 5: Surface the priced tier in the embed

**Files:**
- Modify: `src/discord/embeds.rs`

**Interfaces:**
- Consumes: `PriceEstimate.basis`, `ParsedItem::craftability()`.
- Produces: `fn tier_note(parsed: &ParsedItem, est: &PriceEstimate) -> String` + a field rendered by `estimate_embed`.

- [ ] **Step 1: Write the failing test for the note helper**

In `src/discord/embeds.rs` `tests` (add one if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::itemtext::parse;
    use crate::trade::model::{Confidence, Currency, EstimateBasis, PriceEstimate};

    fn est(basis: EstimateBasis, n: usize) -> PriceEstimate {
        PriceEstimate {
            low: 1.0, typical: 2.0, high: 3.0, listing_count: n,
            confidence: Confidence::from_count(n), modal_currency: Currency::Divine, basis,
        }
    }

    const BOOTS: &str = "Item Class: Boots\nRarity: Rare\nKraken Slippers\nSandsworn Sandals\n--------\nItem Level: 83\n--------\n{ Prefix Modifier \"Hellion's\" (Tier: 1) }\n35% increased Movement Speed\n{ Suffix Modifier \"of Archaeology\" (Tier: 1) }\n16% increased Rarity of Items found\n";

    #[test]
    fn tier_note_describes_craft_tier() {
        let p = parse(BOOTS).unwrap();
        let note = tier_note(&p, &est(EstimateBasis::CraftTier, 7));
        assert!(note.contains("open prefix"), "{note}");
        assert!(note.contains("7"), "{note}");
    }

    #[test]
    fn tier_note_flags_broad_market() {
        let p = parse(BOOTS).unwrap();
        let note = tier_note(&p, &est(EstimateBasis::BroadMarket, 30));
        assert!(note.to_lowercase().contains("broad-market"), "{note}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test embeds::`
Expected: FAIL — `tier_note` undefined.

- [ ] **Step 3: Implement `tier_note` and render it**

In `src/discord/embeds.rs`:

```rust
/// One-line description of which comparable set the estimate used.
fn tier_note(parsed: &ParsedItem, est: &PriceEstimate) -> String {
    use crate::trade::model::EstimateBasis::*;
    match est.basis {
        CraftTier => {
            let c = parsed.craftability();
            let open_p = c.map(|c| c.open_prefixes).unwrap_or(0);
            let open_s = c.map(|c| c.open_suffixes).unwrap_or(0);
            format!(
                "clean base · {open_p} open prefix(es), {open_s} open suffix(es) · {} comparable listings",
                est.listing_count
            )
        }
        BroadMarket => "broad-market estimate — no comparable open-base listings".to_string(),
        AffixesOnly => {
            "affixes present; craftability not detected — paste in Advanced Mode for a sharper estimate"
                .to_string()
        }
    }
}
```

In `estimate_embed`, in the `else` branch (where `listing_count != 0`), add a
field before the footer:

```rust
            .field("Priced as", tier_note(parsed, est), false)
```

(`ParsedItem` is already imported in `embeds.rs`.)

- [ ] **Step 4: Run to green, format, lint, commit**

Run: `cargo test embeds::` → PASS; `cargo test` → full suite green; `cargo build` clean (zero warnings).

```bash
cargo fmt && cargo clippy
git add src/discord/embeds.rs
git commit -m "feat(discord): show priced craftability tier in the estimate embed"
# + trailer
```

---

## Final verification (after all tasks)

- [ ] `cargo fmt --check` clean; `cargo clippy` clean; `cargo test` green (offline).
- [ ] **Manual calibration in Discord** (the real acceptance test — can't run offline): paste the reference boot in Advanced Mode and confirm Fair lands in a sane band (≈1–3 div), the embed shows `2 open prefix(es)`, and the breakdown deltas are non-zero. Paste a bad-filled variant (extra prefixes) and confirm it stays cheap / shows a different tier.
- [ ] Confirm logs never contain a POESESSID (unchanged from prior work, but the pricing path was touched).
