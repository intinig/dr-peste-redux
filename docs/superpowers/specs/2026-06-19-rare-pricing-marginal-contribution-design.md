# Rare Pricing — Marginal-Contribution (Hedonic) Estimate — Design

**Date:** 2026-06-19
**Status:** Approved in brainstorming; pending spec review before planning.
**Builds on:** craftability-tier pricing (PR #11/#12) and the rate-limit throttle
(PR #13). Fixes the "No comparable listings found" failure on heavily-modded rares.

## Problem

`/paste` on a multi-mod rare (e.g. a 6-affix Chiming Staff) returns **"No
comparable listings found."** Root cause (confirmed live via the operator proxy):
`build_baseline` emits one banded stat filter per mod — including socketed
**runes** — and ANDs them all. The result set collapses to zero almost
immediately, and `gather_comparables` only relaxes `max_relax = 3`:

| filters | results (live) |
|---|---|
| base + ilvl≥80 + uncorrupted | 3,868 |
| + 1 | 12 |
| + 2 | 4 |
| + 3 | **0** |
| 8 (full query) | **0** |

So gather returns 0 → estimate prices nothing → "No comparable listings." The boot
in PR #11 priced only because its resistances pseudo-collapse into one filter; a
caster staff has no such collapse and over-constrains.

**Value model (from the user).** An item's value is the **combination** of its
mods, weighted by each mod's **inherent worth** (tier *and* mod type — a T1 Mana
Regen is worth less than a T2 Spirit). A single mod is too diluted to indicate
price. We should **derive** the full item's value from heavily-overlapping
partial comparables (e.g. two 5-affix staves overlapping on 4 mods pin down the
6th), not require an exact match. Treat the output as an **estimate with
confidence**, which may over/under-shoot when extrapolating beyond combos the
market actually has.

## Verified capability (live probe)

The trade2 `fetch` response carries, per listing, the data needed to model value:

- `item.explicitMods[]` = objects with `hash` (`"stat.explicit.stat_2768835289"`),
  `description`, and `mods[].magnitudes` (roll min/max) + `mods[].tier`.
- `item.extended.hashes.explicit` = `[["explicit.stat_2768835289", [positions]], …]`
  — the item's explicit stat-id set.

Our own mods' stat ids come from `build_baseline` (`StatFilter.id`, e.g.
`explicit.stat_2768835289`). Stripping the leading `stat.` aligns the fetch hash
to our filter id, so we can determine **id-exactly** which of our mods each
comparable has.

## Design

Two paths, chosen by how populated the exact query is.

### Fast path (common items — unchanged behaviour)

Build the query as today **but exclude runes** (and implicits/enchants/granted
skills) from the stat filters — they are sockets/base-inherent, not the item's
affixes. Search. If the constrained result yields **≥ `MIN_COMPARABLES` (10)**,
price exactly as today: craftability-filter (`explicit_count ≤ ours`) + trimmed
p20/p50/p80. Most items (few mods, or mods that pseudo-collapse) stay one search.

### Value path (thin exact query — the bug case)

When the exact query yields `< MIN_COMPARABLES`, estimate by modelling each mod's
marginal contribution:

1. **Sample with variety.** Issue, via the existing `Comparables` seam:
   - one **base** query (type + misc/ilvl, no stat filters) → the floor + many
     low-mod items, and
   - one query per **affix mod** (`base + that one mod`, banded) → rares carrying
     that mod (which incidentally carry *other* of our mods too, giving
     multi-mod observations for free).

   That is `N + 1` searches for `N` affix mods (`N ≤ 6` typical; cap at
   `PROBE_CEILING = 16`). All paced by the PR #13 throttle, so no 429s.
2. **Pool + dedup** the fetched listings by listing id (a new `Listing.id`), so a
   rare returned by several sub-queries counts once. Restrict to the craftability
   tier (`explicit_count ≤ ours`) so we don't price against more-filled items.
