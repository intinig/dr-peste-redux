# Corpus Filter + Sub-1-div Floor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make value-model categories trustable by removing corpus noise — a shared `is_priceable` price-band predicate drops sub-1-div + absurd-troll rows at capture and retroactively at consumption, the rebuild learns only from timestamped fresh rows, and `/paste` reports sub-1-div items as "under 1 divine" instead of estimating.

**Architecture:** One new pure module (`src/trade/quality.rs`) holds the predicate and the two constants. The capture path (`client::parse_fetch`) and the value-model rebuild (`value::rebuild_into`) both call it; the rebuild additionally requires a present+parseable+fresh timestamp. `/paste` branches on a new `PriceEstimate::is_sub_priceable()` before rendering.

**Tech Stack:** Rust (no new dependencies). `serde_json` for fixtures, `tempfile` + `ObservationLog` for corpus tests (both already used in the suite).

## Global Constraints

- This is a **binary crate with no lib target** — run `cargo test` / `cargo test <name>`, **never** `cargo test --lib`.
- CI runs `cargo clippy --all-targets -- -D warnings` on a current toolchain (1.96) — keep clippy clean; run `cargo fmt` before every commit.
- `MIN_PRICEABLE_DIVINE = 1.0` (inclusive floor), `ABSURD_DIVINE_CAP = 100_000.0` (exclusive upper) — exact values, verbatim.
- `MIN_PRICEABLE_DIVINE` is a product floor on what we bother pricing, **not** a tuned model parameter — no code path may move it to match observed or target prices.
- Filters are applied at capture **and** consumption (so the existing on-disk corpus is cleaned without a re-harvest); raw JSONL rows are **never** deleted (Keep+Filter).
- Stage files by name in commits — never `git add -A`. End commit messages with the `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` trailer.

---

### Task 1: Price-quality predicate (`src/trade/quality.rs`)

**Files:**
- Create: `src/trade/quality.rs`
- Modify: `src/trade/mod.rs` (add `pub mod quality;` between `pub mod pseudo;` and `pub mod query;`)

**Interfaces:**
- Produces: `pub const MIN_PRICEABLE_DIVINE: f64 = 1.0;`, `pub const ABSURD_DIVINE_CAP: f64 = 100_000.0;`, `pub fn is_priceable(price_divine: f64) -> bool` (true iff `MIN_PRICEABLE_DIVINE <= price_divine < ABSURD_DIVINE_CAP`).

- [ ] **Step 1: Create the module with the predicate, constants, and tests**

Create `src/trade/quality.rs`:

```rust
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
    price_divine >= MIN_PRICEABLE_DIVINE && price_divine < ABSURD_DIVINE_CAP
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
}
```

- [ ] **Step 2: Register the module**

In `src/trade/mod.rs`, the module list is alphabetical (`ablation, age, categories, client, limiter, model, pseudo, query, rates, session, stats, value`). Insert `pub mod quality;` between `pub mod pseudo;` and `pub mod query;`.

- [ ] **Step 3: Run the tests**

Run: `cargo test quality -- --nocapture`
Expected: 3 tests pass.

- [ ] **Step 4: fmt + clippy + commit**

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
git add src/trade/quality.rs src/trade/mod.rs
git commit -m "feat(quality): is_priceable price-band predicate (1 div floor, absurd cap)"
```

---

### Task 2: Drop unpriceable rows at capture (`client::parse_fetch`)

**Files:**
- Modify: `src/trade/client.rs` (`parse_fetch`, immediately after the mirror-tier `if let Some(mirror)` block — around line 299)
- Test: `src/trade/client.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::trade::quality::is_priceable` (Task 1).

This keeps **future** harvest rows and **live ablation comparables** clean in one place, alongside the existing mirror-tier/veiled drops.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/trade/client.rs` (uses the existing `test_client()` helper, whose rates are `divine=1.0, chaos=0.1`, with **no** mirror rate — so the absurd-cap backstop is what catches the troll here):

