# Phase 1 — Calibrated-Range Fallback with Abstention Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When live trade2 ablation can't price a `/paste` item (no listings *or* errored/rate-limited), fall back to a corpus-derived **price range** (floor/fair/ask) with band-width confidence — or **abstain**. Live ablation stays primary.

**Architecture:** Repurpose the unused per-`(league,category)` k-NN point estimator into a range estimator: `CategoryModel::range_estimate` builds an **exact-mod-set-first, adaptive-K** comparable pool, emits p20/p50/p80 with width-confidence, and abstains on a thin/dissimilar pool or a top-decile result. `TradePricer::range_estimate` (replacing `learned_estimate`) is called from `/paste` only when live ablation yields nothing.

**Tech Stack:** Rust, no new deps.

## Global Constraints

- Binary crate, **no lib target** — `cargo test` / `cargo test <name>`, **never** `cargo test --lib`.
- CI runs `cargo clippy --all-targets -- -D warnings` (toolchain 1.96) — keep clean; `cargo fmt` before each commit. (`is_multiple_of` is stable ≥1.87 and preferred by clippy 1.96 — do not "fix" it to `% 2`.)
- Provisional constants (exact values, verbatim): `MIN_POOL = 8`, `RELAX_JACCARD = 0.6`; quantiles **p20/p50/p80** → floor/fair/ask; confidence by `ask/floor`: ≤2× High, ≤5× Medium, else Low. These are starting defaults, **not tuned to any price prior** (Phase 2 calibrates).
- **Roll magnitude is NOT used for pool selection** (proved non-predictive); pool membership is mod-SET (stat_id set) only. No recency weighting (the 14-day freshness filter at build already applies).
- `/paste` **live-success** output is unchanged; the range surfaces ONLY on live-empty / live-errored.
- Stage files by name; commit messages end with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

---

### Task 1: Range estimator core (`estimate.rs` + `CategoryModel`)

