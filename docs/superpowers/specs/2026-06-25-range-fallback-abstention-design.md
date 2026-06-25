# Phase 1 — Calibrated-Range Secondary Layer with Abstention — Design

**Goal:** When the live trade2 ablation can't price a `/paste` item — it returns no
listings *or* errors/rate-limits — fall back to a **corpus-derived price range** with
honest confidence, or **abstain** when the corpus can't speak credibly. Live ablation
stays the primary `/paste` price; this only fills the gap currently shown as
"No comparable listings found" or "Couldn't reach trade right now."

**Architecture (one sentence):** Repurpose the (currently-unused) per-`(league,category)`
k-NN `CategoryModel::estimate` from a single point into a `range_estimate` that builds an
**exact-mod-set-first, adaptive-K** comparable pool, emits **p20/p50/p80** as
floor/fair/ask with **band-width confidence**, and **abstains** (returns `None`) on a
thin/dissimilar pool or a top-decile result — surfaced on `/paste` only when live
ablation yields nothing.

**Tech stack:** Rust. Extends `src/trade/value/estimate.rs` (range estimator),
`src/trade/value/mod.rs` (`CategoryModel`), `src/trade/mod.rs` (`TradePricer`), and
`src/discord/paste.rs` + `embeds.rs` (fallback surfacing). No new deps.

---

## Why (grounding)

From the 10-expert debate consensus (memory `pricing-strategy-debate-consensus`) and the
Phase-0 honest harness: the corpus model has positive skill only for Staff, the expensive
tail is unservable from the corpus (underpriced 5–20×), and listing prices are noisy — so
a corpus estimate must be a **range with abstention**, never a confident point. After
Phase 0, the corpus layer feeds only `/insights`; `/farm` is poe.ninja-based and the
`/paste` learned line was suppressed. **The sole pricing surface for this range is the
`/paste` empty/errored-ablation fallback** (frequency unverified — Phase 4 telemetry will
size it; building it now is an explicit, accepted decision). `/paste` live-success output
is unchanged.

Existing machinery this repurposes (all `pub`, currently unused but not dead-code-flagged
in this binary): `CategoryModel::estimate` (point, fixed `K_NEIGHBORS=15`),
`query_from_stats`, `ValueEstimate`, `Confidence`, `weighted_median`, `relative_spread`,
and `TradePricer::learned_estimate`. There are **no `#[allow(dead_code)]` to remove**.

## Decisions (settled in brainstorming, 2026-06-25)

1. **Scope:** full range + abstention layer (consensus Phase 1). The Staff-only coarse
   tier (consensus Phase 3) and conformal calibration (Phase 2) are **out of scope**.
2. **Quantiles:** p20/p50/p80 → floor/fair/ask, matching the live embed's
   "Quick sale / Fair / Patient" labels so the fallback reads consistently.
3. **Confidence from band width** (`ask/floor`): ≤2× High, ≤5× Medium, else Low.
4. **Provisional thresholds** (`MIN_POOL=8`, relax `Jaccard ≥ 0.6`, width bands 2×/5×) are
   sensible defaults, **not tuned to any price prior**; Phase 2's calibration tunes them.

## Components

### 1. Range estimator — `CategoryModel::range_estimate` (`estimate.rs`)

`pub fn range_estimate(&self, query: &[(String, Option<f64>)]) -> Option<RangeEstimate>`
where `RangeEstimate { floor: f64, fair: f64, ask: f64, confidence: Confidence, pool: usize }`.

- **Comparable pool, exact-first:**
  1. **Exact mod-set:** items whose set of `stat_id`s equals the query's set.
  2. If `pool.len() < MIN_POOL` (8), **relax** to items with `Jaccard(mod_set) ≥ 0.6`
     (a superset including the exact matches), taking **all** that qualify — **adaptive K,
     replacing the fixed `K_NEIGHBORS=15`** that diluted exact matches.
  - Mod-set membership only — **roll magnitude is NOT used for pool selection** (it proved
    non-predictive within a mod-set). Roll percentile is retained only for display / as a
    tie-break ordering within an equal-Jaccard tier. **No recency weighting** (the 14-day
    freshness filter at corpus build already applies).
- **Range:** empirical p20/p50/p80 of the pool's `price_divine` → floor/fair/ask.
- **Confidence:** from `ask/floor` width per Decision 3.

