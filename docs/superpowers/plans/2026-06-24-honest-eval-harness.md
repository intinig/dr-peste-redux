# Phase 0 — Honest Evaluation Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the leaky, individual-listing `loo_error ≤ 0.50` trust gate with a leave-the-mod-set-group-out (self-excluded) **skill-over-baseline** metric, and surface honest per-category numbers in `/insights`. Measurement/reporting only — `/paste` is unchanged.

**Architecture:** The value-model rebuild already calls `backtest::tune_weights` per category. Replace it with `tune_and_calibrate`, which (a) selects similarity weights by **self-excluded** model error and (b) returns a `Calibration { model_err, baseline_err, skill }`. `CategoryModel` stores the `Calibration`; a shared `CategoryModel::is_trusted()` (`sample ≥ 80 && skill > 0`) gates both `learned_estimate` and the `/insights` verdict.

**Tech Stack:** Rust. No new deps.

## Global Constraints

- Binary crate, no lib target — `cargo test` / `cargo test <name>`, **never** `cargo test --lib`.
- CI runs `cargo clippy --all-targets -- -D warnings` (toolchain 1.96) — keep clean; `cargo fmt` before each commit.
- Self-exclusion = exclude **every item sharing the probe's exact mod-set** (set of `stat_id`s), not just the probe index. This applies to BOTH the k-NN predictor and the baseline predictor, scored on the same probe set.
- `skill = (baseline_err − model_err) / baseline_err`; `skill > 0` ⇔ model beats the no-feature category-median baseline.
- No metric is scored against the operator's price prior. Temporal/forward split is **out of scope** (deferred to Phase 2).
- Stage files by name; end commit messages with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

---

### Task 1: Self-excluded skill metric in `backtest.rs`

**Files:**
- Modify: `src/trade/value/backtest.rs`

**Interfaces:**
- Consumes: `ItemVector { mods: Vec<(String, Option<f64>)>, price_divine: f64 }`, `similarity`, `weighted_median`, `SimWeights`, `K_NEIGHBORS`, `MIN_NEIGHBORS` (all already imported).
- Produces: `pub struct Calibration { pub model_err: Option<f64>, pub baseline_err: Option<f64>, pub skill: Option<f64> }` (derive `Debug, Default, Clone, Copy, PartialEq`); `pub fn tune_and_calibrate(items: &[ItemVector]) -> (SimWeights, Calibration)`.

- [ ] **Step 1: Write failing tests**

Add to the `tests` module in `src/trade/value/backtest.rs`:

```rust
#[test]
fn self_exclusion_drops_same_mod_set_siblings() {
    // Probe at index 0 (mods {a}, price 100). One sibling (also {a}, price 100) would
    // make a self-included predictor return 100 (0 error). All OTHER items share NO mods
    // with the probe (mods {b}, similarity 0) → after self-excluding the {a} group there
    // are no positive-similarity neighbours, so predict_one returns None and the probe
    // contributes nothing. Proves siblings are excluded, not used.
    let mut items = vec![
        ItemVector { mods: vec![("a".into(), None)], price_divine: 100.0 },
        ItemVector { mods: vec![("a".into(), None)], price_divine: 100.0 },
    ];
    for _ in 0..20 {
        items.push(ItemVector { mods: vec![("b".into(), None)], price_divine: 1.0 });
    }
    let keys = mod_keys(&items);
    assert!(predict_one(&items, &keys, 0, SimWeights { jaccard: 1.0, roll: 0.0 }).is_none(),
        "same-mod-set siblings must be excluded, leaving no similar neighbours");
}

#[test]
fn skill_positive_when_model_beats_median() {
    // Two well-separated mod-set groups at very different prices, each with enough members
    // that leave-the-group-out still leaves the OTHER group as neighbours? No — the k-NN
    // would have no same-set neighbour. Instead: price is a smooth function of a shared
    // mod's roll, so roll-proximity (within the kept neighbours) predicts far better than
    // the global median. The grid will pick roll weight and skill must be > 0.
    let items: Vec<ItemVector> = (0..60).map(|i| {
        let r = i as f64 / 59.0;
        // distinct mod-set per item (so self-exclusion removes only itself), shared mod "a"
        // carries the price signal via roll; a unique tag mod makes each mod-set unique.
        ItemVector { mods: vec![("a".into(), Some(r)), (format!("tag{i}"), None)],
                     price_divine: 10.0 + 100.0 * r }
    }).collect();
    let (_w, cal) = tune_and_calibrate(&items);
    assert!(cal.skill.unwrap() > 0.0, "model tracks roll → beats median baseline; skill={:?}", cal.skill);
}

#[test]
fn skill_non_positive_when_no_signal() {
    // Price is independent of mods (random-ish but deterministic), each item a unique
    // mod-set. The k-NN cannot beat predicting the median → skill <= 0 (or None).
    let items: Vec<ItemVector> = (0..60).map(|i| {
        ItemVector { mods: vec![(format!("m{i}"), None)],
                     price_divine: 1.0 + (i % 7) as f64 }
    }).collect();
    let (_w, cal) = tune_and_calibrate(&items);
    assert!(cal.skill.map(|s| s <= 0.0).unwrap_or(true), "no signal → skill<=0/None; got {:?}", cal.skill);
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test self_exclusion_drops_same_mod_set_siblings skill_positive_when_model_beats_median skill_non_positive_when_no_signal`
Expected: FAIL — `mod_keys`, `tune_and_calibrate`, and the new `predict_one` signature don't exist yet.

