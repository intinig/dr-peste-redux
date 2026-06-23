# ValueModel Predictive Redesign — Design

**Goal:** Turn the corpus-mined `ValueModel` from a presence-based descriptive
layer into a **per-`(league, category)` predictive value model** that estimates an
item's market value from its mods *and rolls* via k-nearest-neighbours over the
observation corpus, with a parallel descriptive decomposition ("what drives the
price"), operator-triggered targeted sampling for rare gate mods, and a
leave-one-out backtest that both validates and tunes it.

**Architecture (one sentence):** A k-NN estimator that retains per-category corpus
item-vectors in memory and scores similarity over `{mod-set overlap, roll
proximity}` with per-category backtest-tuned weights, kept strictly **secondary**
to the live trade2 ablation pricer, alongside a descriptive lift/magnitude layer
and an auto-detected, operator-run targeted-harvest path.

**Tech stack:** Rust (in-process, no Python/ML libs in production); extends
`src/trade/value.rs`, `src/trade/mod.rs` (harvest), `src/discord/{paste,insights}.rs`;
reads the existing append-only JSONL corpus (`src/observe.rs`).

---

## Why (grounding in the cross-category EDA)

EDA on representative, age-filtered harvests across five classes
(see memory `value-model-cross-category-findings`) established:

- **Roll/tier magnitude is the dominant value signal for accessories/armour**
  (amulets/rings/body-armour) and a real signal everywhere. The current
  `ValueModel` is **presence-only** (`lift`, `top_decile_freq`, co-occurrence) and
  ignores the `roll`/`tier` already stored on every observation — so it learns
  ~nothing useful (lifts ≈ 1.0) for half the classes.
- **No staff pattern generalises**: combination-dominance (staff-only), the
  Desecrated premium (staff-only), and `+spell-levels` are all category-specific.
  The model must learn weights per category, never hard-code a class's pattern.
- The framework that **does** generalise: per-`(league, category)` weights over
  `{archetype, enrichment, combination, roll-magnitude}` — exactly what a k-NN
  similarity metric expresses when its weights are tuned per category.
- **Archetype-gating magnitude mods recur cross-class** (`+spell` ↔ `+projectile`),
  are handled in *pricing* already by `is_cornerstone` (cornerstone-exact search),
  but are **rare** and undersampled even by the adaptive harvest → can't learn
  their magnitude curve without targeted sampling.

Prior context: the hedonic-regression pricer was scrapped for underpricing
(per-mod additive value off a biased cheap sample). This design **avoids additivity
in the estimate** (k-NN, non-parametric) and keeps the learned number **secondary**
to live ablation until a backtest earns per-category trust. See
[[pricing-rework-phases]], [[pricing-truth-seeking-not-tuning]].

## Decisions (settled in brainstorming)

1. **Purpose:** predictive per-item value estimate, *alongside* live ablation
   (fills thin markets, cross-checks, decomposes value). Not insights-only.
2. **Method:** hybrid — k-NN estimate + descriptive lift/magnitude decomposition.
   The decomposition never feeds the estimate.
3. **Scope:** includes targeted sampling in v1.
4. **Targeting:** auto-detected undersampled-gate candidates, **operator-triggered**
   targeted harvest.
5. **Success bar:** leave-one-out backtest per category (median relative error +
   coverage) + live-ablation sanity. No tuning to the operator's price prior.

---

## Architecture

### Module layout

Split `src/trade/value.rs` into a focused `src/trade/value/` module:

- `mod.rs` — `ValueModel` (per `league → category → CategoryModel`); `build` /
  `rebuild_into` from the corpus; owns the existing descriptive aggregates **and**
  the new per-category **item-vector set** for k-NN.
- `magnitude.rs` — per-`(category, mod)` roll distribution → percentile
  normalisation; per-mod roll→price curve.
- `estimate.rs` — k-NN: `similarity`, neighbour selection, weighted estimate,
  confidence; per-category weight tuning via the backtest.
- `backtest.rs` — leave-one-out evaluation (validation + weight tuning).

The existing descriptive stats (`StatValue` lift / `top_decile_freq` /
`conditional_lift`, `ModPair` co-occurrence, `rank_deconfounded`) **stay** and feed
/insights + decomposition.

### Data model additions

- `CategoryModel` gains:
  - `items: Vec<ItemVector>` — retained corpus rows for this category (fresh,
    deduped): `{ mods: Vec<(stat_id, roll_norm)>, price_divine, indexed }`.
    (~1–5k/category × a handful of categories — cheap in memory.)
  - `mod_rolls: HashMap<stat_id, RollStats>` — per-mod roll quantiles for
    normalisation + the roll→price curve.
  - `weights: SimWeights` — backtest-tuned `(w_jaccard, w_roll)` for this category.
  - `undersampled_gates: Vec<GateCandidate>` — detected targeted-sampling targets.
- `StatValue` is unchanged (presence stats remain for /insights). Magnitude lives
  in `mod_rolls`, not on `StatValue`, to keep the descriptive struct stable.

### Data flow (unchanged in spirit)

corpus JSONL → `rebuild_into` (timer / post-harvest) → `ValueModel` in
`Arc<RwLock<…>>` → read by /paste (estimate) and /insights (decomposition).
k-NN reads the in-memory `items`; **no per-query I/O**.

---

## Components

### 1. k-NN estimate (`estimate.rs`)

- **Query:** a `ParsedItem`'s `(canonical category, mods[stat_id, roll/tier])`.
- **Candidates:** `CategoryModel.items` for the active `(league, category)`
  (already fresh-filtered at build).
- **Similarity(query, item)** =
  `w_jaccard · Jaccard(mod_set_q, mod_set_i)` +
  `w_roll · mean(1 − |roll_norm_q − roll_norm_i|)` over **shared** mods
  (0 shared → roll term contributes 0). Both terms in `[0,1]`; weights normalised
  to sum 1. Mod-set overlap captures archetype/combination; roll proximity captures
  magnitude.
- **Estimate** = similarity-weighted **median** of the top-`k` neighbours'
  `price_divine` (median: robust to round-number clustering + trolls). `k` a
  constant (e.g. 15), clamped to available neighbours.
- **Confidence** = function of (neighbour count, top similarity, neighbour price
  dispersion). Returned as an enum/score so /paste can label it; **low** when few
  or dissimilar neighbours.
- **Relationship to live ablation:** live trade2 ablation (`price_check`) stays
  **primary** for /paste. The learned estimate is **secondary** (cross-check),
  becomes the **fallback** when live ablation returns nothing, and /paste **flags
  divergence** when |learned − live| / live exceeds a threshold.

### 2. Magnitude + decomposition (`magnitude.rs`)

- **Normalisation:** per `(category, mod)`, `RollStats` holds roll quantiles from
  the corpus; `roll_norm = percentile(roll)` in `[0,1]`. Missing/absent roll → mod
  contributes to Jaccard only (roll term skipped).
- **Roll→price curve:** per mod with enough samples, median price by roll bucket
  (for /insights + explanation). Mods below the sample threshold: no curve.
- **Decomposition of an estimate:** rank the query's mods by
  `category lift × roll_norm`; attach the nearest archetype label. Purely
  descriptive — computed beside the estimate, never an input to it. Output shape:
  ordered list of `(mod label, roll percentile, tier, contribution rank)`.

### 3. Archetypes

Archetype clusters are a **descriptive overlay** (for /insights labels +
explanation), not a precondition of the estimate — the k-NN similarity already
groups by archetype implicitly via mod-set overlap. Clustering is computed per
category at build time (mod co-occurrence; pure-Rust, e.g. greedy grouping over the
existing co-occurrence pairs — no ML dep). A query is labelled by its nearest
cluster. (If clustering proves low-value in the plan, it can degrade to "top
co-occurring mods" without affecting the estimate.)

### 4. Targeted sampling

- **Detection** (per category, at build): flag a mod as an *undersampled gate* when
  it is **either** (a) a cornerstone (`is_cornerstone` on its label) **or**
  (b) high-signal (`lift ≥ DRIVER_LIFT` or high `top_decile_freq`) **and**
  `count < MAGNITUDE_MIN_SAMPLE` (too few to fit a roll→price curve). Store as
  `GateCandidate { stat_id, label, count }`.
- **Surface:** /insights lists candidates per category
  (e.g. *"undersampled: +Projectile levels (n=12)"*).
- **Targeted harvest:** a new operator command (e.g. `/harvest mod:<flagged>`, or a
  `mod` option on `/harvest`) that runs the **existing adaptive price-band sweep
  with a stat filter pinned to the chosen mod**, so every fetched item carries it,
  swept across the price range. Reuses adaptive sub-banding + age capture; appends
  normal `Source::Harvest` observations. Operator-triggered (member session/proxy).
  The stat filter uses the same `stats` field already in `TradeQuery`.

### 5. Calibration / backtest (`backtest.rs`)

- **Leave-one-out:** per category with `sample_size ≥` a floor, predict each item
  from the rest via the same k-NN, compute **median |relative error|** and coverage
  (fraction with ≥ min neighbours). Run as a `#[test]`/maintenance routine over a
  corpus fixture **and** exposed for an operator/maintener run over the live corpus.
- **Weight tuning:** at build time, pick each category's `(w_jaccard, w_roll)` from
  a **small fixed grid** (e.g. 5 points) minimising LOO error — so the model
  self-selects combination-vs-magnitude per category. Small grid bounds overfitting
  and build cost.
- **Live sanity:** log learned-vs-live divergence on /paste for ongoing monitoring.

### 6. Surfacing

- **/paste:** live ablation price (primary) + learned estimate (secondary: value,
  confidence label, top drivers, divergence flag).
- **/insights [category]:** drivers (lift) + magnitude curves for top drivers +
  archetype labels + undersampled-gate candidates.
- **New targeted-harvest command/option.**

---

## Testing

- `similarity`: Jaccard + roll terms, shared-mod handling, weight normalisation.
- `magnitude`: percentile normalisation; roll→price curve bucketing; absent-roll.
- **k-NN estimate on synthetic fixtures** — a combination-dominant corpus and a
  magnitude-dominant corpus — asserting backtest tuning picks the right weights and
  the estimate tracks the dominant axis (proves category-adaptivity, the core claim).
- `confidence`: low on thin/dissimilar neighbours.
- `undersampled-gate` detection (cornerstone + high-signal-low-count).
- targeted-harvest query construction (stat filter pinned; adaptive sweep reused).
- LOO backtest harness on a fixture (deterministic error number).
- Regression: empty / thin model ⇒ /paste behaves exactly as today (live ablation
  only); the learned estimate is omitted, never blocks.

## Success criteria

- Leave-one-out median |relative error| reported per category; the learned estimate
  is shown on /paste **only** for categories that clear a per-category trust bar
  (enough data + acceptable LOO error). Thin categories show live-ablation only.
- No code path tunes parameters to match the operator's price prior; calibration is
  measured against held-out corpus prices and live ablation.

## Out of scope (v1)

- Replacing live ablation as the primary /paste price (learned stays secondary).
- Cross-league transfer learning; non-staff/accessory exotica beyond what the
  corpus contains.
- Any ML dependency (kept pure-Rust by construction).

## Risks & mitigations

- **Re-creating the hedonic failure** → estimate is non-additive (k-NN) and
  secondary to live ablation; decomposition is descriptive-only.
- **Overfitting per-category weights** → small fixed grid; honest LOO reporting.
- **Memory growth from retained items** → cap retained items per category (recent +
  representative) if a category's corpus grows large; documented in the plan.
- **Targeted-harvest trade2 load** → operator-triggered, reuses the polite adaptive
  sweep + limiter; one mod at a time.
