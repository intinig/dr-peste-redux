# Phase 4 — Market Learning (ValueModel + `/insights` + pricing feedback)

**Status:** approved design (2026-06-22). Phase 4 of the pricing rework.
Predecessors (shipped): Phase 1 heuristic price-check (PR #16), Phase 2 durable
observation corpus (PR #17), Phase 3 `/harvest` warm-up (PR #18). Parent design:
`2026-06-22-pricing-heuristic-and-market-learning-design.md`.

## Goal

Mine the accumulated observation corpus into a per-category **ValueModel** that:

1. **Surfaces how prices are made** — `/insights [category]`: human-readable
   value-drivers (the primary deliverable; the operator's goal is to *learn how
   prices are made*, e.g. "staves want +spell levels / spell physical damage /
   crit; physical-spell ≫ fire-spell").
2. **Feeds learned value back into the price-check** — relaxation order and band
   tightness, so pricing improves as the corpus fills and degrades gracefully
   when it is thin.

**Descriptive aggregation, not ML.** The goal is explanation, not a learned price
function. A prior hedonic regression failed structurally here (priced a ~240-div
staff at 0.2 div: value is super-additive — the *combination* of mods, not their
sum — and it trained on cheapest-comparable samples with a trimmed tail). The
pricing problem is solved by *query construction* (relax-and-read), not a learned
price function. Descriptive stats are directly interpretable, fit the data scale
(single guild; hundreds–low-thousands of observations, observational and
selection-biased toward cheapest listings), and need no training pipeline. The
one real weakness of univariate statistics — confounding by co-traveling mods —
is addressed by a lightweight deconfounding pass used **only** for the `/insights`
ranking (see ValueModel §). Heavier ML is deferred, not foreclosed: the corpus is
a clean JSONL substrate a model can read later if aggregation proves insufficient.

## Corpus today (validates the design)

The live corpus (`/opt/dr-peste-redux/data/observations.jsonl`) after the first
harvest holds 480 observations:

- 400 harvest (`source:harvest`, `category:"Staff"`), spanning **0–50 div,
  median 12.5** across 13 staff bases — the price-banded sweep captured the
  expensive tail organic pastes never surface.
- 80 paste (`source:paste`, `category:"Staves"`), median 0.15 div, max 1.0.

Two facts this confirms: (a) the **category-key divergence** is real and live —
paste logs the clipboard Item Class (`"Staves"`), harvest logs the trade2
category (`"Staff"`) — they must be reconciled; (b) the harvest warm-up gives the
ValueModel a real expensive tail to learn from, directly attacking the 0.15-div
accuracy ceiling.

## Architecture & data flow

```
corpus JSONL ──build (startup / after-harvest / every VALUE_REFRESH_MINS)──▶ Arc<RwLock<ValueModel>>
                                                                              ├─▶ /insights         (reads)
                                                                              └─▶ price()/build_baseline (reads)
```

The ValueModel is rebuilt from the append-only corpus; it is never the source of
truth, just a derived in-memory index. Build is best-effort: a build failure
keeps the last good model (or empty); a thin/absent corpus simply yields no
signal for that category.

### Module layout

- `src/trade/value.rs` *(new)* — `ValueModel`, `CategoryModel`, `StatValue`,
  `ModPair`; `canonical_category()` + the alias table; `ValueModel::build(...)`;
  driver-selection and relax-rank helpers.
- `src/observe.rs` — add a **read** path (`read_all()` / iterator) that skips
  corrupt lines. Currently write-only.
- `src/discord/insights.rs` *(new)* — `/insights [category]` command + embed +
  category autocomplete.
- `src/trade/query.rs` — `build_baseline` gains an optional value context
  (the model + the item's resolved canonical category).
- `src/trade/mod.rs` — `price()` reads the model; periodic + post-harvest rebuild.
- `src/discord/mod.rs` — `Data` gains `value: Arc<RwLock<ValueModel>>`.
- `src/main.rs` — build the model at startup, spawn the refresh task, register
  `/insights`.
- `src/config.rs` — the thresholds/intervals below (env-overridable; sane
  defaults).

## Canonical category

`canonical_category(raw: &str) -> String`: a lowercase-trim lookup in a **static
alias table** mapping the clipboard Item Class to the trade2 category text, e.g.
`Staves→Staff`, `Wands→Wand`, `Sceptres→Sceptre`, `Quarterstaves→Quarterstaff`,
`Amulets→Amulet`, `Rings→Ring`, `Belts→Belt`, `Body Armours→Body Armour`,
`Helmets→Helmet`, `Gloves→Gloves`, `Boots→Boots`, … An unknown key passes through
unchanged (best-effort identity). The PoE item-class taxonomy is a closed, known
set, so this is a maintained artifact like `pseudo_map.json` — re-check after each
major PoE2 patch.

- Applied at **build/read** — the authority. Folds today's 80 `"Staves"` + 400
  `"Staff"` into one `"Staff"` key.
- Also applied at **paste write time** going forward (harvest already writes the
  canonical trade2 category), so new observations are stored canonical and the
  file stays clean. The read-side fold remains the safety net for legacy lines.

## ValueModel

Built per canonical category by streaming the corpus:

```
struct ValueModel { categories: HashMap<String, CategoryModel> }   // key = canonical category

struct CategoryModel {
    category: String,            // canonical, e.g. "Staff"
    sample_size: usize,          // listings in this category
    base_median: f64,            // median price_divine across the category
    stats: Vec<StatValue>,       // sorted by deconfounded driver rank (see below)
    cooccurrences: Vec<ModPair>, // top mod pairs on the expensive tail
}

struct StatValue {
    stat_id: String,
    label: Option<String>,       // human label via StatCatalog if resolvable
    count: usize,                // listings carrying this stat
    median_with: f64,            // median price of listings carrying it
    lift: f64,                   // median_with / median_without  (marginal lift)
    conditional_lift: Option<f64>, // deconfounded lift (insights ranking); None if subset too thin
    top_decile_freq: f64,        // fraction of top-10%-priced listings carrying it
}

struct ModPair { a: String, b: String, count: usize }  // co-occurrence count among the top decile
```

### Metrics

- **base_median** — median `price_divine` across the category's listings.
- **lift** (univariate marginal lift) — `median_with / median_without`: the
  median price of listings carrying the stat over the median of those without it.
  >1 ⇒ the stat associates with higher price. Used by the **pricing feedback**.
  (Revised 2026-06-22 from `median_with / base_median`, which collapses to ≈1 for
  a driver that is common among priced listings — base_median is then dragged up
  to the driver's own price level. `median_without` is robust to driver
  prevalence and is the standard lift definition. `base_median` is retained as a
  reported field and as the denominator fallback when *every* listing carries the
  stat.)
- **top_decile_freq** — fraction of the most-expensive-10% listings (by
  `price_divine`) that carry the stat. "Is it actually *on* the expensive items."
- **co-occurrence** — most frequent stat *pairs* among the top-decile listings.
  Captures the "the combo of two mods defines price" / "physical-spell ≫
  fire-spell" signal that a per-mod view misses.

### Deconfounded driver ranking (insights only)

Univariate lift is confounded: a worthless mod that co-travels with a real driver
inherits its lift. A greedy pass deconfounds the **ranking** shown in `/insights`
(it does **not** affect pricing):

1. Rank-1 driver = the stat with the highest univariate `lift` (subject to the
   trust gates below). Its `conditional_lift` = its raw `lift`.
2. For each remaining stat, compute **conditional lift** = median price of
   listings carrying the stat, *restricted to listings that carry none of the
   already-picked drivers*, ÷ the median price of the rest of that
   driver-free subset. A mod that only had value via co-travel collapses to a
   conditional lift ≈ 1.
3. Pick the next-highest conditional lift, add it to the driver set, and repeat.
   Stop when no remaining stat clears `DRIVER_LIFT`, or the driver-free subset
   falls below `MIN_STAT_SAMPLE`. Remaining stats are ranked by raw lift with
   `conditional_lift = None` (rendered "unconfirmed").

`CategoryModel.stats` is stored in this deconfounded rank order. Pricing reads
`StatValue.lift` (univariate) regardless of rank.

### Trust gates (constants; env-overridable)

- `MIN_CATEGORY_SAMPLE` (default 50) — below this, the category is untrusted:
  `/insights` shows "not enough data yet," and pricing feedback is **off**
  (cold-start).
- `MIN_STAT_SAMPLE` (default 15) — a stat needs at least this `count` for its
  lift to be trusted (drives pricing; gates conditional-lift computation).
- `DRIVER_LIFT` (default 1.5) — a trusted stat with `lift ≥ DRIVER_LIFT` is a
  **value-driver**.
- `VALUE_DRIVER_BAND` (default ±9%) vs the existing loose band (±18%).
- `VALUE_REFRESH_MINS` (default 60) — periodic rebuild interval.

### Refresh

Built at startup; rebuilt after each `/harvest` completes; rebuilt every
`VALUE_REFRESH_MINS`. All best-effort and off the pricing hot path. The model is
held behind `Arc<RwLock<ValueModel>>` in `Data`; readers take a short read lock
and clone what they need.

## `/insights [category]`

- **No arg** → ephemeral embed listing categories that have trusted data, with
  sample sizes, prompting the user to pass one.
- **`/insights staff`** (argument canonicalized; autocomplete sourced from the
  model's category keys) → embed of the deconfounded value-drivers. Each row:
  label, **raw lift** and **independent (conditional) lift**, **top-decile
  frequency**, and sample size — e.g.
  `spell physical damage — 3.1× (independent 2.8×) · in 80% of priciest · n=312`.
  Confounded mods read e.g. `… — 2.4× (independent 1.05×, rides spell phys dmg)`.
  A "Top combos on expensive items" line lists the leading co-occurrence pairs.
  Footer: corpus size for the category + last-refresh age.
- **Thin/unknown category** → friendly "not enough data yet for X."
- **Access: everyone.** Single-guild, non-secret market data; no gating.

## Pricing feedback (relax-order + value-drivers)

In `build_baseline`, resolve the item's canonical category
(`canonical_category(item.item_class)`) and look up its `CategoryModel`.

When the category is **trusted** (`sample_size ≥ MIN_CATEGORY_SAMPLE`):

- **Band tier** per explicit mod:
  - cornerstone → exact (`min = roll`, no max) — unchanged;
  - **value-driver** (trusted stat, `lift ≥ DRIVER_LIFT`) → tight band
    `VALUE_DRIVER_BAND` (±9%);
  - everything else → the existing loose band (±18%).
- **Relaxation drop order** (relaxation drops the weakest survivor first;
  `gather_comparables` pops the last filter): order so the drop sequence is
  *low-value normal mods → high-value normal mods → value-drivers → cornerstones
  last*. Rank normal mods by ascending `lift`; an untrusted stat
  (`count < MIN_STAT_SAMPLE`) falls back to the cold-start tier rank and the loose
  band.

When the category is **untrusted/empty**, behavior is **exactly today's**: tier
ordering (weakest tier-number dropped first, cornerstones last) and a uniform
loose band. This is a hard requirement and is regression-tested: an empty
ValueModel must produce a byte-identical `TradeQuery` to the current code.

Confounding is benign in the pricing path: a confounded mod flagged as a driver
co-occurs with the real driver, so constraining it constrains essentially the
same comparable set. That is why pricing uses raw univariate lift and the
deconfounding pass is reserved for `/insights`.

*Note on band widths:* this introduces a second **fixed** band width (±9% for
drivers alongside ±18% for normals). Band width is still not *learned* — the
earlier "band width is not learned" non-goal holds; there are simply two fixed
tiers now.

## Error handling

- Learning never blocks or panics pricing: corrupt corpus lines are skipped on
  read; a build failure keeps the last good model; a missing/thin model routes
  pricing to the cold-start path.
- `/insights` on an unknown or thin category returns a friendly message, never an
  error.
- The refresh task logs and continues on failure; it never aborts the process.

## Testing

**Offline (no network):**

- `canonical_category`: folds known aliases (`Staves→Staff`), is idempotent on an
  already-canonical input, and passes unknown inputs through unchanged.
- `ObservationLog` read path: returns well-formed observations and **skips
  corrupt/partial lines**.
- `ValueModel::build` over a synthetic corpus: recovers a **planted driver**
  (high lift, high top-decile frequency), computes `base_median`, and surfaces the
  planted co-occurrence pair; a thin category is marked untrusted.
- **Deconfounding (the key proof):** plant a true driver `A` (listings with `A`
  are expensive regardless of `B`) and a worthless co-traveler `B` (appears only
  alongside `A`; `B`-without-`A` listings are cheap). Assert both show high *raw*
  lift, but the deconfounded ranking keeps `A` first and collapses `B`'s
  `conditional_lift` to ≈1.
- Driver selection honors the trust gates (`MIN_*`, `DRIVER_LIFT`).
- `build_baseline` with a seeded model: a value-driver gets the tight band and is
  dropped last (before cornerstones); normals drop in ascending-value order.
- `build_baseline` with an **empty** model: produces a `TradeQuery` identical to
  the current tier-based behavior (regression guard).
- `/insights` rendering: category menu, per-category drivers (raw + conditional
  lift, top-decile, co-occurrence), and the thin-data path.

**Live acceptance:**

- `/insights staff` shows sane drivers from the 400-staff corpus (spell physical
  damage, +to spell skill levels, crit).
- A heavily-modded staff prices in a sane div range — the warm corpus plus the
  value-driver constraint lifts it off the 0.15-div floor — labelled with how far
  it relaxed.

## Phasing within Phase 4

A single implementation plan, but ordered so each task is independently testable:

1. `canonical_category` + alias table; `ObservationLog` read path.
2. `ValueModel` build (lift, top-decile, co-occurrence) + trust gates.
3. Deconfounded conditional-lift driver ranking.
4. Refresh wiring (startup / periodic / post-harvest) + `Data` field.
5. `/insights` command + embed + autocomplete.
6. Pricing feedback in `build_baseline` (relax-order + value-driver bands) with
   the empty-model regression guard.

## Non-goals (YAGNI)

- No ML / learned price function (deferred; revisit only with more data **and**
  evidence aggregation is insufficient).
- Band widths remain fixed (two tiers), not learned.
- Deconfounding affects only `/insights` ranking, never pricing.
- No per-base_type model in v1 (per canonical category; base_type is retained in
  the corpus for a possible future drill-down).