**Files:**
- Modify: `src/trade/value/estimate.rs` (add `RangeEstimate`, `range_estimate`, `percentile_sorted`; remove the point `estimate` + `relative_spread`. **Keep `ValueEstimate` for now** — it's still referenced by `learned_estimate` (Task 2) and `estimate_embed` (Task 3 removes both, then deletes the type).)
- Modify: `src/trade/value/mod.rs` (add `MIN_POOL`/`RELAX_JACCARD` consts; add `top_decile_price` field to `CategoryModel`; compute it in `build_category`)

**Interfaces:**
- Consumes: `ItemVector { mods: Vec<(String, Option<f64>)>, price_divine: f64 }`, `Confidence` (existing), `K_NEIGHBORS`/`MIN_NEIGHBORS` (only the point `estimate` used these — both are being removed here; `weighted_median` stays for `backtest`).
- Produces: `pub struct RangeEstimate { pub floor: f64, pub fair: f64, pub ask: f64, pub confidence: Confidence, pub pool: usize }`; `pub fn CategoryModel::range_estimate(&self, query: &[(String, Option<f64>)]) -> Option<RangeEstimate>`; `CategoryModel.top_decile_price: Option<f64>`.

- [ ] **Step 1: Write failing tests** (in `src/trade/value/estimate.rs` `#[cfg(test)] mod tests`)

```rust
fn iv(stats: &[&str], price: f64) -> ItemVector {
    ItemVector { mods: stats.iter().map(|s| ((*s).to_string(), None)).collect(), price_divine: price }
}
fn model_with(items: Vec<ItemVector>, top_decile: Option<f64>) -> crate::trade::value::CategoryModel {
    crate::trade::value::CategoryModel { items, top_decile_price: top_decile, ..Default::default() }
}

#[test]
fn range_estimate_uses_exact_mod_set_pool() {
    // 10 items with exact mod-set {a,b} priced 10..100; plus dissimilar {c} items.
    let mut items: Vec<ItemVector> = (1..=10).map(|i| iv(&["a","b"], i as f64 * 10.0)).collect();
    for _ in 0..10 { items.push(iv(&["c"], 1.0)); }
    let m = model_with(items, None);
    let q = vec![("a".to_string(), None), ("b".to_string(), None)];
    let r = m.range_estimate(&q).expect("exact pool has >= MIN_POOL");
    // p20/p50/p80 of 10..100 — dissimilar {c} items excluded.
    assert!(r.floor >= 10.0 && r.ask <= 100.0 && r.floor < r.fair && r.fair < r.ask, "{r:?}");
    assert_eq!(r.pool, 10, "only exact-mod-set items in the pool");
}

#[test]
fn range_estimate_relaxes_when_exact_too_thin() {
    // Only 2 exact {a,b,c}; relax to Jaccard>=0.6 neighbours {a,b} (J=2/3=0.67).
    let mut items = vec![iv(&["a","b","c"], 50.0), iv(&["a","b","c"], 60.0)];
    for i in 0..10 { items.push(iv(&["a","b"], 40.0 + i as f64)); } // J({a,b},{a,b,c}) = 2/3
    items.push(iv(&["x"], 999.0)); // J=0, excluded
    let m = model_with(items, None);
    let q = vec![("a".into(), None), ("b".into(), None), ("c".into(), None)];
    let r = m.range_estimate(&q).expect("relaxed pool has >= MIN_POOL");
    assert!(r.pool >= 8 && r.ask < 999.0, "relaxed pool excludes the J=0 item: {r:?}");
}

#[test]
fn range_estimate_abstains_on_thin_dissimilar_pool() {
    // Query shares nothing with the corpus → no exact, no Jaccard>=0.6 → abstain.
    let items: Vec<ItemVector> = (0..20).map(|_| iv(&["a"], 5.0)).collect();
    let m = model_with(items, None);
    let q = vec![("zzz".into(), None)];
    assert!(m.range_estimate(&q).is_none(), "no credible comparable pool → abstain");
}

#[test]
fn range_estimate_abstains_on_top_decile() {
    // Exact pool exists but its fair (p50) is at/above the category top decile → abstain.
    let items: Vec<ItemVector> = (0..12).map(|_| iv(&["a","b"], 500.0)).collect();
    let m = model_with(items, Some(400.0)); // top_decile_price = 400 < fair 500
    let q = vec![("a".into(), None), ("b".into(), None)];
    assert!(m.range_estimate(&q).is_none(), "fair >= top decile → abstain, route to live");
}

#[test]
fn range_estimate_confidence_from_band_width() {
    let tight: Vec<ItemVector> = (0..12).map(|i| iv(&["a"], 100.0 + i as f64)).collect(); // ~flat → ask<=2x floor
    let r = model_with(tight, None).range_estimate(&[("a".into(), None)]).unwrap();
    assert_eq!(r.confidence, Confidence::High, "narrow band → High: {r:?}");
}

#[test]
fn range_estimate_ignores_roll_for_pool_membership() {
    // Same mod-set, different rolls: both in the exact pool (roll doesn't gate membership).
    let items = vec![
        ItemVector { mods: vec![("a".into(), Some(0.1))], price_divine: 10.0 },
        ItemVector { mods: vec![("a".into(), Some(0.9))], price_divine: 20.0 },
    ];
    let mut all = items; for i in 0..10 { all.push(iv(&["a"], 12.0 + i as f64)); }
    let m = model_with(all, None);
    let r = m.range_estimate(&[("a".into(), Some(0.5))]).expect("pool by mod-set, roll ignored");
    assert!(r.pool >= 12, "all {{a}} items pooled regardless of roll: {r:?}");
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test range_estimate`
Expected: FAIL — `range_estimate`, `RangeEstimate`, `top_decile_price` don't exist.

- [ ] **Step 3: Add consts + `CategoryModel.top_decile_price`** (`src/trade/value/mod.rs`)

After the existing consts (near `MIN_NEIGHBORS`), add:
```rust
/// Minimum comparable-pool size for a corpus range; below it, abstain.
pub const MIN_POOL: usize = 8;
/// When the exact-mod-set pool is thinner than MIN_POOL, relax to neighbours with at
/// least this Jaccard overlap of mod-sets.
pub const RELAX_JACCARD: f64 = 0.6;
```
Add to the `CategoryModel` struct (after `calibration`):
```rust
    /// p90 of this category's prices; the range estimator abstains when a query's
    /// `fair` lands at/above it (the corpus underprices the expensive tail).
    pub top_decile_price: Option<f64>,
```
In `build_category`, after `let items = itemvec::build_item_vectors(...)`, compute and include it:
```rust
    let top_decile_price = {
        let mut ps: Vec<f64> = items.iter().map(|it| it.price_divine).collect();
        if ps.is_empty() {
            None
        } else {
            ps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            Some(estimate::percentile_sorted(&ps, 0.90))
        }
    };
```
and add `top_decile_price,` to the `CategoryModel { … }` literal.

- [ ] **Step 4: Add `percentile_sorted`, `RangeEstimate`, `range_estimate`; remove the point estimator** (`src/trade/value/estimate.rs`)

Add the helper (pub(crate) so `build_category` can call it) and the range type/method, and **delete** `relative_spread`, the point `estimate`, and `ValueEstimate` (replaced):
```rust
/// Linear-interpolation percentile of an ascending-sorted slice. `p` in [0,1].
/// Matches the live ablation's percentile method so the fallback range reads
/// consistently with live prices.
pub(crate) fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    match sorted.len() {
        0 => 0.0,
        1 => sorted[0],
        n => {
            let rank = p * (n - 1) as f64;
            let lo = rank.floor() as usize;
            let hi = rank.ceil() as usize;
            sorted[lo] + (sorted[hi] - sorted[lo]) * (rank - lo as f64)
        }
    }
}

/// A corpus-derived price range (floor/fair/ask = p20/p50/p80) with band-width
/// confidence. The secondary `/paste` fallback when live ablation yields nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct RangeEstimate {
    pub floor: f64,
    pub fair: f64,
    pub ask: f64,
    pub confidence: Confidence,
    pub pool: usize,
}

impl crate::trade::value::CategoryModel {
    /// Range estimate from an exact-mod-set-first, adaptive-K comparable pool, or
    /// `None` (abstain) on a thin/dissimilar pool or a top-decile result. Pool
    /// membership is by mod-SET only (roll is not a price-shifter).
    pub fn range_estimate(&self, query: &[(String, Option<f64>)]) -> Option<RangeEstimate> {
        use crate::trade::value::{MIN_POOL, RELAX_JACCARD};
        use std::collections::HashSet;
        if self.items.is_empty() || query.is_empty() {
            return None;
        }
        let qset: HashSet<&str> = query.iter().map(|(s, _)| s.as_str()).collect();
        let jaccard = |it: &super::itemvec::ItemVector| -> f64 {
            let iset: HashSet<&str> = it.mods.iter().map(|(s, _)| s.as_str()).collect();
            let inter = qset.iter().filter(|s| iset.contains(**s)).count();
            let union = qset.len() + iset.len() - inter;
            if union == 0 { 0.0 } else { inter as f64 / union as f64 }
        };
        // Exact mod-set first (Jaccard == 1.0); relax to Jaccard >= RELAX_JACCARD only
        // if the exact pool is thinner than MIN_POOL. Adaptive K: take ALL that qualify.
        let mut pool: Vec<f64> = self
            .items
            .iter()
            .filter(|it| jaccard(it) >= 1.0)
            .map(|it| it.price_divine)
            .collect();
        if pool.len() < MIN_POOL {
            pool = self
                .items
                .iter()
                .filter(|it| jaccard(it) >= RELAX_JACCARD)
                .map(|it| it.price_divine)
                .collect();
        }
        if pool.len() < MIN_POOL {
            return None; // abstain: no credible comparable pool
        }
        pool.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let floor = percentile_sorted(&pool, 0.20);
        let fair = percentile_sorted(&pool, 0.50);
        let ask = percentile_sorted(&pool, 0.80);
        if let Some(td) = self.top_decile_price {
            if fair >= td {
                return None; // abstain: corpus underprices the expensive tail → live
            }
        }
        let confidence = if floor > 0.0 && ask <= 2.0 * floor {
            Confidence::High
        } else if floor > 0.0 && ask <= 5.0 * floor {
            Confidence::Medium
        } else {
            Confidence::Low
        };
        Some(RangeEstimate { floor, fair, ask, confidence, pool: pool.len() })
    }
}
```
Then **remove** the now-replaced point machinery in `estimate.rs`: the `relative_spread` fn (and its `#[allow(dead_code)]`) and `CategoryModel::estimate` (and its `#[allow]`). **Do NOT delete `ValueEstimate` yet** — `learned_estimate` (Task 2) and `estimate_embed` (Task 3) still reference it; Task 3 deletes it once those references are gone. Remove or migrate any `estimate.rs` tests that exercised the point `estimate` to the new `range_estimate` tests above. Keep `weighted_median` (used by `backtest`) and `query_from_stats` (used in Task 2) — and drop `query_from_stats`'s `#[allow(dead_code)]` only once Task 2 wires it (do it in Task 2).

- [ ] **Step 5: Run tests**

Run: `cargo test range_estimate && cargo test percentile`
Expected: PASS. (Whole-crate build may still fail on `learned_estimate` referencing the removed `estimate` — Task 2 fixes that caller; note it, like the Phase-0 sequencing.)

- [ ] **Step 6: fmt + commit**

```bash
cargo fmt
git add src/trade/value/estimate.rs src/trade/value/mod.rs
git commit -m "feat(value): range_estimate (exact-first adaptive-K pool, p20/p50/p80, abstention)"
```

---

### Task 2: `TradePricer::range_estimate` replaces `learned_estimate` (`trade/mod.rs`)

**Files:**
- Modify: `src/trade/mod.rs` (`learned_estimate` → `range_estimate`; its trust-bar tests)

**Interfaces:**
- Consumes: `CategoryModel::{is_trusted, query_from_stats, range_estimate}`, `RangeEstimate` (Task 1).
- Produces: `pub fn TradePricer::range_estimate(&self, item: &ParsedItem, league: &str) -> Option<crate::trade::value::estimate::RangeEstimate>`.

- [ ] **Step 1: Update the trust-bar tests** (in `src/trade/mod.rs` tests) — they currently call `learned_estimate` and assert a point `ValueEstimate`. Change them to call `range_estimate` and assert a `RangeEstimate` (e.g. `let r = pricer.range_estimate(&item, "Standard").expect(...); assert!(r.floor <= r.fair && r.fair <= r.ask);`), and the untrusted/None cases assert `range_estimate(...).is_none()`. Keep the `trusted_*` fixture (roll-correlated corpus) — it produces a pool.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test range_estimate -- --include-ignored` (and the trust-bar test names)
Expected: FAIL — `TradePricer::range_estimate` doesn't exist; `learned_estimate` still returns the removed `ValueEstimate`.

- [ ] **Step 3: Replace `learned_estimate` with `range_estimate`** (`src/trade/mod.rs`)

Rename the method and change steps 4–6 to return a range; **remove** the `#[allow(dead_code)]` (now wired to `/paste` in Task 3) and update the doc comment:
```rust
    /// Corpus-derived price RANGE for `item` in `league`, or `None` (abstain) when:
    /// item class absent, no model for the league, the category is not trusted, or the
    /// comparable pool is thin/dissimilar / top-decile. Surfaced on `/paste` only as the
    /// fallback when live ablation yields nothing. Synchronous (in-memory read only).
    pub fn range_estimate(
        &self,
        item: &ParsedItem,
        league: &str,
    ) -> Option<crate::trade::value::estimate::RangeEstimate> {
        let canon = crate::trade::value::canonical_category(item.item_class.as_deref()?);
        let model = self.value.read().unwrap_or_else(|e| e.into_inner());
        let cat = model.category(league, &canon)?;
        if !cat.is_trusted() {
            return None;
        }
        // Explicit mods only (corpus stores explicits; mixing implicits/runes would
        // desync the query's mod-set from the corpus's — see the original invariant).
        let resolved: Vec<(String, Option<f64>)> = item
            .explicits
            .iter()
            .filter_map(|m| {
                let id = self
                    .catalog
                    .match_stat(&m.raw, crate::trade::stats::StatGroup::Explicit)?;
                Some((id, m.value))
            })
            .collect();
        let query = cat.query_from_stats(&resolved);
        cat.range_estimate(&query)
    }
```
Drop the `#[allow(dead_code)]` on `CategoryModel::query_from_stats` in `estimate.rs` (now reachable via this path).

- [ ] **Step 4: Run tests + whole crate**

Run: `cargo test` then `cargo clippy --all-targets -- -D warnings`
Expected: all pass; clippy clean. `query_from_stats`'s allow is gone (now wired). `ValueEstimate` is still present (referenced by `estimate_embed`) and may still carry its `#[allow(dead_code)]`; Task 3 deletes it. The point `estimate`/`relative_spread` are gone, no dangling refs.

- [ ] **Step 5: fmt + commit**

```bash
cargo fmt
git add src/trade/mod.rs src/trade/value/estimate.rs
git commit -m "feat(value): TradePricer::range_estimate replaces learned_estimate"
```

---

### Task 3: `/paste` range fallback on empty/errored ablation (`paste.rs` + `embeds.rs`)

**Files:**
- Modify: `src/discord/embeds.rs` (add `range_fallback_line` pure helper + test)
- Modify: `src/discord/paste.rs` (`run_pricing`: call `range_estimate` on the `Err` branch and the `listing_count == 0` case)

**Interfaces:**
- Consumes: `TradePricer::range_estimate` (Task 2), `RangeEstimate`, `Confidence`.

- [ ] **Step 1: Write the failing test for the render helper** (`src/discord/embeds.rs` tests)

```rust
#[test]
fn range_fallback_line_shows_floor_fair_ask_and_confidence() {
    use crate::trade::value::estimate::{Confidence, RangeEstimate};
    let r = RangeEstimate { floor: 5.0, fair: 12.0, ask: 30.0, confidence: Confidence::Low, pool: 14 };
    let line = super::range_fallback_line("Chiming Staff", &r);
    assert!(line.contains("Chiming Staff"), "{line}");
    assert!(line.contains("No live listings"), "{line}");
    assert!(line.contains('5') && line.contains("30") && line.contains("12"), "floor/ask/fair: {line}");
    assert!(line.to_lowercase().contains("low"), "confidence label: {line}");
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test range_fallback_line`
Expected: FAIL — `range_fallback_line` not found.

- [ ] **Step 3: Implement the render helper** (`src/discord/embeds.rs`)

```rust
/// Secondary `/paste` fallback line shown when live ablation has no price: a corpus
/// range labelled with its confidence. Takes the item name (not `ParsedItem`) so it is
/// trivially testable.
pub fn range_fallback_line(item_name: &str, r: &crate::trade::value::estimate::RangeEstimate) -> String {
    use crate::trade::value::estimate::Confidence;
    let conf = match r.confidence {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    };
    format!(
        "📊 **{}** — no live listings · corpus estimate **{:.0}–{:.0} div** (fair ~{:.0}) · {} confidence",
        item_name, r.floor, r.ask, r.fair, conf
    )
}
```

- [ ] **Step 4: Wire into `run_pricing`** (`src/discord/paste.rs`)

In the `Err` branch (currently editing the reply to "Couldn't reach trade right now — try again shortly."), first try the corpus range:
```rust
        Err(e) => {
            tracing::warn!(error = %e, "trade price failed");
            let content = match pricer.range_estimate(parsed, &league.name) {
                Some(r) => embeds::range_fallback_line(&parsed.name, &r),
                None => "Couldn't reach trade right now — try again shortly.".to_string(),
            };
            reply
                .edit(*ctx, poise::CreateReply::default().content(content))
                .await?;
            return Ok(());
        }
```
For the **empty** case: after `est` is bound and before/at the point the embed would show "No comparable listings found" (i.e. when `est.listing_count == 0`), short-circuit to the range fallback (it is more useful than the empty embed):
```rust
    if est.listing_count == 0 {
        let content = match pricer.range_estimate(parsed, &league.name) {
            Some(r) => embeds::range_fallback_line(&parsed.name, &r),
            None => "No comparable listings found.".to_string(),
        };
        reply
            .edit(*ctx, poise::CreateReply::default().content(content).components(vec![]))
            .await?;
        return Ok(());
    }
```
Place this `listing_count == 0` block after the `is_sub_priceable()` short-circuit (sub-1-div still wins) and before the `secondary_rate`/embed construction. The live-success path (`listing_count > 0`) is unchanged.

- [ ] **Step 5: Remove the now-dead learned-line param from `estimate_embed` + delete `ValueEstimate`**

Since Phase 0, every `estimate_embed` call passes `None` for the `learned: Option<&ValueEstimate>` argument, and the empty case is now handled by the range fallback above — so the learned-line code in `estimate_embed` is fully dead. In `src/discord/embeds.rs`:
- Drop the `learned: Option<&crate::trade::value::estimate::ValueEstimate>` parameter from `estimate_embed`, and remove the "Learned (corpus)" field block + the `learned_line`/divergence-from-learned helper(s) it used (the `live` divergence basis tied to `learned`). Keep the `listing_count == 0` → "No comparable listings found" field as-is (still reachable via other embed callers, if any; the `/paste` empty case now short-circuits before building this embed, but leave the field for safety).
- Update the three `estimate_embed(parsed, &est, league, secondary_rate, None)` call sites in `src/discord/paste.rs` to drop the trailing `None` argument.
Then in `src/trade/value/estimate.rs`, **delete the `ValueEstimate` struct** (and any remaining `#[allow(dead_code)]` on it) — it now has no references. Confirm with `grep -rn ValueEstimate src/` returning nothing.

- [ ] **Step 6: Run tests + clippy**

Run: `cargo test` then `cargo clippy --all-targets -- -D warnings`
Expected: all pass; clippy clean. **No `#[allow(dead_code)]` remain anywhere in the value module** (verify: `grep -rn "allow(dead_code)" src/trade/`). The `/paste` wiring is glue verified by compilation + the `range_fallback_line` unit test; confirm with a real no-listing paste after deploy.

- [ ] **Step 7: fmt + commit**

```bash
cargo fmt
git add src/discord/embeds.rs src/discord/paste.rs src/trade/value/estimate.rs
git commit -m "feat(paste): corpus range fallback on empty/errored live ablation; drop dead learned-line path"
```

---

## Notes for implementer / reviewer

- **No `#[allow(dead_code)]` should remain in the value module** after Task 2 — the range path makes `query_from_stats` live, and the point `estimate`/`ValueEstimate`/`relative_spread` are deleted (not allowed).
- **Provisional thresholds** (`MIN_POOL=8`, `RELAX_JACCARD=0.6`, width 2×/5×, p20/p50/p80) are honest starting defaults; Phase 2's conformal calibration tunes them. Don't tune them to any expected price.
- **Out of scope:** conformal calibration (Phase 2), the Staff-only coarse tier (Phase 3), the temporal split + trade2-empty telemetry (Phase 4), and any change to `/farm`, `/insights`, or the live-success `/paste` path.