```rust
#[test]
fn parse_fetch_drops_sub_one_div_and_absurd_listings() {
    let client = test_client();
    let v = serde_json::json!({
        "result": [
            // 0.5 div (5 chaos) → sub-1-div → dropped
            { "listing": { "price": { "amount": 5.0, "currency": "chaos" } },
              "item": { "explicitMods": ["a"] } },
            // 0.5 div (divine) → sub-1-div → dropped
            { "listing": { "price": { "amount": 0.5, "currency": "divine" } },
              "item": { "explicitMods": ["b"] } },
            // 200000 div, mirror rate unavailable → ≥ ABSURD_DIVINE_CAP → dropped
            { "listing": { "price": { "amount": 200000.0, "currency": "divine" } },
              "item": { "explicitMods": ["c"] } },
            // 3 div → in band → kept
            { "listing": { "price": { "amount": 3.0, "currency": "divine" } },
              "item": { "explicitMods": ["d", "e"] } }
        ]
    });
    let ls = client.parse_fetch(&v);
    assert_eq!(ls.len(), 1, "only the 3-div in-band listing survives");
    assert_eq!(ls[0].price_divine, 3.0);
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test parse_fetch_drops_sub_one_div_and_absurd_listings`
Expected: FAIL — currently all three of the first listings are kept (`assert_eq!(ls.len(), 1)` sees 4).

- [ ] **Step 3: Add the filter**

In `parse_fetch`, immediately after the mirror-tier block (the `if let Some(mirror) = rates.to_divine(1.0, "mirror") { ... }` that ends around line 299, before `drop(rates);`), insert:

```rust
                        // Drop listings outside the priceable band: sub-1-div items
                        // (not worth pricing or learning from) and absurd troll prices
                        // the mirror-tier filter can't catch when the mirror rate is
                        // unavailable. See `crate::trade::quality`.
                        if !crate::trade::quality::is_priceable(price_divine) {
                            return None;
                        }
```

- [ ] **Step 4: Run the new test + the whole client suite**

Run: `cargo test parse_fetch`
Expected: the new test PASSES and every existing `parse_fetch_*` test still passes (all existing fixtures use ≥ 1.0-div amounts, so none are newly dropped). If any pre-existing test that asserts mod/id parsing happens to use a sub-1-div amount, bump that fixture's amount to ≥ 1.0 div — **do not** weaken the filter.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
git add src/trade/client.rs
git commit -m "feat(capture): drop sub-1-div + absurd-troll listings in parse_fetch"
```

---

### Task 3: Drop unpriceable + undated rows at consumption (`value::rebuild_into`)

**Files:**
- Modify: `src/trade/value/mod.rs` (`rebuild_into`, the `fresh` filter around lines 202–207)
- Test: `src/trade/value/mod.rs` (existing `#[cfg(test)] mod tests`, alongside `rebuild_into_drops_stale_observations`)

**Interfaces:**
- Consumes: `crate::trade::quality::is_priceable` (Task 1); `crate::trade::age::{parse_indexed, is_fresh_at, now_unix, MAX_LISTING_AGE_DAYS}` (existing).

This retroactively cleans the **existing** on-disk corpus on the next rebuild (startup/periodic/post-harvest) — no re-harvest, no file rewrite. `is_fresh_at`'s global semantics (absent timestamp ⇒ kept) are **unchanged** — the "timestamp required" rule is local to model learning.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/trade/value/mod.rs` (reuses the existing `ob(category, price, stats)` builder, which sets `league = "Standard"`, `category = "Staff"`, `indexed = None`):