- [ ] **Step 3: Implement**

Replace the bodies of `predict_one`, `loo_median_error`, and `tune_weights` (lines 31–94) with:

```rust
/// A stable signature of an item's mod-SET (the set of stat_ids, order-independent),
/// used to leave the probe's whole exact-mod-set group out of its own evaluation.
fn mod_keys(items: &[ItemVector]) -> Vec<String> {
    items
        .iter()
        .map(|it| {
            let mut ids: Vec<&str> = it.mods.iter().map(|(s, _)| s.as_str()).collect();
            ids.sort_unstable();
            ids.join("\u{1}")
        })
        .collect()
}

fn median_sorted(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(v[v.len() / 2])
}

/// k-NN prediction for the probe, EXCLUDING every item sharing the probe's exact mod-set
/// (self-exclusion: `keys[i] != keys[skip]`), not just the probe itself.
fn predict_one(items: &[ItemVector], keys: &[String], skip: usize, w: SimWeights) -> Option<f64> {
    let q: Vec<(String, Option<f64>)> = items[skip].mods.clone();
    let mut scored: Vec<(f64, f64)> = items
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != skip && keys[*i] != keys[skip])
        .map(|(_, it)| (similarity(&q, it, w), it.price_divine))
        .filter(|(s, _)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(K_NEIGHBORS);
    if scored.len() < MIN_NEIGHBORS {
        return None;
    }
    Some(weighted_median(&scored))
}

/// Evenly-spaced probe indices across [0, n): `k·n/probes` (bounded by LOO_MAX_PROBES,
/// spread across the whole corpus — see the original loo_median_error note).
fn probe_indices(n: usize) -> Vec<usize> {
    let probes = loo_probe_count(n);
    (0..probes).map(|k| k * n / probes).collect()
}

/// Median self-excluded relative error of the k-NN over the probe set.
fn model_error(items: &[ItemVector], keys: &[String], w: SimWeights) -> Option<f64> {
    let mut errs: Vec<f64> = Vec::new();
    for &i in &probe_indices(items.len()) {
        let actual = items[i].price_divine;
        if actual > 0.0 {
            if let Some(pred) = predict_one(items, keys, i, w) {
                errs.push((pred - actual).abs() / actual);
            }
        }
    }
    if errs.len() < MIN_NEIGHBORS {
        return None;
    }
    median_sorted(&mut errs)
}

/// Median relative error of the NO-FEATURE baseline: predict each probe by the median
/// price of all items EXCEPT the probe's mod-set group (same self-exclusion as the model).
fn baseline_error(items: &[ItemVector], keys: &[String]) -> Option<f64> {
    let mut errs: Vec<f64> = Vec::new();
    for &i in &probe_indices(items.len()) {
        let actual = items[i].price_divine;
        if actual <= 0.0 {
            continue;
        }
        let mut others: Vec<f64> = items
            .iter()
            .enumerate()
            .filter(|(j, _)| keys[*j] != keys[i])
            .map(|(_, it)| it.price_divine)
            .collect();
        if let Some(m) = median_sorted(&mut others) {
            errs.push((m - actual).abs() / actual);
        }
    }
    if errs.is_empty() {
        return None;
    }
    median_sorted(&mut errs)
}

/// Per-category calibration: model error, no-feature baseline error, and skill =
/// fraction of baseline error the model removes (`> 0` ⇒ beats guessing the median).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Calibration {
    pub model_err: Option<f64>,
    pub baseline_err: Option<f64>,
    pub skill: Option<f64>,
}

/// Pick similarity weights by minimizing self-excluded model error, then compute the
/// baseline error and skill over the same probe set.
pub fn tune_and_calibrate(items: &[ItemVector]) -> (SimWeights, Calibration) {
    let keys = mod_keys(items);
    let mut best_w = SimWeights { jaccard: 1.0, roll: 0.0 };
    let mut model_err: Option<f64> = None;
    for (j, r) in WEIGHT_GRID {
        let w = SimWeights { jaccard: j, roll: r };
        if let Some(e) = model_error(items, &keys, w) {
            if model_err.map(|b| e < b).unwrap_or(true) {
                model_err = Some(e);
                best_w = w;
            }
        }
    }
    let baseline_err = baseline_error(items, &keys);
    let skill = match (model_err, baseline_err) {
        (Some(m), Some(b)) if b > 0.0 => Some((b - m) / b),
        _ => None,
    };
    (best_w, Calibration { model_err, baseline_err, skill })
}
```