### 2. Mandatory abstention (`range_estimate` returns `None`)

- **Thin/dissimilar pool:** fewer than `MIN_POOL` items even after the Jaccard≥0.6 relax →
  abstain. Never fabricate a range from dissimilar neighbours.
- **Top-decile guard:** if `fair` (or `ask`) lands at/above the category's top price decile
  (computed once at build, stored on `CategoryModel`), abstain — the corpus underprices the
  expensive tail, so route those to live rather than mislead.

### 3. `TradePricer` accessor (`trade/mod.rs`)

`pub fn range_estimate(&self, item: &ParsedItem, league: &str) -> Option<RangeEstimate>` —
mirrors the old `learned_estimate`: canonical category → poison-safe model read → category
lookup → `query_from_stats` (explicit mods only, the existing invariant) →
`CategoryModel::range_estimate`. Synchronous (in-memory read only). The old
`learned_estimate` (point) is **replaced** by this.

### 4. `/paste` surfacing (`paste.rs` + `embeds.rs`)

In `run_pricing`:
- **Live ablation succeeds** (`listing_count > 0`): unchanged — live price only.
- **Live returns empty** (`listing_count == 0`) **or `price()` errored** (rate-limited /
  unreachable): call `pricer.range_estimate`. If `Some`, render a clearly-secondary
  fallback, e.g.:
  *"📊 No live listings — corpus estimate: **5–30 div** (fair ~12) · low confidence"*
  (floor–ask, fair, confidence label). If `None` (abstain), keep today's message
  ("No comparable listings found" / "Couldn't reach trade right now").
- The render lives in `embeds.rs` as a small pure helper (`range_fallback_line` /
  a dedicated embed) so it is unit-testable without Discord I/O.

## Data flow

Unchanged build path (corpus → `rebuild_into` → `build_category`); `build_category` also
stores the per-category **top-decile price threshold** for the abstention guard.
`/paste` → live ablation → (on empty/error) `TradePricer::range_estimate` →
`CategoryModel::range_estimate` (in-memory). No per-query I/O.

## Testing

- **Pool selection:** exact-mod-set chosen first; relax to Jaccard≥0.6 only when exact `<
  MIN_POOL`; adaptive K (all qualifying, not capped at 15).
- **Range:** p20/p50/p80 → floor/fair/ask on a known fixture; confidence from width
  (≤2×/≤5×/else).
- **Abstention:** thin/dissimilar pool → `None`; `fair` ≥ top-decile → `None`.
- **Roll excluded from selection:** two items identical in mod-set but different rolls are
  both in the pool (roll doesn't gate membership); roll only affects display/tie-break.
- **`/paste` fallback:** renders the range on live-empty and on live-error; abstains
  (keeps the old message) when `range_estimate` is `None`; **live-success path unchanged**
  (no range shown) — regression-guarded.
- Pure render helper tested for format (floor–ask, fair, confidence).

## Success criteria

- On `/paste` for an item with no live listings, the user sees a labelled, low/medium/high-
  confidence corpus **range** (or an honest abstention), instead of a bare "not found."
- The estimator abstains rather than emit a range from dissimilar comps or for top-decile
  items. No path tunes to the operator's price prior; bands are raw empirical quantiles.

## Out of scope (later phases)

- **Conformal calibration** of the bands (Phase 2 — needs temporal/forward-split data we
  don't have yet; bands here are raw empirical quantiles, provisional).
- **Staff-only coarse value-tier** (Phase 3).
- The **temporal/forward split** and **trade2 empty/rate-limited telemetry** (Phase 4 —
  would retroactively size how often this fallback fires).
- Any change to `/farm`, `/insights`, or the live-success `/paste` path.

## Risks & mitigations

- **Fallback rarely fires** (live ablation usually finds something via relax-and-read): the
  range layer is then low-traffic — accepted; Phase 4 telemetry will quantify it. Cost is
  bounded (in-memory, no I/O).
- **Re-creating a misleading point:** mitigated by ranges + width-confidence + hard
  abstention (thin pool, top-decile), and by staying strictly the *fallback*, never
  overriding live.
- **Provisional thresholds mis-set:** they are explicit constants, honestly labelled
  provisional, and Phase 2 calibrates them against held-out data.