```rust
#[test]
fn rebuild_into_drops_sub_one_div_and_undated_observations() {
    let dir = tempfile::tempdir().unwrap();
    let log = ObservationLog::new(dir.path().join("obs.jsonl"));
    // 15 clean, fresh, in-band rows → kept.
    for _ in 0..15 {
        log.append(&Observation {
            indexed: Some("2099-01-01T00:00:00Z".into()), // future → always fresh
            ..ob("Staff", 30.0, &["explicit.a"])
        })
        .unwrap();
    }
    // 5 fresh but sub-1-div rows → dropped by is_priceable.
    for _ in 0..5 {
        log.append(&Observation {
            indexed: Some("2099-01-01T00:00:00Z".into()),
            ..ob("Staff", 0.5, &["explicit.a"])
        })
        .unwrap();
    }
    // 7 undated rows (ob() defaults indexed: None) → dropped by timestamp-required rule.
    for _ in 0..7 {
        log.append(&ob("Staff", 30.0, &["explicit.a"])).unwrap();
    }
    let slot = RwLock::new(ValueModel::default());
    rebuild_into(&log, &slot, &crate::trade::stats::StatCatalog::default());
    let model = slot.read().unwrap();
    let cat = model
        .category("Standard", "Staff")
        .expect("Staff category present");
    assert_eq!(
        cat.sample_size, 15,
        "only clean, fresh, in-band, dated rows are learned from"
    );
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test rebuild_into_drops_sub_one_div_and_undated_observations`
Expected: FAIL — today undated rows are kept and sub-1-div rows aren't filtered, so `sample_size` is 27, not 15.

- [ ] **Step 3: Tighten the rebuild filter**

In `rebuild_into`, replace the existing `fresh` filter:

```rust
    let now = now_unix();
    let fresh: Vec<Observation> = log
        .read_all()
        .into_iter()
        .filter(|o| is_fresh_at(o.indexed.as_deref(), now, MAX_LISTING_AGE_DAYS))
        .collect();
```

with:

```rust
    let now = now_unix();
    let fresh: Vec<Observation> = log
        .read_all()
        .into_iter()
        // Learn only from rows that are (a) in the priceable band — sub-1-div dust
        // and absurd trolls carry no signal — and (b) positively dated as fresh.
        // Unlike the live path, the model treats an absent/unparseable timestamp as
        // NOT learnable (legacy pre-timestamp rows are cheap-biased), so a present,
        // parseable, in-window `indexed` is required.
        .filter(|o| {
            crate::trade::quality::is_priceable(o.price_divine)
                && o.indexed.as_deref().is_some_and(|t| {
                    crate::trade::age::parse_indexed(t).is_some()
                        && is_fresh_at(Some(t), now, MAX_LISTING_AGE_DAYS)
                })
        })
        .collect();
```

(The `parse_indexed(t).is_some()` guard is what makes unparseable timestamps non-learnable, since `is_fresh_at` alone would keep them.)

- [ ] **Step 4: Run the new test + the existing rebuild test**

Run: `cargo test rebuild_into_`
Expected: the new test PASSES; `rebuild_into_drops_stale_observations` still PASSES (its rows are all dated and in-band, so it is unaffected).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
git add src/trade/value/mod.rs
git commit -m "feat(value): learn only from priceable, dated, fresh rows in rebuild_into"
```

---

### Task 4: `/paste` sub-1-div short-circuit

**Files:**
- Modify: `src/trade/model.rs` (add `impl PriceEstimate` with `is_sub_priceable`, after the `PriceEstimate` struct ~line 162)
- Modify: `src/discord/embeds.rs` (add `sub_one_div_message`; test in its `#[cfg(test)] mod tests` ~line 271)
- Modify: `src/discord/paste.rs` (`run_pricing`, after `est` is bound ~line 124)

**Interfaces:**
- Consumes: `crate::trade::quality::MIN_PRICEABLE_DIVINE` (Task 1); `PriceEstimate` fields `typical: f64`, `listing_count: usize`.
- Produces: `PriceEstimate::is_sub_priceable(&self) -> bool`; `embeds::sub_one_div_message(item_name: &str) -> String`.

- [ ] **Step 1: Write the failing test for `is_sub_priceable`**

Add to the `tests` module in `src/trade/model.rs` (variants confirmed: `Confidence::Medium`, `Currency::Divine`, `EstimateBasis::CraftTier`):

```rust
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
    assert!(base.is_sub_priceable(), "cheap with listings → sub-priceable");
    assert!(
        !PriceEstimate { typical: 1.0, ..base.clone() }.is_sub_priceable(),
        "exactly 1 div → priceable"
    );
    assert!(!PriceEstimate { typical: 5.0, ..base.clone() }.is_sub_priceable());
    assert!(
        !PriceEstimate { listing_count: 0, ..base.clone() }.is_sub_priceable(),
        "no listings is unknown, not cheap"
    );
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test is_sub_priceable_only_when_listings_and_cheap`
Expected: FAIL — `no method named is_sub_priceable`.

