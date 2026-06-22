# Rare Pricing Heuristic + Market-Learning System — Design

**Date:** 2026-06-22
**Status:** Approved in brainstorming; pending spec review before planning.
**Supersedes:** the marginal-contribution (hedonic) value path
(`2026-06-19-rare-pricing-marginal-contribution-design.md`), which is removed.

## Why we're redoing this

The hedonic regression value path priced a ~55–240 div staff at **0.2 div**. Root
cause (confirmed with live data): it learns per-mod value from the *cheapest*
comparables (trade2 sorts price-ascending) and the outlier trim discards the
expensive tail — so a top-tier item has no high-value examples and collapses to
the junk floor. The live tell: `base + Spell Physical Damage [181–281%]` alone
returns **median 0.12 div but max 258 div** — a single mod doesn't price the
item; the *combination* of strong mods does, and that value lives in the tail the
regression throws away.

This is the third structural failure of the regression approach (over-constraint
→ fetch cap → sampling/trim bias). We replace it.

**The real goal** (operator's words): not a clever pricing formula, but a system
that **learns over time how prices are made** — e.g. that staves want +spell
levels / spell physical damage / crit, that *physical*-spell staves are worth far
more than fire/cold-spell staves, that the priciest amulets are +skill-levels and
nothing else comes close.

So this design is two coupled parts, built together:
1. a **price-check heuristic** that emulates how the operator prices by hand, and
2. a **market-learning layer** that mines accumulated observations to surface
   insights *and* feed value knowledge back into the price-check.

## Principle: hand-code only knowns, learn the value

The only hand-coded pieces are genuine PoE facts, never value judgments:
- **Cornerstone affixes** searched exact (e.g. `+X to [all/...] skill levels`,
  movement speed) — the rule that "30% vs 25% movement speed is a different item."
- **Pseudo-grouping** (total elemental resistance; local ES/armour/evasion) — a
  search *equivalence*, not a value call (already partly implemented).
- **One loose default band** (~±18%, the operator's "55 → 45–65") for every
  non-cornerstone mod. Band width is a search mechanic, kept fixed (not learned).

Everything that is a *value judgment* — which mods matter for a base, which to
drop, drop order, value tiers — is **adaptive**: it falls out of relaxation now
and is **learned from data** as the corpus fills.

## Part 1 — Price-check heuristic

### Mechanism (replaces the value path)

1. **Parse tier.** Add `tier: Option<u8>` to `ItemStat`, read from the
   Advanced-Mode `{ … (Tier: N) … }` annotation (we already detect the line; we
   just don't extract `N`). Powers cold-start relaxation and per-tier learning.

2. **Build the search query** from the item's **explicit affixes** (runes /
   implicits / enchants already excluded):
   - pseudo-group same-y affixes (extend existing resolution),
   - **cornerstone** affixes → searched exact (min = roll),
   - every other mod → the single loose band around its roll.
   - No pre-dropping of weak affixes (the old "low-tier cutoff" is gone).

3. **Search + relax.** Search cheapest-first (+ the batched ≤10-id fetch already
   shipped). If fewer than `MIN_MATCHES` comparables, **drop the weakest remaining
   affix and retry**, repeating up to a cap. "Weakest" =
   - **cold-start (no learned signal):** highest tier-number first (weakest roll),
   - **learned:** lowest learned value for this base first (so the price-driver
     mods survive and the filler is dropped).
   Cornerstones are dropped last.

4. **Read the price.** `p20/p50/p80` (Quick / Fair / Patient — the existing
   embed) over the cheapest matches of the **tightest query that still returned
   results**. No craftability filter, **no top-trim**, no regression. Because the
   surviving query is well-constrained, its cheapest matches *are* true
   comparables, and the tightest-non-empty query is exactly the strong-mod combo
   that defines the price (super-additivity handled for free). Confidence scales
   with match count and how far we had to relax.

5. **Remove** `src/trade/hedonic.rs`, `marginal_estimate`, `FeatureRow`, and the
   craftability-tier value-path filtering. Keep parsing/`craftability()` only if
   still used elsewhere.

### Why this fixes the staff

Constraining on the strong mods (Spell Phys Dmg, spell-skill levels, crit, …)
filters out the junk single-mod staves; the cheapest *matches of that constrained
query* sit in the tens-of-div range (the 258-div comparable proves the cluster
exists and is reachable). The price emerges from the combination, not a per-mod
average.

## Part 2 — Durable observation corpus

### The atomic observation = one real market listing

Every listing we fetch (in price-checks **and** in harvest) is logged as:

```
Observation {
  timestamp_unix, league,
  base_type, category,
  mods: [ { stat_id, tier: Option<u8>, roll: Option<f64> } ],   // from the fetch response
  price_divine,
  source: Paste | Harvest,
}
```

The trade2 `fetch` response already carries each listing's explicit mods with
`tier` and roll `magnitudes` (verified live), and we already extract
`explicit_stat_ids`. This atomic shape is the ideal training substrate for "which
mods drive price," and it means **every price-check already contributes** every
comparable it fetched — a few pastes yield hundreds of listings, so organic
traffic warms the corpus too.

**`category`** is the clipboard's `Item Class:` (e.g. "Staves"), already captured
by the parser as `item_class` — no new base→category table is needed. `base_type`
(e.g. "Chiming Staff") is stored alongside it for finer analysis later; the
ValueModel keys primarily on `category`, matching the operator's per-class mental
model ("staves want …"). For harvested listings the category is the one being
harvested.

This replaces the current `Probe`/`ProbeLog` (per-check aggregate) with a
per-listing log.

### Durability

- Append-only JSONL, written to a path on a **host-mounted Docker volume**
  (e.g. `/data/observations.jsonl`), configured by `OBSERVATION_LOG_PATH`.
- Today the log is written inside the container and lost on every deploy
  (terraform recreates the container) — so this requires a terraform change:
  add the volume/bind-mount + the env var.
- **Member POESESSIDs are never written** (unchanged). Observations are
  non-secret market data only.
- Corrupt/partial lines are skipped on read; the log never blocks pricing.

## Part 3 — Market-learning layer

### ValueModel (descriptive, no ML)

A loader builds, per `category`, from the observation corpus:
- per `stat_id` (optionally per tier): the price distribution of listings that
  have it, its **lift** (median-with vs the category base median), and its
  frequency among the top-decile (expensive) listings;
- notable **co-occurrences** (mod pairs frequent on the expensive tail).

Loaded at startup and refreshed periodically (and after a harvest). All
best-effort: a thin/absent corpus simply yields no signal for that category.

### Two consumers

- **Insights** — `/insights [category]`: an embed of the learned value-drivers
  for that base, with sample sizes ("Staves: + to all/physical spell levels,
  spell physical damage, crit chance; physical-spell ≫ fire/cold-spell").
- **Feedback into pricing** — the price-check consults the ValueModel to (i) rank
  mods for **relaxation order** (drop lowest learned value first) and (ii) mark
  high-value mods as **value-drivers** kept longest / searched tighter. Requires a
  minimum per-(category, stat_id) sample before it's trusted; below that it falls
  back to the cold-start tier rule. So pricing degrades gracefully and improves as
  the corpus fills.

## Part 4 — Warm-up: on-demand market harvester

Single-guild paste volume is too low to learn broad patterns quickly, so an
operator-triggered harvester seeds the corpus from the live market.

- **Command:** operator-only `/harvest <category>` (gated to the operator).
- **Searches by category, not a single base** — uses the trade2 category filter
  (`type_filters.category`, already supported in `to_payload`) so one harvest
  covers all bases in the class (all staff bases), giving the breadth the
  per-category ValueModel wants.
- **Sampling across the price spectrum:** trade2 search returns only the cheapest
  ~100, so to capture the expensive end (where value signal lives) the harvester
  walks **price bands** — search the category with `min price ≥ 1div`, then `≥5`,
  `≥20`, … — each returning the cheapest 100 *within that band*. This requires
  adding a min-price filter to `TradeQuery`/`to_payload` (trade2
  `trade_filters.price`).
- Every fetched listing → an `Observation { source: Harvest }` in the same corpus.
- **Politeness:** runs through the existing throttle + the **operator session**
  (global `POESESSID`) / default proxy, bounded per cycle so it never starves
  member price-checks or trips 429s.
- **Scheduled background harvest is deferred** to a later phase once the on-demand
  path is proven safe.

## Error handling

- Price-check never returns empty: if even the fully-relaxed query is thin, show a
  low-confidence base estimate rather than "No comparable listings found."
- Learning + harvest are best-effort and never block pricing.
- Harvest respects rate limits; a 429/seam error aborts the cycle gracefully with
  a partial corpus contribution.

## Testing

Offline (no network):
- Tier parsing from annotations (incl. hybrid/desecrated/crafted prefixes).
- Query build: cornerstone-exact vs loose-band vs pseudo-group on the staff
  fixture; relaxation drops weakest-by-tier (cold-start) and weakest-by-value
  (with a seeded ValueModel); cornerstones dropped last.
- Price read = p20/p50/p80 of cheapest matches of the tightest non-empty query.
- Observation serialize/round-trip; corrupt-line skip.
- ValueModel aggregation over a synthetic corpus recovers a planted driver
  ("stat X drives price for category Y") and the lift/top-decile metrics.
- Insights rendering; feedback selection (trusted only above the sample
  threshold, else tier fallback).
- Harvester price-band query construction + listing extraction against fixtures.

Live acceptance:
- The Chiming Staff prices in a sane div range (not 0.2), labelled with how much
  it relaxed.
- `/harvest staff` fills the corpus; `/insights staff` then shows sensible drivers.

## Phasing (designed together, built in order)

1. **Price-check heuristic** — tier parse, relax-and-read pricing, remove the
   regression. *Fixes production*; stateless; cold-start tier-based relaxation.
2. **Observation corpus** — per-listing JSONL on a mounted volume + the terraform
   volume change; every price-check logs its fetched listings.
3. **Harvester** — `/harvest <category>` price-banded warm-up + the min-price
   query filter (fills the corpus before learning lands).
4. **Learning** — ValueModel aggregation, `/insights`, and feedback into the
   price-check's relax order / value-driver selection.

Each phase is an independently shippable plan; we'll write a plan per phase.

## Non-goals (YAGNI)

- No ML — descriptive aggregation only (revisit later).
- Band width is not learned (fixed loose default).
- Scheduled background harvest deferred (Phase 4+).
- "Break it down" (ablation breakdown): out of scope here — re-derive from the
  price-check or the ValueModel later; flag as follow-up.
- No change to currency conversion, sessions, proxy, the throttle, or the batched
  fetch (all reused as-is).

## Files touched (indicative)

| Area | Change |
|---|---|
| `src/itemtext.rs` | `ItemStat.tier`; parse `(Tier: N)` |
| `src/trade/query.rs` | cornerstone-exact + loose band build; min-price filter in `to_payload` |
| `src/trade/pricecheck.rs` (new) / `ablation.rs` | relax-and-read price-check; remove hedonic/marginal |
| remove `src/trade/hedonic.rs` | delete the regression |
| `src/observe.rs` (new, replaces `pricelog.rs`) | `Observation` + append-only JSONL on the volume |
| `src/trade/client.rs` / `model.rs` | extract per-listing `{stat_id,tier,roll}` from fetch; min-price filter |
| `src/learn.rs` (new) | `ValueModel` aggregation + feedback + insights rendering |
| `src/trade/harvest.rs` (new) | price-banded harvester (operator session, throttle) |
| `src/discord/` | `/insights`, operator `/harvest`; price embed wiring |
| `src/config.rs` | `OBSERVATION_LOG_PATH`, harvest + value-model config |
| infra (terraform) | mounted volume + env var |
