# Phase 0 — Honest Evaluation Harness — Design

**Goal:** Replace the mis-specified value-model trust metric with an honest one, so
`/insights` stops implying every category "just needs more data" when the truth is "the
model has no skill over a no-feature baseline." Measurement + reporting only — no change
to how items are priced (`/paste` already shows live trade2 ablation; the learned line is
currently gated off everywhere and stays off).

**Architecture (one sentence):** In the value-model rebuild, score each category's k-NN by
**leave-one-MOD-SET-out** (self-exclusion) and report **skill over a no-feature baseline**
(the category median), replacing the leaky, individual-listing `loo_error ≤ 0.50` trust
gate with `skill > 0`, and surface those honest numbers in `/insights`.

**Tech stack:** Rust. Touches `src/trade/value/backtest.rs`, `src/trade/value/mod.rs`
(`CategoryModel` fields + trust constants + `build_category`), `src/trade/mod.rs`
(`learned_estimate` trust check), `src/discord/insights.rs` (`calibration_line`).

---

## Why (from the 10-expert debate consensus)

See memory `pricing-strategy-debate-consensus`. The current trust gate
(`sample_size ≥ 80 && loo_error ≤ 0.50`) is wrong on two counts, both verified on the live
corpus:

1. **Leakage.** `loo_median_error`/`predict_one` leave out only the single probe item, so
   the held-out item's **exact mod-set siblings remain in the neighbour pool**. With heavy
   round-number clustering, a sibling at the same round price makes the prediction look
   near-perfect. Self-included exact-set rank-vs-actual is +0.76/+0.87/+0.80/+0.68/+0.73
   (Staff/Ring/Amulet/Body/Bow); under honest leave-the-group-out it collapses to Staff
   +0.56, Ring +0.11, Amulet ~0, Body −0.09, Bow −0.08.
2. **Wrong target / no reference.** Median relative error of predicting an *individual
   listing* is reported with no baseline, so a 67–80% number reads as "almost there" when
   in fact the model **barely beats — or loses to — guessing the category median**
   (verified skill: Staff +17%, Ring −34%, Amulet −19%, Body −102%, Bow −65%).

**Temporal/forward split is deferred** (the consensus put the rolling holdout in Phase 2).
It is not valid on today's corpus: capture time (`timestamp_unix`) spans only
2026-06-22→24 (≈1 day of real history), and splitting on posting time (`indexed`) is
confounded by survivorship — an older-posted listing still live hasn't sold, so it is
systematically overpriced. Capture time is already recorded, so a forward split becomes
valid automatically as the bot accumulates snapshots; Phase 2 builds it then.

## Components

### 1. Self-exclusion in the backtest (`backtest.rs`)

The neighbour search for a held-out probe must exclude **every item sharing the probe's
exact mod-set** (the set of `stat_id`s), not just the probe's own index.

- Precompute each item's mod-set key once (e.g. a sorted-stat-id signature) for the
  category's `ItemVector`s.
- `predict_one(items, probe, w)` filters neighbours to those whose mod-set key ≠ the
  probe's key (in addition to skipping the probe itself), then proceeds as today
  (similarity, top-K, weighted median). Returns `None` if fewer than `MIN_NEIGHBORS`
  remain.

This is the single load-bearing correctness fix.

### 2. Skill-over-baseline metric (`backtest.rs` + `CategoryModel`)

Over the same evenly-spaced probe set already used by `loo_median_error`, with the same
self-exclusion applied to BOTH predictors:

- `model_err` = median over probes of `|knn_pred − actual| / actual`.
- `baseline_err` = median over probes of `|cat_median_excl − actual| / actual`, where
  `cat_median_excl` is the median `price_divine` of all items **except the probe's
  mod-set group** (the no-feature predictor, scored on the identical held-out set).
- `skill = (baseline_err − model_err) / baseline_err` (fraction of baseline error the
  model removes; `> 0` ⇒ beats no-features, `≤ 0` ⇒ no better than guessing the median).

`CategoryModel` replaces `loo_error: Option<f64>` with `model_err`, `baseline_err`, and
`skill` (all `Option<f64>`; `None` when too few probes resolve). `tune_weights` selects the
per-category `(w_jaccard, w_roll)` by **minimizing self-excluded `model_err`** (same grid),
so weight tuning is also leakage-free.

### 3. Trust gate (`value/mod.rs` + `learned_estimate` in `trade/mod.rs`)

- Remove `TRUST_MAX_ERROR`. Keep `TRUST_MIN_SAMPLE = 80`.
- A category is "trusted" (the learned layer has demonstrable signal) iff
  `sample_size ≥ TRUST_MIN_SAMPLE && skill > 0`.
- `learned_estimate`'s trust check switches from `loo_error ≤ 0.50` to `skill > 0`. (Net
  effect today: Staff becomes the only category that could surface a learned line — but
  `/paste` surfacing is rebuilt in Phase 1, so no `/paste` behaviour change ships here.)

### 4. `/insights` calibration line (`insights.rs`)

`calibration_line` shows the honest read per category, e.g.:

`• Staff: n=2087 · model 75% · base 88% · skill +15% ✓ (beats baseline)`
`• Amulet: n=1206 · model 75% · base 76% · skill −1% ✗ (no skill over baseline)`

`n/a` when metrics are `None`. The binary "trusted/untrusted" wording is replaced by the
skill verdict so a reader never mistakes "no skill" for "needs more data."

## Data flow

Unchanged: corpus JSONL → `rebuild_into` (existing fresh+priceable filter) → `build` →
`build_category` (now computes self-excluded `model_err`/`baseline_err`/`skill` via
`tune_weights`) → `ValueModel`. `/insights` reads the new fields; `learned_estimate` reads
the new gate.

## Testing

- **Self-exclusion:** synthetic corpus where a probe's only same-mod-set sibling sits at
  the probe's exact price and all other items are far off → assert the prediction does NOT
  return that sibling's price (siblings excluded) and the error reflects the dissimilar
  remainder.
- **Skill computation:** (a) model strictly better than category median → `skill > 0`;
  (b) k-NN == predicting the median → `skill ≈ 0`; (c) model worse → `skill < 0`.
- **`baseline_err`** equals the median rel error of the self-excluded category median on a
  known fixture.
- **Trust gate:** `sample ≥ 80 && skill > 0` trusted; `skill ≤ 0` untrusted regardless of
  sample size.
- **`calibration_line`** format: shows model/base/skill and the ✓/✗ verdict; `n/a` on
  `None`.
- **Regression:** existing `backtest`/`insights` tests updated to the new fields; no other
  behaviour changes.

## Success criteria

- `/insights` reports per-category `model_err`, `baseline_err`, and `skill` computed with
  leave-the-mod-set-group-out self-exclusion; the leaky individual-listing `LOO ≤ 0.50`
  gate is gone.
- On the live corpus the readout matches the verified skills (Staff ≈ +15–17%, accessories
  ≤ 0). No metric is scored against the operator's price prior.

## Out of scope (later phases)

- The range/abstention/tier engine, conformal calibration, the temporal/forward split
  (deferred until capture history accumulates), tail routing to live trade2, and any change
  to `/paste` output.