- [ ] **Step 3: Implement `is_sub_priceable`**

In `src/trade/model.rs`, after the `PriceEstimate` struct definition (ends ~line 162), add:

```rust
impl PriceEstimate {
    /// True when we have live listings but the representative (typical/p50) value is
    /// below the priceable floor — too cheap to bother estimating precisely.
    /// `listing_count == 0` (no comparable data) is NOT sub-priceable: absence of
    /// comps is not evidence of cheapness.
    pub fn is_sub_priceable(&self) -> bool {
        self.listing_count > 0 && self.typical < crate::trade::quality::MIN_PRICEABLE_DIVINE
    }
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test is_sub_priceable_only_when_listings_and_cheap`
Expected: PASS.

- [ ] **Step 5: Write the failing test for `sub_one_div_message`**

Add to the `tests` module in `src/discord/embeds.rs`:

```rust
#[test]
fn sub_one_div_message_names_the_item_and_says_under_one_div() {
    let m = super::sub_one_div_message("Chiming Staff");
    assert!(m.contains("Chiming Staff"));
    assert!(m.contains("under 1 divine"));
}
```

- [ ] **Step 6: Run it to confirm it fails**

Run: `cargo test sub_one_div_message_names_the_item`
Expected: FAIL — `cannot find function sub_one_div_message`.

- [ ] **Step 7: Implement `sub_one_div_message`**

In `src/discord/embeds.rs` (near `estimate_embed`), add:

```rust
/// Message shown on `/paste` when an item prices under 1 divine: report it's cheap
/// rather than estimating a precise sub-1-div value. Takes the item name (not the
/// whole `ParsedItem`) so it is trivially testable.
pub fn sub_one_div_message(item_name: &str) -> String {
    format!("💸 **{item_name}** — worth under 1 divine. Not worth pricing precisely.")
}
```

- [ ] **Step 8: Run the test**

Run: `cargo test sub_one_div_message_names_the_item`
Expected: PASS.

- [ ] **Step 9: Wire the short-circuit into `/paste`**

In `src/discord/paste.rs::run_pricing`, immediately after the `let est = match pricer.price(...) { ... };` block (ends ~line 124) and **before** the `secondary_rate` / `learned` computation, insert:

```rust
    // Sub-1-div items: report "too cheap to price" and stop — skip the precise
    // breakdown and the learned estimate (we don't care how cheap, just that it is).
    if est.is_sub_priceable() {
        reply
            .edit(
                *ctx,
                poise::CreateReply::default()
                    .content(embeds::sub_one_div_message(&parsed.name))
                    .components(vec![]),
            )
            .await?;
        return Ok(());
    }
```

- [ ] **Step 10: Verify the whole suite + clippy**

Run:
```bash
cargo test
cargo clippy --all-targets -- -D warnings
```
Expected: all tests pass (count = prior + 6 new across Tasks 1–4); clippy clean. The `/paste` wiring itself is 2 lines of glue verified by compilation + the two pure unit tests above; confirm behaviour with a real sub-1-div paste after deploy (note in the PR, same as the autocomplete round-trip).

- [ ] **Step 11: fmt + commit**

```bash
cargo fmt
git add src/trade/model.rs src/discord/embeds.rs src/discord/paste.rs
git commit -m "feat(paste): short-circuit sub-1-div items to 'under 1 divine'"
```

---

## Notes for the implementer / reviewer

- **No trust-bar changes.** `TRUST_MIN_SAMPLE` (80) and `TRUST_MAX_ERROR` (0.50) stay exactly as they are. The point of this work is to clean inputs, not move the bar.
- **Effect is automatic on deploy.** The next `rebuild_into` (startup) re-reads the existing JSONL through the tighter filter, so Staff et al. rebuild clean without a re-harvest. Re-measure per-category LOO afterward via `/insights`.
- **Don't special-case "999" walls** — out of scope by decision; the weighted-median k-NN absorbs the residual high outliers. Revisit only if post-clean LOO is still > 0.50.
