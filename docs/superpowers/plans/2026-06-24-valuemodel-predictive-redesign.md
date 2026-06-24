# ValueModel Predictive Redesign — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a per-`(league, category)` k-NN predictive value estimate (mod-set overlap + roll-magnitude proximity) to the corpus-mined `ValueModel`, with a descriptive decomposition, auto-detected operator-triggered targeted sampling, and a leave-one-out backtest that validates and tunes it — kept strictly secondary to live trade2 ablation.

**Architecture:** Split `src/trade/value.rs` into a `src/trade/value/` module. `CategoryModel` retains per-category item-vectors (mods + normalized rolls + price) plus per-mod roll quantiles and backtest-tuned similarity weights. A k-NN estimator scores `w_jaccard·Jaccard + w_roll·roll-proximity` and returns a similarity-weighted median price with a confidence label. A leave-one-out backtest both reports per-category error and picks each category's weights from a small grid. /paste shows the learned estimate as a secondary line; /insights gains magnitude curves + archetype labels + undersampled-gate candidates; a new targeted-harvest path pins a stat filter onto the existing adaptive sweep.

**Tech Stack:** Rust 2021, pure in-process (no ML/Python deps). serde/serde_json, anyhow, tracing, poise/serenity. Binary crate (no lib target → use `cargo test`, never `--lib`). Reads the append-only JSONL corpus via `src/observe.rs`.

## Global Constraints

- **Pure Rust, no new ML dependency.** k-NN/backtest are hand-rolled.
- **Estimate is non-additive (k-NN) and SECONDARY to live ablation.** `pricer.price()` (live trade2 ablation) stays the primary /paste number; the learned estimate is a cross-check, the fallback when live is empty, and never blocks pricing.
- **Learned estimate is shown only for categories clearing a per-category trust bar** (`sample_size ≥ TRUST_MIN_SAMPLE` AND backtest median rel-error ≤ `TRUST_MAX_ERROR`). Thin/untrusted categories show live ablation only.
- **No tuning to the operator's price prior.** Calibration is measured against held-out corpus prices + live ablation only.
- **Candidates are fresh (≤14 days).** Reuse `crate::trade::age::is_fresh_at` / `MAX_LISTING_AGE_DAYS` (already applied in `rebuild_into`).
- **Decomposition is descriptive only** — it is never an input to the estimate.
- **CI parity:** `cargo fmt`, `cargo test`, `cargo clippy --all-targets -- -D warnings` all green (CI clippy is a newer/stricter toolchain — prefer `if let Some(x)` over `is_some()`+`unwrap()`, ranges over `a>=lo && a<hi`).
- **Commits:** stage files by name (never `git add -A`); never commit secrets; end commit messages with the `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` trailer.

## Constants (define in `src/trade/value/mod.rs`, reuse existing where noted)

```rust
pub const K_NEIGHBORS: usize = 15;          // top-k for the estimate
pub const MIN_NEIGHBORS: usize = 5;         // below this (after sim>0 filter) → no estimate
pub const ROLL_QUANTILES: usize = 21;       // 0,5,...,100th percentile knots for RollStats
pub const MAGNITUDE_MIN_SAMPLE: usize = 15; // min listings carrying a mod to fit a roll curve / trust its magnitude
pub const TRUST_MIN_SAMPLE: usize = 80;     // category sample floor to show the learned estimate
pub const TRUST_MAX_ERROR: f64 = 0.50;      // category LOO median |rel error| ceiling to show the estimate
pub const DIVERGENCE_FLAG: f64 = 0.50;      // |learned-live|/live above this → flag on /paste
// existing, reused: MIN_STAT_SAMPLE=15, DRIVER_LIFT=1.5 (from value.rs)
```

The similarity-weight grid (Task 6): `[(1.0,0.0),(0.75,0.25),(0.5,0.5),(0.25,0.75),(0.0,1.0)]` (jaccard, roll), each normalized to sum 1.

---

## File Structure

The current single file `src/trade/value.rs` (~430 lines) becomes a module directory. Move the existing code verbatim into `mod.rs`, then add focused files:

- `src/trade/value/mod.rs` — module decls + `pub use`; existing `ValueModel`, `CategoryModel`, `StatValue`, `ModPair`, `canonical_category`, `build`, `rebuild_into`, `build_category`, `rank_deconfounded`, `median`, consts. New fields on `CategoryModel`. New constants above.
- `src/trade/value/magnitude.rs` — `RollStats` (per-mod roll quantiles + `normalize`), `build_mod_rolls`, `roll_price_curve`.
- `src/trade/value/itemvec.rs` — `ItemVector`, `build_item_vectors` (corpus rows → normalized vectors).
- `src/trade/value/estimate.rs` — `SimWeights`, `similarity`, `ValueEstimate`, `Confidence`, `CategoryModel::estimate`, `weighted_median`.
- `src/trade/value/backtest.rs` — `loo_median_error`, `tune_weights`.
- `src/trade/value/gates.rs` — `GateCandidate`, `detect_gates`.
- decomposition lives in `estimate.rs` (`CategoryModel::decompose`).