3. **Feature per comparable:** a binary vector over our `N` mods — present iff our
   mod's stat id is in the comparable's explicit stat-id set (a new
   `Listing.explicit_stat_ids`).
4. **Fit** `ln(price_divine) ~ β₀ + Σ βᵢ·presentᵢ` by ordinary least squares
   (hand-rolled normal equations + Gaussian elimination on an `(N+1)×(N+1)`
   system — no new dependency), after trimming price outliers (reuse the existing
   bottom-fraction trim, extended to also drop the top fraction). Each `βᵢ` is
   mod *i*'s marginal log-value, estimated **jointly** so co-occurring mods
   triangulate and dilution is removed.
5. **Predict** the full item (all features = 1): `p50 = exp(β₀ + Σ βᵢ)`. Build the
   interval from the fit's residual quantiles: `p20 = p50 · exp(q20(resid))`,
   `p80 = p50 · exp(q80(resid))`. Floor `p50` at the base-tier median (a full item
   is never worth less than a bare base).
6. **Confidence:** `High` only with a healthy sample and little extrapolation;
   `Low` when the sample is small, few comparables carry several of our mods, or
   the prediction extrapolates well past observed combos. The embed labels it
   "estimated from marginal mod values (base + N mods modelled)".

### Latency estimate (don't surprise the user)

The fast path is ~1 search; the value path is `N+1` paced searches + fetches and
can run for tens of seconds. So the user is told *before* the wait, and only when
the value path actually triggers:

- `/paste` defers immediately ("Pricing…"). The exact query (1 search) runs first.
- **Fast path:** result replaces "Pricing…" — no extra message.
- **Value path:** before issuing the sub-queries, the pricer reports
  `(sub_query_count, estimated_duration)` to the caller through an async progress
  hook; the discord handler edits the deferred message to
  *"Heavily-modded item — modelling value from K market samples (~Ts)…"*, then
  replaces it with the estimate when done.
- **The estimate** comes from the throttle, which knows the live rate: a new
  `RateLimiter::estimate(ep, n) -> Duration` returns the expected wall-clock for
  `n` more requests on `ep` given the current window + learned rules. The pricer
  sums the search and fetch estimates for the `N+1` sub-queries, rounds up, and
  reports it. It is a *ballpark* shown so the wait isn't surprising, not a promise.

The progress hook is a small async trait (`PriceProgress`) with a no-op
implementation used by tests and any non-interactive caller, so the pricing core
stays decoupled from discord.

### Robustness & fallbacks

- **Identifiability guards before fitting:** drop any feature with no variance in
  the sample (a mod always-present or always-absent — its `βᵢ` is unidentifiable);
  its contribution is folded into the intercept / handled by the floor. Require a
  minimum pooled sample (e.g. `≥ 20` after trim) and a non-singular normal matrix.