Update the existing `backtest` tests that reference `loo_median_error`/`tune_weights`/old `predict_one` to the new API (`tune_and_calibrate`, `predict_one(items, &mod_keys(items), i, w)`), keeping their intent. The two `tune_picks_*` weight-selection tests should assert via `tune_and_calibrate(&items).0` (the returned weights); the `loo_probe_count_*` test stays (the fn is unchanged).

- [ ] **Step 4: Run tests**

Run: `cargo test backtest`
Expected: PASS (new + migrated tests).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
git add src/trade/value/backtest.rs
git commit -m "feat(value): self-excluded skill-over-baseline calibration in backtest"
```

---

### Task 2: Wire `Calibration` into `CategoryModel` + trust gate

**Files:**
- Modify: `src/trade/value/mod.rs` (`CategoryModel`, `TRUST_*` constants, `build_category`, add `is_trusted`)
- Modify: `src/trade/mod.rs` (`learned_estimate` trust check)

**Interfaces:**
- Consumes: `backtest::{Calibration, tune_and_calibrate}` (Task 1).
- Produces: `CategoryModel.calibration: backtest::Calibration` (replaces `loo_error`); `CategoryModel::is_trusted(&self) -> bool`; `pub use backtest::Calibration` re-export from the value module if convenient for callers.

- [ ] **Step 1: Write failing test**

Add to the `tests` module in `src/trade/value/mod.rs`:

```rust
#[test]
fn is_trusted_requires_sample_and_positive_skill() {
    let mk = |n: usize, skill: Option<f64>| CategoryModel {
        sample_size: n,
        calibration: backtest::Calibration { model_err: Some(0.7), baseline_err: Some(0.8), skill },
        ..Default::default()
    };
    assert!(mk(100, Some(0.15)).is_trusted(), "enough samples + positive skill");
    assert!(!mk(100, Some(0.0)).is_trusted(), "zero skill is not trusted");
    assert!(!mk(100, Some(-0.2)).is_trusted(), "negative skill is not trusted");
    assert!(!mk(10, Some(0.5)).is_trusted(), "under-sampled is not trusted");
    assert!(!mk(100, None).is_trusted(), "no calibration is not trusted");
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test is_trusted_requires_sample_and_positive_skill`
Expected: FAIL — `calibration` field and `is_trusted` don't exist.

- [ ] **Step 3: Implement**

In `src/trade/value/mod.rs`:
- Replace the field `pub loo_error: Option<f64>,` (line 122) with `pub calibration: backtest::Calibration,`.
- Remove the `TRUST_MAX_ERROR` constant (line ~35-36); keep `TRUST_MIN_SAMPLE = 80`.
- Add to `impl CategoryModel`:

```rust
/// A category's learned layer is trusted iff it has enough samples AND demonstrates
/// positive skill over the no-feature (category-median) baseline. Replaces the old
/// `loo_error <= 0.50` gate (which scored leaky, individual-listing error).
pub fn is_trusted(&self) -> bool {
    self.sample_size >= TRUST_MIN_SAMPLE && self.calibration.skill.is_some_and(|s| s > 0.0)
}
```

- In `build_category` replace `let (weights, loo_error) = backtest::tune_weights(&items);` (line ~339) with `let (weights, calibration) = backtest::tune_and_calibrate(&items);` and set `calibration,` in the `CategoryModel { … }` literal (replacing `loo_error,`).

In `src/trade/mod.rs::learned_estimate`, replace the step-4 trust block (lines 180–188) with:

```rust
        // 4. Trust bar: enough samples AND positive skill over the no-feature baseline.
        if !cat.is_trusted() {
            return None;
        }
```

Update the doc comment above (lines 161–162) to: "the category is not trusted (`sample_size < TRUST_MIN_SAMPLE`, or no positive skill over the category-median baseline)."

- [ ] **Step 4: Run tests**

Run: `cargo test value::` then `cargo test --quiet`
Expected: the new test passes; fix any other `loo_error` references the compiler flags (they are now `calibration`).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
git add src/trade/value/mod.rs src/trade/mod.rs
git commit -m "feat(value): CategoryModel.calibration + skill>0 trust gate"
```

---

### Task 3: Honest `/insights` calibration line

**Files:**
- Modify: `src/discord/insights.rs` (`calibration_line`, imports, tests)

**Interfaces:**
- Consumes: `CategoryModel { calibration, weights, sample_size, category }`, `CategoryModel::is_trusted()`, `TRUST_MIN_SAMPLE` (Task 2). `TRUST_MAX_ERROR` no longer exists — remove from the `use`.

- [ ] **Step 1: Write failing tests**

Replace the three existing `calibration_line_*` tests (they assert the old `LOO err / trusted` wording) with:

```rust
fn cat(name: &str, n: usize, model: Option<f64>, base: Option<f64>, skill: Option<f64>) -> CategoryModel {
    CategoryModel {
        category: name.into(),
        sample_size: n,
        calibration: crate::trade::value::backtest::Calibration { model_err: model, baseline_err: base, skill },
        ..Default::default()
    }
}

#[test]
fn calibration_line_shows_skill_and_beats_verdict() {
    let line = calibration_line(&cat("Staff", 2087, Some(0.75), Some(0.88), Some(0.15)));
    assert!(line.contains("n=2087"), "{line}");
    assert!(line.contains("skill"), "{line}");
    assert!(line.contains("15%"), "{line}");
    assert!(line.contains("✓"), "positive skill + samples → trusted mark: {line}");
}

#[test]
fn calibration_line_marks_no_skill() {
    let line = calibration_line(&cat("Amulet", 1206, Some(0.75), Some(0.76), Some(-0.01)));
    assert!(line.contains("✗"), "negative skill → not trusted: {line}");
}

#[test]
fn calibration_line_handles_missing_metrics() {
    let line = calibration_line(&cat("Wand", 12, None, None, None));
    assert!(line.contains("n/a"), "{line}");
    assert!(line.contains("✗"), "{line}");
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test calibration_line`
Expected: FAIL (compile error on removed `TRUST_MAX_ERROR` / old field, then assertion changes).

- [ ] **Step 3: Implement**

Remove `TRUST_MAX_ERROR` from the `use` at line 7. Replace `calibration_line` (lines 16–32):

```rust
/// One calibration line per category, e.g.
/// `Staff: n=2087 · model 75% · base 88% · skill +15% ✓ (beats baseline)`
/// or `Amulet: n=1206 · model 75% · base 76% · skill −1% ✗ (no skill over baseline)`.
/// Metrics show `n/a` when absent.
pub fn calibration_line(cat: &CategoryModel) -> String {
    let pct = |x: Option<f64>| match x {
        Some(v) => format!("{:.0}%", v * 100.0),
        None => "n/a".to_string(),
    };
    let skill = match cat.calibration.skill {
        Some(s) => format!("{:+.0}%", s * 100.0),
        None => "n/a".to_string(),
    };
    let (mark, verdict) = if cat.is_trusted() {
        ("✓", "beats baseline")
    } else {
        ("✗", "no skill over baseline")
    };
    format!(
        "{}: n={} · model {} · base {} · skill {} {} ({})",
        cat.category,
        cat.sample_size,
        pct(cat.calibration.model_err),
        pct(cat.calibration.baseline_err),
        skill,
        mark,
        verdict,
    )
}
```

(If `CategoryModel::is_trusted` is not re-exported, the call resolves through the existing `CategoryModel` import.)

- [ ] **Step 4: Run tests + full suite + clippy**

```bash
cargo test calibration_line
cargo test --quiet
cargo clippy --all-targets -- -D warnings
```
Expected: all pass; clippy clean.

- [ ] **Step 5: fmt + commit**

```bash
cargo fmt
git add src/discord/insights.rs
git commit -m "feat(insights): show model/baseline/skill instead of leaky LOO + binary trust"
```

---

## Notes for implementer / reviewer

- Do not add the temporal/forward split — it is deferred (capture history too thin; posting-time split is survivorship-confounded). This task is the leakage fix + honest skill readout only.
- `/paste` must not change behaviour: the learned line is gated by `is_trusted()`, which today is true only for Staff; but `/paste` surfacing is rebuilt in Phase 1, so no `/paste` output change is expected from this PR (verify the learned-line code path still compiles and is unchanged except for the gate predicate).
- After deploy, `/insights` should show Staff with positive skill (✓) and the accessories with ≤0 skill (✗) — the honest readout the whole phase is about.