Integration files (modified, not created):
- `src/trade/value.rs` → deleted (content moved). `src/trade/mod.rs` already declares `pub mod value;` — that resolves to the directory once `value.rs` is gone and `value/mod.rs` exists.
- `src/trade/mod.rs` — `TradePricer` gains a `learned_estimate` helper + targeted-harvest method.
- `src/discord/embeds.rs` — `estimate_embed` adds the learned line.
- `src/discord/insights.rs` — magnitude/archetype/gates surfacing.
- `src/discord/{paste,farm,mod}.rs` or wherever `/harvest` is registered — targeted-harvest command/option.

---

## Task 0: Module split (no behavior change)

**Files:**
- Create: `src/trade/value/mod.rs` (move all of `src/trade/value.rs` into it verbatim)
- Delete: `src/trade/value.rs`

- [ ] **Step 1: Move the file.** `git mv src/trade/value.rs src/trade/value/mod.rs` (creates the dir). No content changes.
- [ ] **Step 2: Verify the module still resolves.** `src/trade/mod.rs` line 14 already says `pub mod value;`; Rust resolves `value/mod.rs`. Run `cargo build`. Expected: compiles unchanged.
- [ ] **Step 3: Run tests.** `cargo test value` — all existing value tests pass (unchanged).
- [ ] **Step 4: Commit.**
```bash
git add src/trade/value/mod.rs && git commit -m "refactor(value): move value.rs into value/ module (no behavior change)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 1: Roll magnitude normalization (`magnitude.rs`)

**Files:**
- Create: `src/trade/value/magnitude.rs`
- Modify: `src/trade/value/mod.rs` (add `pub mod magnitude;`, constants)

**Interfaces:**
- Produces: `RollStats { quantiles: Vec<f64> }`; `RollStats::from_rolls(&[f64]) -> RollStats`; `RollStats::normalize(&self, roll: f64) -> f64` (→ `[0,1]`); `build_mod_rolls(obs: &[&Observation]) -> HashMap<String, RollStats>`.

- [ ] **Step 1: Failing test.** In `magnitude.rs` `#[cfg(test)]`:
```rust
#[test]
fn normalize_maps_roll_to_percentile() {
    let rs = RollStats::from_rolls(&[10.0, 20.0, 30.0, 40.0, 50.0]);
    assert!((rs.normalize(10.0) - 0.0).abs() < 1e-9, "min → 0");
    assert!((rs.normalize(50.0) - 1.0).abs() < 1e-9, "max → 1");
    assert!((rs.normalize(30.0) - 0.5).abs() < 0.05, "median ≈ 0.5");
    assert_eq!(rs.normalize(5.0), 0.0, "below min clamps to 0");
    assert_eq!(rs.normalize(99.0), 1.0, "above max clamps to 1");
}

#[test]
fn single_value_normalizes_to_zero() {
    let rs = RollStats::from_rolls(&[7.0]);
    assert_eq!(rs.normalize(7.0), 0.0); // degenerate range → no magnitude signal
}
```
- [ ] **Step 2: Run, expect FAIL** (`RollStats` undefined): `cargo test magnitude::`
- [ ] **Step 3: Implement.**
```rust
//! Per-(category, mod) roll-magnitude normalization. Maps a rolled value to its
//! percentile within the corpus distribution of that mod, so similarity can treat
//! "high roll" comparably across mods with different scales.
use crate::observe::Observation;
use std::collections::HashMap;
use super::ROLL_QUANTILES;

#[derive(Debug, Clone, Default)]
pub struct RollStats {
    /// Evenly-spaced quantile knots (ROLL_QUANTILES of them), ascending.
    pub quantiles: Vec<f64>,
}

impl RollStats {
    pub fn from_rolls(rolls: &[f64]) -> RollStats {
        let mut v: Vec<f64> = rolls.iter().copied().filter(|r| r.is_finite()).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if v.len() < 2 {
            return RollStats { quantiles: v };
        }
        let n = v.len();
        let quantiles = (0..ROLL_QUANTILES)
            .map(|i| {
                let p = i as f64 / (ROLL_QUANTILES - 1) as f64;
                let idx = (p * (n - 1) as f64).round() as usize;
                v[idx.min(n - 1)]
            })
            .collect();
        RollStats { quantiles }
    }

    /// Percentile of `roll` in [0,1]; 0.0 if the distribution is degenerate.
    pub fn normalize(&self, roll: f64) -> f64 {
        let q = &self.quantiles;
        if q.len() < 2 || q[q.len() - 1] <= q[0] {
            return 0.0;
        }
        if roll <= q[0] { return 0.0; }
        if roll >= q[q.len() - 1] { return 1.0; }
        // linear interp between the bracketing knots
        for w in q.windows(2).enumerate() {
            let (i, pair) = w;
            if roll <= pair[1] {
                let frac = if pair[1] > pair[0] { (roll - pair[0]) / (pair[1] - pair[0]) } else { 0.0 };
                return (i as f64 + frac) / (q.len() - 1) as f64;
            }
        }
        1.0
    }
}

pub fn build_mod_rolls(obs: &[&Observation]) -> HashMap<String, RollStats> {
    let mut rolls: HashMap<&str, Vec<f64>> = HashMap::new();
    for o in obs {
        for m in &o.mods {
            if let Some(r) = m.roll {
                rolls.entry(m.stat_id.as_str()).or_default().push(r);
            }
        }
    }
    rolls.into_iter().map(|(k, v)| (k.to_string(), RollStats::from_rolls(&v))).collect()
}
```
- [ ] **Step 4: Run, expect PASS.** `cargo test magnitude::`
- [ ] **Step 5: Commit** (`git add src/trade/value/magnitude.rs src/trade/value/mod.rs`).