- **Sanity clamp:** clamp each `βᵢ ≥ 0` (a mod cannot reduce value below the
  model's base in this domain — negative coefficients are confounding artifacts),
  then predict.
- **Fallback chain** when the fit can't run (too few/collinear) — never error:
  base + craftability-tier trimmed percentile (always populated, `Low`
  confidence, labelled "priced on base tier — too few comparables to model").

### Breakdown (scope)

`breakdown` ("Break it down") is **out of scope** for this change — it keeps its
current ablation. For over-modded items its baseline is the same thin query, so it
may be uninformative; the `βᵢ` marginal values are a natural forward breakdown and
rebuilding the breakdown on them is a tracked follow-up, not part of this fix.

## Components / files

| File | Change |
|---|---|
| `src/trade/model.rs` | `Listing` gains `id: String` and `explicit_stat_ids: Vec<String>`. |
| `src/trade/client.rs` | `parse_fetch` extracts the listing id and the explicit stat-id set (from `extended.hashes.explicit`, falling back to `explicitMods[].hash`, normalising off the `stat.` prefix). |
| `src/trade/query.rs` | `build_baseline` stops emitting filters for runes/implicits/enchants (affix explicits only). A helper to derive the base-only and base+single-mod sub-queries. |
| `src/trade/hedonic.rs` (**new**) | Pure model: feature matrix → trimmed OLS (`fit`) → `predict`; plus the marginal-contribution estimator orchestrating sampling via `Comparables`. All numeric core pure + unit-tested. |
| `src/trade/ablation.rs` / `mod.rs` | `estimate`/`price` route to the value path when the exact query is thin; fast path otherwise. `price` gains a `&dyn PriceProgress` arg and reports `(count, est)` before the value-path sub-queries. |
| `src/trade/model.rs` (`EstimateBasis`) | add a `Marginal` basis variant for labelling/confidence. |
| `src/trade/limiter.rs` | `RateLimiter::estimate(ep, n) -> Duration` — expected wall-clock for `n` more requests given the current window + learned rules (pure helper, unit-tested). |
| `src/trade/mod.rs` (or `trade/progress.rs`) | `PriceProgress` async trait (`value_path(count, est)`) + a no-op impl for tests/non-interactive callers. |
| `src/discord/paste.rs` | implement `PriceProgress` to edit the deferred response ("modelling value from K samples (~Ts)…"); pass it into `price`. |

## Data flow

```
price(item)
  query = build_baseline(item)           # affix explicits only (no runes)
  exact = comparables(query)             # fast path
  if exact.len() >= MIN: percentile-price(craft_filter(exact))   # unchanged
  else:                                  # value path
    progress.value_path(N+1, limiter.estimate(...))   # show ETA before the wait
    base   = comparables(base_query)
    perMod = [comparables(base + mod_i) for mod_i in query.stats]
    pool   = dedup_by_id(base ++ perMod) |> craft_filter
    X,y    = features(pool, our_stat_ids), ln(price)
    fit    = ols_trim(X, y)  (guards/fallback)
    est    = predict(fit, all_present)  → p20/p50/p80 + confidence
```

## Testing

Offline (pure, no network):
- **`parse_fetch` extraction:** a fixture item with `extended.hashes.explicit` and
  `explicitMods[].hash` → `Listing.explicit_stat_ids` normalised correctly; listing
  id captured; existing currency-drop behaviour unchanged.
- **OLS recovery:** synthetic comparables where base=1 div, mod A=+1, mod B=+2
  (multiplicative on log) → fit recovers β, `predict(A&B)` ≈ 4 div within
  tolerance; duplicate rows deduped; outliers trimmed.
- **Triangulation:** a sample containing only ≤5-of-6 combos still predicts the
  full-6 price (the headline requirement).
- **Guards/fallback:** always-present feature dropped; tiny/collinear sample →
  base-tier fallback with `Low` confidence.
- **Routing:** exact query ≥ MIN → fast path (no extra searches, asserted via a
  fake `Comparables` call counter); < MIN → value path issues `N+1` sub-queries.
- **Latency estimate:** `RateLimiter::estimate(ep, n)` grows with `n` and the
  rules (e.g. n under one window → ~0; n beyond it → ≈ extra windows); the value
  path fires `PriceProgress::value_path` exactly once with `count == N+1`, and the
  fast path never fires it (asserted via a recording no-op progress).

Live acceptance (manual, via `/paste`): the Chiming Staff returns a non-zero
estimate with a sensible interval and a "modelled" label, and shows the
"modelling … (~Ts)" notice during the wait; a common rare (boot) still prices via
the fast path with no notice and is unchanged.

## Non-goals (YAGNI)

- No pairwise/interaction terms in the model (additive main effects only; revisit
  only if additive proves too crude in practice).
- No change to the breakdown in this PR (tracked follow-up).
- No maintained mod-value table (value is learned live from the market).
- No change to the throttle, sessions, proxy, or currency conversion.
- No persistence/caching of fitted models (recomputed per estimate; the 60 s
  query cache already de-dups the underlying searches).