---

## Task 2: Item vectors retained per category (`itemvec.rs` + `CategoryModel` fields)

**Files:**
- Create: `src/trade/value/itemvec.rs`
- Modify: `src/trade/value/mod.rs` (`pub mod itemvec;`; add fields to `CategoryModel`; populate in `build_category`)

**Interfaces:**
- Consumes: `RollStats`, `build_mod_rolls` (Task 1).
- Produces: `ItemVector { mods: Vec<(String, Option<f64>)>, price_divine: f64 }` (stat_id, normalized roll); `build_item_vectors(obs, &mod_rolls) -> Vec<ItemVector>`. New `CategoryModel` fields: `pub mod_rolls: HashMap<String, RollStats>`, `pub items: Vec<ItemVector>`.

- [ ] **Step 1: Failing test** (`itemvec.rs`):
```rust
#[test]
fn item_vectors_carry_normalized_rolls() {
    use crate::observe::{Observation, Source};
    use crate::trade::model::ListingMod;
    let mk = |roll: f64, price: f64| Observation {
        timestamp_unix: 0, league: "L".into(), base_type: None, category: Some("Ring".into()),
        mods: vec![ListingMod { stat_id: "explicit.a".into(), tier: None, roll: Some(roll) }],
        price_divine: price, source: Source::Harvest, indexed: None,
    };
    let obs = vec![mk(10.0, 1.0), mk(30.0, 2.0), mk(50.0, 3.0)];
    let refs: Vec<&Observation> = obs.iter().collect();
    let mr = super::magnitude::build_mod_rolls(&refs);
    let vecs = build_item_vectors(&refs, &mr);
    assert_eq!(vecs.len(), 3);
    let hi = vecs.iter().find(|v| v.price_divine == 3.0).unwrap();
    assert!((hi.mods[0].1.unwrap() - 1.0).abs() < 1e-9, "roll 50 → norm 1.0");
}
```
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement `itemvec.rs`.**
```rust
//! Per-category corpus item-vectors retained in the model for k-NN: each mod's
//! stat_id paired with its roll normalized to a percentile (None when the mod has
//! no rolled value).
use crate::observe::Observation;
use super::magnitude::RollStats;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ItemVector {
    pub mods: Vec<(String, Option<f64>)>,
    pub price_divine: f64,
}

pub fn build_item_vectors(obs: &[&Observation], mod_rolls: &HashMap<String, RollStats>) -> Vec<ItemVector> {
    obs.iter()
        .map(|o| ItemVector {
            mods: o.mods.iter().map(|m| {
                let norm = m.roll.and_then(|r| mod_rolls.get(&m.stat_id).map(|rs| rs.normalize(r)));
                (m.stat_id.clone(), norm)
            }).collect(),
            price_divine: o.price_divine,
        })
        .collect()
}
```
- [ ] **Step 4: Add fields + populate.** In `mod.rs`, add to `CategoryModel`:
```rust
    pub mod_rolls: std::collections::HashMap<String, crate::trade::value::magnitude::RollStats>,
    pub items: Vec<crate::trade::value::itemvec::ItemVector>,
    pub weights: crate::trade::value::estimate::SimWeights, // set in Task 6; Default for now
    pub undersampled_gates: Vec<crate::trade::value::gates::GateCandidate>, // Task 7; empty for now
```
At the end of `build_category`, before constructing `CategoryModel`:
```rust
    let mod_rolls = crate::trade::value::magnitude::build_mod_rolls(obs);
    let items = crate::trade::value::itemvec::build_item_vectors(obs, &mod_rolls);
```
and include `mod_rolls, items, weights: Default::default(), undersampled_gates: Vec::new()` in the struct literal. (Add `#[derive(Default)]` stubs / placeholder modules for `estimate::SimWeights` and `gates::GateCandidate` now as empty types so this compiles; they're fleshed out in Tasks 6–7. Declare `pub mod estimate;` and `pub mod gates;` with the minimal type defs.)
  - Minimal stubs to add now: in `estimate.rs`: `#[derive(Debug,Clone,Copy,Default)] pub struct SimWeights { pub jaccard: f64, pub roll: f64 }`; in `gates.rs`: `#[derive(Debug,Clone)] pub struct GateCandidate { pub stat_id: String, pub label: Option<String>, pub count: usize }`.
- [ ] **Step 5: Run, expect PASS.** `cargo test itemvec:: && cargo build`
- [ ] **Step 6: Commit.**

---

## Task 3: Similarity metric (`estimate.rs`)

**Files:**
- Modify: `src/trade/value/estimate.rs`

**Interfaces:**
- Consumes: `ItemVector` (Task 2), `SimWeights`.
- Produces: `fn similarity(query: &[(String, Option<f64>)], item: &ItemVector, w: SimWeights) -> f64` (→ `[0,1]`); `SimWeights::normalized(self) -> SimWeights`.

- [ ] **Step 1: Failing tests** (prove category-adaptivity at the metric level):
```rust
#[test]
fn jaccard_weight_rewards_mod_overlap() {
    let item = ItemVector { mods: vec![("a".into(), None), ("b".into(), None)], price_divine: 1.0 };
    let w = SimWeights { jaccard: 1.0, roll: 0.0 };
    let full = similarity(&[("a".into(), None), ("b".into(), None)], &item, w);
    let half = similarity(&[("a".into(), None), ("c".into(), None)], &item, w);
    assert!((full - 1.0).abs() < 1e-9);
    assert!(full > half && half > 0.0);
}

#[test]
fn roll_weight_rewards_roll_proximity_on_shared_mods() {
    let item = ItemVector { mods: vec![("a".into(), Some(0.9))], price_divine: 1.0 };
    let w = SimWeights { jaccard: 0.0, roll: 1.0 };
    let near = similarity(&[("a".into(), Some(0.85))], &item, w);
    let far = similarity(&[("a".into(), Some(0.1))], &item, w);
    assert!(near > far);
}

#[test]
fn empty_query_or_no_shared_is_zero() {
    let item = ItemVector { mods: vec![("a".into(), Some(0.5))], price_divine: 1.0 };
    let w = SimWeights { jaccard: 0.5, roll: 0.5 }.normalized();
    assert_eq!(similarity(&[], &item, w), 0.0);
}
```
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement** (append to `estimate.rs`):
```rust
use super::itemvec::ItemVector;
use std::collections::{HashMap, HashSet};

impl SimWeights {
    pub fn normalized(self) -> SimWeights {
        let s = self.jaccard + self.roll;
        if s <= 0.0 { SimWeights { jaccard: 1.0, roll: 0.0 } } else { SimWeights { jaccard: self.jaccard / s, roll: self.roll / s } }
    }
}

pub fn similarity(query: &[(String, Option<f64>)], item: &ItemVector, w: SimWeights) -> f64 {
    if query.is_empty() || item.mods.is_empty() { return 0.0; }
    let qset: HashSet<&str> = query.iter().map(|(s, _)| s.as_str()).collect();
    let iset: HashSet<&str> = item.mods.iter().map(|(s, _)| s.as_str()).collect();
    let inter = qset.intersection(&iset).count();
    let union = qset.union(&iset).count();
    let jac = if union == 0 { 0.0 } else { inter as f64 / union as f64 };

    let qroll: HashMap<&str, f64> = query.iter().filter_map(|(s, r)| r.map(|r| (s.as_str(), r))).collect();
    let mut sum = 0.0; let mut n = 0usize;
    for (s, r) in &item.mods {
        if let (Some(r), Some(qr)) = (r, qroll.get(s.as_str())) {
            sum += 1.0 - (qr - r).abs();
            n += 1;
        }
    }
    let roll = if n == 0 { 0.0 } else { sum / n as f64 };
    let w = w.normalized();
    w.jaccard * jac + w.roll * roll
}
```
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit.**

---

## Task 4: k-NN estimate + confidence (`estimate.rs`)

**Files:**
- Modify: `src/trade/value/estimate.rs`, `src/trade/value/mod.rs`

**Interfaces:**
- Consumes: `similarity` (Task 3), `CategoryModel.items`/`weights`, consts `K_NEIGHBORS`, `MIN_NEIGHBORS`.
- Produces: `enum Confidence { High, Medium, Low }`; `struct ValueEstimate { pub value_divine: f64, pub confidence: Confidence, pub neighbors: usize }`; `impl CategoryModel { pub fn estimate(&self, query: &[(String, Option<f64>)]) -> Option<ValueEstimate> }`; `fn weighted_median(sorted_by_price: &[(f64 /*sim*/, f64 /*price*/)]) -> f64`.

- [ ] **Step 1: Failing test.**
```rust
#[test]
fn estimate_returns_weighted_median_of_neighbors() {
    use crate::trade::value::CategoryModel;
    let items = (0..10).map(|i| ItemVector {
        mods: vec![("a".into(), Some(0.5)), ("b".into(), None)],
        price_divine: 100.0 + i as f64, // 100..109
    }).collect();
    let mut cat = CategoryModel::default();
    cat.items = items;
    cat.weights = SimWeights { jaccard: 1.0, roll: 0.0 };
    let est = cat.estimate(&[("a".into(), Some(0.5)), ("b".into(), None)]).expect("estimate");
    assert!(est.value_divine >= 100.0 && est.value_divine <= 109.0);
    assert!(est.neighbors >= MIN_NEIGHBORS);
}

#[test]
fn estimate_none_when_too_few_neighbors() {
    use crate::trade::value::CategoryModel;
    let mut cat = CategoryModel::default();
    cat.items = vec![ItemVector { mods: vec![("a".into(), None)], price_divine: 5.0 }];
    cat.weights = SimWeights { jaccard: 1.0, roll: 0.0 };
    assert!(cat.estimate(&[("a".into(), None)]).is_none(), "1 neighbor < MIN_NEIGHBORS");
}
```
(Requires `CategoryModel: Default` — add `#[derive(Default)]` already present on it; confirm.)
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence { High, Medium, Low }

#[derive(Debug, Clone)]
pub struct ValueEstimate {
    pub value_divine: f64,
    pub confidence: Confidence,
    pub neighbors: usize,
}

/// Median of prices weighted by similarity. `scored` is (sim, price), sim>0.
fn weighted_median(scored: &[(f64, f64)]) -> f64 {
    let mut v: Vec<(f64, f64)> = scored.to_vec();
    v.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let total: f64 = v.iter().map(|(s, _)| *s).sum();
    if total <= 0.0 { return 0.0; }
    let mut acc = 0.0;
    for (s, p) in &v {
        acc += s;
        if acc >= total / 2.0 { return *p; }
    }
    v.last().map(|(_, p)| *p).unwrap_or(0.0)
}

impl crate::trade::value::CategoryModel {
    pub fn estimate(&self, query: &[(String, Option<f64>)]) -> Option<ValueEstimate> {
        if self.items.is_empty() { return None; }
        let mut scored: Vec<(f64, f64)> = self.items.iter()
            .map(|it| (similarity(query, it, self.weights), it.price_divine))
            .filter(|(s, _)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(super::K_NEIGHBORS);
        if scored.len() < super::MIN_NEIGHBORS { return None; }
        let value_divine = weighted_median(&scored);
        let top_sim = scored[0].0;
        // dispersion: neighbor price spread relative to the estimate
        let prices: Vec<f64> = scored.iter().map(|(_, p)| *p).collect();
        let spread = relative_spread(&prices, value_divine);
        let confidence = if scored.len() >= super::K_NEIGHBORS && top_sim >= 0.6 && spread <= 0.5 {
            Confidence::High
        } else if scored.len() >= super::MIN_NEIGHBORS * 2 && spread <= 1.0 {
            Confidence::Medium
        } else {
            Confidence::Low
        };
        Some(ValueEstimate { value_divine, confidence, neighbors: scored.len() })
    }
}

fn relative_spread(prices: &[f64], center: f64) -> f64 {
    if center <= 0.0 || prices.is_empty() { return f64::INFINITY; }
    let mut dev: Vec<f64> = prices.iter().map(|p| (p - center).abs()).collect();
    dev.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    dev[dev.len() / 2] / center // median abs deviation / center
}
```
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit.**

---

## Task 5: Query construction from a ParsedItem (`estimate.rs`)

**Files:**
- Modify: `src/trade/value/estimate.rs`

**Interfaces:**
- Consumes: `crate::trade::stats::StatCatalog` (maps a raw mod line → stat_id; same matching the live query uses), `CategoryModel.mod_rolls`.
- Produces: `impl CategoryModel { pub fn query_from_stats(&self, stat_ids_and_rolls: &[(String, Option<f64>)]) -> Vec<(String, Option<f64>)> }` — normalizes raw rolls to percentiles via `mod_rolls`. (The caller resolves the `ParsedItem`'s mod lines to `(stat_id, raw_roll)` using the existing catalog matching in `src/trade/query.rs`; this fn only does roll normalization so the unit test is catalog-free.)

- [ ] **Step 1: Failing test.**
```rust
#[test]
fn query_normalizes_raw_rolls_via_mod_rolls() {
    use crate::trade::value::{CategoryModel, magnitude::RollStats};
    let mut cat = CategoryModel::default();
    cat.mod_rolls.insert("a".into(), RollStats::from_rolls(&[0.0, 50.0, 100.0]));
    let q = cat.query_from_stats(&[("a".into(), Some(100.0)), ("b".into(), None)]);
    assert_eq!(q[0].0, "a");
    assert!((q[0].1.unwrap() - 1.0).abs() < 1e-9);
    assert_eq!(q[1], ("b".into(), None)); // unknown mod → roll passes as None
}
```
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
```rust
impl crate::trade::value::CategoryModel {
    pub fn query_from_stats(&self, stats: &[(String, Option<f64>)]) -> Vec<(String, Option<f64>)> {
        stats.iter().map(|(id, roll)| {
            let norm = roll.and_then(|r| self.mod_rolls.get(id).map(|rs| rs.normalize(r)));
            (id.clone(), norm)
        }).collect()
    }
}
```
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit.**

---

## Task 6: Leave-one-out backtest + per-category weight tuning (`backtest.rs`)

**Files:**
- Create: `src/trade/value/backtest.rs`
- Modify: `src/trade/value/mod.rs` (`pub mod backtest;`; call `tune_weights` in `build_category`; store result in `CategoryModel.weights`)

**Interfaces:**
- Consumes: `similarity`, `weighted_median` logic (reuse `CategoryModel::estimate` by temporarily setting weights), `ItemVector`.
- Produces: `fn loo_median_error(items: &[ItemVector], w: SimWeights) -> Option<f64>` (median |pred-actual|/actual over items with ≥MIN_NEIGHBORS neighbors; None if too few); `fn tune_weights(items: &[ItemVector]) -> (SimWeights, Option<f64>)` (grid search → best weights + its error).

- [ ] **Step 1: Failing tests** (the central category-adaptivity proof):
```rust
#[test]
fn tune_picks_roll_weight_for_magnitude_dominant_corpus() {
    // price determined purely by roll of mod "a"; mod-set identical across items.
    let items: Vec<ItemVector> = (0..40).map(|i| {
        let r = i as f64 / 39.0;
        ItemVector { mods: vec![("a".into(), Some(r))], price_divine: 10.0 + 100.0 * r }
    }).collect();
    let (w, err) = tune_weights(&items);
    assert!(w.roll > w.jaccard, "magnitude-dominant → roll weight wins (w={:?})", w);
    assert!(err.unwrap() < 0.3, "calibrated");
}

#[test]
fn tune_picks_jaccard_for_combination_dominant_corpus() {
    // price determined by how many of {a,b,c} are present; rolls absent.
    let mk = |present: &[&str], price: f64| ItemVector {
        mods: present.iter().map(|s| (s.to_string(), None)).collect(), price_divine: price,
    };
    let mut items = Vec::new();
    for _ in 0..15 { items.push(mk(&["a"], 10.0)); }
    for _ in 0..15 { items.push(mk(&["a","b"], 50.0)); }
    for _ in 0..15 { items.push(mk(&["a","b","c"], 200.0)); }
    let (w, _) = tune_weights(&items);
    assert!(w.jaccard >= w.roll, "combination-dominant → jaccard not beaten (w={:?})", w);
}
```
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
```rust
//! Leave-one-out calibration: report per-category prediction error and pick the
//! similarity weights that minimize it (so each category self-selects whether
//! combination or roll-magnitude drives value).
use super::estimate::{similarity, SimWeights};
use super::itemvec::ItemVector;
use super::{K_NEIGHBORS, MIN_NEIGHBORS};

const WEIGHT_GRID: [(f64, f64); 5] =
    [(1.0, 0.0), (0.75, 0.25), (0.5, 0.5), (0.25, 0.75), (0.0, 1.0)];

fn predict_one(items: &[ItemVector], skip: usize, w: SimWeights) -> Option<f64> {
    let q: Vec<(String, Option<f64>)> = items[skip].mods.clone();
    let mut scored: Vec<(f64, f64)> = items.iter().enumerate()
        .filter(|(i, _)| *i != skip)
        .map(|(_, it)| (similarity(&q, it, w), it.price_divine))
        .filter(|(s, _)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(K_NEIGHBORS);
    if scored.len() < MIN_NEIGHBORS { return None; }
    Some(super::estimate::weighted_median_pub(&scored))
}

pub fn loo_median_error(items: &[ItemVector], w: SimWeights) -> Option<f64> {
    let mut errs: Vec<f64> = Vec::new();
    for i in 0..items.len() {
        let actual = items[i].price_divine;
        if actual <= 0.0 { continue; }
        if let Some(pred) = predict_one(items, i, w) {
            errs.push((pred - actual).abs() / actual);
        }
    }
    if errs.len() < MIN_NEIGHBORS { return None; }
    errs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(errs[errs.len() / 2])
}

pub fn tune_weights(items: &[ItemVector]) -> (SimWeights, Option<f64>) {
    let mut best = (SimWeights { jaccard: 1.0, roll: 0.0 }, None::<f64>);
    for (j, r) in WEIGHT_GRID {
        let w = SimWeights { jaccard: j, roll: r };
        if let Some(e) = loo_median_error(items, w) {
            if best.1.map(|b| e < b).unwrap_or(true) {
                best = (w, Some(e));
            }
        }
    }
    best
}
```
(Add `pub fn weighted_median_pub(scored: &[(f64,f64)]) -> f64 { weighted_median(scored) }` to `estimate.rs`, or make `weighted_median` `pub(crate)` — choose the latter to avoid the wrapper: change `fn weighted_median` → `pub(crate) fn weighted_median`.)
- [ ] **Step 4: Wire into build.** In `build_category` (mod.rs), after `items` is built:
```rust
    let (weights, loo_error) = crate::trade::value::backtest::tune_weights(&items);
```
Add `pub loo_error: Option<f64>` to `CategoryModel`; set `weights`/`loo_error` in the literal (replacing the `weights: Default::default()` from Task 2).
- [ ] **Step 5: Run, expect PASS.** `cargo test backtest::`
- [ ] **Step 6: Commit.**

---

## Task 7: Undersampled-gate detection (`gates.rs`)

**Files:**
- Modify: `src/trade/value/gates.rs`, `src/trade/value/mod.rs` (call in `build_category`)

**Interfaces:**
- Consumes: `StatValue` (existing: `stat_id`, `count`, `lift`, `top_decile_freq`, `label`), `crate::trade::query::is_cornerstone` (make it `pub(crate)`), consts `MAGNITUDE_MIN_SAMPLE`, `DRIVER_LIFT`.
- Produces: `fn detect_gates(stats: &[StatValue]) -> Vec<GateCandidate>`.

- [ ] **Step 1: Make `is_cornerstone` reachable.** In `src/trade/query.rs` change `fn is_cornerstone` → `pub(crate) fn is_cornerstone`.
- [ ] **Step 2: Failing test** (`gates.rs`):
```rust
#[test]
fn flags_cornerstone_and_high_signal_low_count() {
    use crate::trade::value::StatValue;
    let sv = |label: &str, count: usize, lift: f64| StatValue {
        stat_id: format!("id.{label}"), label: Some(label.into()), count,
        median_with: 0.0, lift, conditional_lift: None, top_decile_freq: 0.0,
    };
    let stats = vec![
        sv("+1 to Level of all Projectile Skills", 12, 1.0), // cornerstone, low count → flagged
        sv("# to maximum Life", 400, 1.1),                   // common, not a gate → not flagged
        sv("#% increased Rare Mechanic", 10, 2.0),           // high lift, low count → flagged
        sv("+1 to Level of all Spell Skills", 200, 1.4),     // cornerstone but well-sampled → not flagged
    ];
    let gates = detect_gates(&stats);
    let names: Vec<&str> = gates.iter().filter_map(|g| g.label.as_deref()).collect();
    assert!(names.iter().any(|n| n.contains("Projectile")));
    assert!(names.iter().any(|n| n.contains("Rare Mechanic")));
    assert!(!names.iter().any(|n| n.contains("maximum Life")));
    assert!(!names.iter().any(|n| n.contains("all Spell")));
}
```
- [ ] **Step 3: Implement.**
```rust
//! Detect "undersampled gate" mods: build-defining mods the model can't yet learn
//! a magnitude curve for (too few samples). Surfaced for operator-triggered
//! targeted sampling.
use crate::trade::value::StatValue;
use super::{DRIVER_LIFT, MAGNITUDE_MIN_SAMPLE};

#[derive(Debug, Clone)]
pub struct GateCandidate {
    pub stat_id: String,
    pub label: Option<String>,
    pub count: usize,
}

pub fn detect_gates(stats: &[StatValue]) -> Vec<GateCandidate> {
    let mut out: Vec<GateCandidate> = stats.iter()
        .filter(|s| s.count < MAGNITUDE_MIN_SAMPLE)
        .filter(|s| {
            let cornerstone = s.label.as_deref().map(crate::trade::query::is_cornerstone).unwrap_or(false);
            let high_signal = s.lift >= DRIVER_LIFT;
            cornerstone || high_signal
        })
        .map(|s| GateCandidate { stat_id: s.stat_id.clone(), label: s.label.clone(), count: s.count })
        .collect();
    out.sort_by(|a, b| a.count.cmp(&b.count));
    out
}
```
(Note: `StatValue.label` is currently always `None` from `build_category`. Add a step in `build_category` to fill labels from the `StatCatalog` if available — see Task 9 note — OR pass labels into `detect_gates`. To keep Task 7 self-contained, `detect_gates` reads `s.label`; Task 9 ensures labels are populated. For now, also populate `label` in `build_category` via the catalog passed to `build` — thread the catalog through `build`/`build_category`. If that threading is out of scope here, gate-by-`is_cornerstone` still works once labels exist; add the label-population sub-step to Task 9 and have Task 7 tests use explicit labels as above.)
- [ ] **Step 4: Wire** `undersampled_gates: detect_gates(&stats)` into the `build_category` literal.
- [ ] **Step 5: Run, expect PASS; commit.**

---

## Task 8: Pricer learned-estimate helper (`mod.rs`)

**Files:**
- Modify: `src/trade/mod.rs` (`TradePricer`)

**Interfaces:**
- Consumes: `ValueModel` (held at `self.value: Arc<RwLock<ValueModel>>`), `CategoryModel::{query_from_stats, estimate}`, `TRUST_MIN_SAMPLE`, `TRUST_MAX_ERROR`.
- Produces: `impl TradePricer { pub fn learned_estimate(&self, item: &ParsedItem, league: &str) -> Option<ValueEstimate> }` — resolves the item's mods to `(stat_id, roll)` (reuse the catalog matching already used by `build_baseline`/`build_query` in query.rs — extract a `pub(crate) fn resolve_stat_ids(item, catalog) -> Vec<(String, Option<f64>)>` if not already available), looks up `(league, canonical_category)`, returns an estimate **only if** the category clears the trust bar.

- [ ] **Step 1: Failing test** with a fake-populated `ValueModel` (build a `CategoryModel` with ≥TRUST_MIN_SAMPLE synthetic items at a known price and assert `learned_estimate` returns ~that price; and returns `None` for an untrusted/thin category). Place in `mod.rs` tests using `ValueModel`/`CategoryModel` constructors.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement** `learned_estimate`: canonical category via `crate::trade::value::canonical_category(item.item_class)`, `model.category(league, &canon)`, trust check `cat.sample_size >= TRUST_MIN_SAMPLE && cat.loo_error.map(|e| e <= TRUST_MAX_ERROR).unwrap_or(false)`, then `cat.estimate(&cat.query_from_stats(&resolve_stat_ids(item, &self.catalog)))`.
- [ ] **Step 4: Run, expect PASS; commit.**

---

## Task 9: Surface learned estimate on /paste + /insights

**Files:**
- Modify: `src/discord/embeds.rs` (`estimate_embed`), `src/discord/paste.rs` (`run_pricing`), `src/discord/insights.rs`

- [ ] **Step 1 (paste):** In `run_pricing`, after `est` is computed, call `let learned = pricer.learned_estimate(parsed, &league.name);` and pass it to `estimate_embed`. In `estimate_embed`, when `learned` is `Some`, add a field: `Learned (corpus): <value> div · confidence <H/M/L> · <n> comps` and, if `|learned-live|/live > DIVERGENCE_FLAG`, append `⚠ diverges from live`. When `None`, omit (regression: byte-identical to today). Add a unit test on `estimate_embed` field presence/absence.
- [ ] **Step 2 (insights):** Extend `insights` to render, per category: existing drivers; **magnitude curves** for the top drivers (roll bucket → median price, from `roll_price_curve`); **archetype labels** (Task 10 optional — if not built, show top co-occurring mods, already available); **undersampled-gate candidates** (`cat.undersampled_gates`, with `n=<count>`). Populate `StatValue.label` in `build_category` from the catalog here (thread `&StatCatalog` into `build`/`rebuild_into`/`build_category`; the refresher already has the catalog). Add a test that insights output includes a gate candidate when present.
- [ ] **Step 3:** `cargo test`, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`; commit.

---

## Task 10: Targeted harvest (operator-triggered, mod-filtered sweep)

**Files:**
- Modify: `src/trade/mod.rs` (`harvest`), the `/harvest` command registration (`src/discord/*` — locate via `rg -n "harvest" src/discord`)

**Interfaces:**
- Consumes: existing `harvest` adaptive sweep; `TradeQuery.stats` (`StatFilter`).
- Produces: `impl TradePricer { pub async fn harvest_mod(&self, category_id, category_text, league, stat_id, session) -> Result<usize> }` — same adaptive band sweep as `harvest`, but every band query carries `stats: vec![StatFilter { id: stat_id, value: {}, .. }]` so every fetched item has the gate mod. Reuse the band/stride/sub-band logic (factor the per-(lo,hi) sweep body out of `harvest` into a shared `harvest_sweep(query_template, …)` so both call it — DRY).

- [ ] **Step 1:** Refactor `harvest`'s band-loop into `harvest_sweep` taking a base `TradeQuery` (category + optional stats); `harvest` calls it with empty stats, `harvest_mod` with the pinned stat filter. Keep existing harvest tests green (they exercise `harvest`).
- [ ] **Step 2:** Add a test: `harvest_mod` issues searches whose `TradeQuery.stats` contains the pinned stat id (assert via a fake `TradeApi` recording queries).
- [ ] **Step 3:** Add the command surface — a `mod` option on `/harvest` (autocomplete from the active category's `undersampled_gates`) that routes to `harvest_mod`. Follow the existing `/harvest` handler pattern.
- [ ] **Step 4:** `cargo test`, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`; commit.

---

## Task 11: LOO backtest report (operator visibility)

**Files:**
- Modify: `src/discord/insights.rs` (or a small maintenance command)

- [ ] **Step 1:** Surface per-category `loo_error` + `sample_size` + chosen `weights` (e.g. a line in `/insights` with no category arg, or a `/calibration` command): `Staff: n=1141, LOO err 31%, weights j/r 0.5/0.5 ✓trusted`. This is the success-criteria readout. Add a test on the formatting helper.
- [ ] **Step 2:** `cargo test`, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`; commit.

---

## Self-Review

**Spec coverage:**
- Predictive k-NN estimate → Tasks 3–5, 8. ✓
- Roll-magnitude first-class → Tasks 1–2 (normalize + item vectors), used in 3–6. ✓
- Backtest validates + tunes weights → Task 6; trust bar → Task 8; report → Task 11. ✓
- Descriptive decomposition + magnitude curves + archetypes → Task 9 (decompose helper noted; archetype overlay degrades to co-occurrence per spec). ⚠ Decomposition `CategoryModel::decompose` is referenced in the spec but only surfaced via insights — fold an explicit `decompose` step into Task 9 Step 2 (rank query mods by `lift × roll_norm`).
- Targeted sampling (detect + operator harvest) → Tasks 7, 10. ✓
- Surfacing /paste + /insights → Task 9; new command → Task 10. ✓
- Secondary-to-live-ablation + regression-safe omission → Task 8 trust bar + Task 9 Step 1 (omit when None). ✓
- Module split for focus → Task 0. ✓

**Placeholder scan:** Task 8 leans on a `resolve_stat_ids` helper "if not already available" — before implementing Task 8, grep `src/trade/query.rs` for the existing ParsedItem→stat-id matching (used by `build_baseline`) and either reuse or extract it; do not invent a parallel matcher. Task 9/Task 7 label population is threaded through `build` — confirm the refresher has a `StatCatalog` to pass (it builds the live pricer, which holds `catalog`).

**Type consistency:** `SimWeights{jaccard,roll}`, `ItemVector{mods:Vec<(String,Option<f64>)>,price_divine}`, `ValueEstimate{value_divine,confidence,neighbors}`, `RollStats{quantiles}`, `GateCandidate{stat_id,label,count}`, `CategoryModel` new fields `{mod_rolls,items,weights,loo_error,undersampled_gates}` — used consistently across tasks.

## Risks

- **`resolve_stat_ids` extraction** (Task 8) is the one integration unknown — scope it first.
- **Memory**: retained `items` per category are unbounded; if a category's fresh corpus exceeds a few thousand, cap to a recent+representative sample in `build_item_vectors` (add only if a category grows large — YAGNI for now, noted).
- **Backtest cost**: `tune_weights` is O(grid × n²) per category at rebuild. Fine at n~1–2k × 5 weights × handful of categories on the 60-min refresh; if it bites, sample the LOO query set.
