# Currency Arbitrage Detector (`/arb`) — Design

**Date:** 2026-06-30
**Status:** Approved design, pre-implementation

## Goal

Help guild members find profitable **flipping** (single-market maker spread) and
**triangulation** (multi-hop cross-rate cycle) opportunities on the PoE2 Currency
Exchange. The bot **finds and ranks** opportunities for a human to execute
in-game. It never places orders.

### Hard boundary — ToS

Programmatically *placing* Currency Exchange orders is against GGG's Terms of
Service and is bannable. This feature is strictly a read-only detector that
surfaces ranked opportunities to a human. No part of the design executes trades,
and nothing here should ever grow that capability.

## Decisions (locked during brainstorming)

1. **Data source: hybrid.** cxapi (whole-market hourly digest) to *screen*,
   trade2 exchange (live order book) to *confirm*.
2. **Strategy: both flips and triangulation**, sharing one order-book dataset and
   one confirm/rank pipeline, but with two detectors because the profit mechanic
   differs (see "Maker vs taker" below).
3. **Surface: on-demand `/arb` command first.** Background alerter is a scoped-out
   later phase.
4. **Build approach (A): pluggable candidate source, trade2-first.** Ship a
   working `/arb` on existing infra now; slot cxapi screening in behind the same
   interface once GGG approves the OAuth app. End state is the hybrid above.

### Why a pluggable candidate source

cxapi requires a **GGG-approved OAuth confidential client** with the
`service:cxapi` scope. Approval is on GGG's timeline, not ours. Approach A treats
the cxapi feed as a pluggable upgrade to the *screening* stage rather than a hard
dependency: the cycle engine, the live-confirm step, and the `/arb` command are
written once against a `CandidateSource` trait and never change when the source
swaps from watchlist to cxapi.

## Maker vs taker (the load-bearing refinement)

A flip is the graph-level "2-cycle" A→B→A, but the *profit mechanic* differs from
triangulation, and the design respects that:

- **Triangulation** — you **take** liquidity on every leg (immediate fills).
  Profit when the taker ratios around a cycle compound to > 1. A 2-cycle on taker
  rates is *always* a small loss (you cross the spread twice), so triangulation is
  inherently length ≥ 3.
- **Flip** — you **make** liquidity: place a resting buy *and* a resting sell on
  one market and earn the bid/ask spread, conditional on both filling.

"Unified" therefore means **one shared order-book dataset and one shared
confirm/rank pipeline** feeding two small detectors (`graph.rs` for taker-rate
cycles, `spread.rs` for maker spreads) — not forcing flips through cycle search.

## Architecture

New isolated module `src/arb/`, mirroring the existing
`poeninja → store → discord` decoupling. The engine never touches Discord; the
command never calls APIs directly. Data flows one direction:

```
CandidateSource  →  graph / spread  →  confirm  →  rank  →  /arb embed
   (edges)          (candidate opps)   (live)     (Live opps)
```

### Module layout

```
src/arb/
  mod.rs       ArbEngine — orchestrates source → detectors → confirm → rank; only public surface
  source.rs    CandidateSource trait + WatchlistSource (trade2, phase 1) [+ CxapiSource phase 2]
  graph.rs     directed rate graph + bounded cycle enumeration (triangulation)
  spread.rs    per-market maker-spread scan (flips)
  confirm.rs   live trade2 re-quote of aggregated legs in surviving candidates
  model.rs     Edge, RatioQuote { ratio, stock, freshness }, Opportunity, Leg
src/discord/arb.rs   the /arb command + embed
```

### Core types (`model.rs`)

- `Freshness { Live, Aggregated }` — `Live` from trade2 order book, `Aggregated`
  from a cxapi hourly digest.
- `RatioQuote { in_amount: u32, out_amount: u32, stock: u64, freshness }` — an
  achievable integer-ratio book entry. Ratios are kept as integer in/out pairs,
  never floats, to match the exchange's discrete-ratio mechanics.
- `Edge { from: CurrencyCode, to: CurrencyCode, quote: RatioQuote }`.
- `Opportunity` — enum `Triangulation { legs: Vec<Leg>, multiplier, feasible_volume, value_div }`
  or `Flip { market, spread_pct, volume, value_div }`, plus a `confidence: Freshness`.
- `Leg` — one hop of a cycle (`from`, `to`, the quote used).

### `CandidateSource` trait (`source.rs`)

```
trait CandidateSource {
    async fn edges(&self, league: &str) -> Result<Vec<Edge>>;
}
```

- **`WatchlistSource` (phase 1)** — for each ordered pair in `ARB_WATCHLIST`,
  live-queries the trade2 exchange order book; emits `Live` edges with real
  top-of-book ratio + stock. Bounded to the watchlist to respect rate limits.
- **`CxapiSource` (phase 2)** — reads the cached whole-market snapshot (refreshed
  hourly from `/currency-exchange/poe2`); emits `Aggregated` edges. cxapi gives
  volume + low/high ratio per market, not a true book, so the screen uses a
  conservative ratio (the worse end of the range) to avoid false positives; these
  edges *must* be confirmed live before surfacing.

## Detection

### Triangulation (`graph.rs`)

Directed edge A→B weight = `-log(taker_ratio)`; a profitable cycle is a
negative-weight cycle (ratios compound to > 1).

Algorithm: **enumerate simple cycles up to length `L`** (`ARB_MAX_CYCLE_LEN`,
default 4) over the candidate edge set, rather than Bellman-Ford's existence-only
answer. The market is small and the edge set sparse, so bounded enumeration is
cheap and yields every profitable cycle *with its multiplier* — required for
ranking. Cycles are deduped by rotation; length is capped to bound fill risk.

### Flips (`spread.rs`)

Per market {A,B}: best bid ratio vs best ask ratio → `spread_pct = (ask - bid) / bid`.
Ranked by `spread_pct × volume`, where volume is the fill-likelihood proxy (the
spread is only earned if both resting orders fill). Surfaced as passive/spread so
it is not mistaken for an instant cycle.

### Profit math (integer-ratio and stock aware)

This is what prevents fake profits:

- **Multiplier** is computed from each leg's achievable integer in/out, never a
  rounded float.
- **Feasible volume = bottleneck leg.** Walk the cycle and find the largest input
  such that every leg's required quantity ≤ its available `stock`. A +8% cycle
  that can only clear 2 exalted is noise.
- **Absolute profit** = `feasible_input × (multiplier - 1)`, valued in divine for
  ranking.
- PoE2's exchange has no percentage fee, but discreteness and minimum order sizes
  mean tiny-volume candidates are filtered by `ARB_MIN_VOLUME`.

### Confirm + thresholds (`confirm.rs`)

- Only the top `ARB_CONFIRM_TOP_N` (default ~8) candidates are confirmed, and only
  legs flagged `Aggregated` trigger a trade2 query — this hard-bounds the rate
  budget regardless of how many candidates the screen produces.
- Re-query live top-of-book + stock, recompute the multiplier/spread, and **drop**
  anything that no longer clears `ARB_MIN_PROFIT_PCT` / `ARB_MIN_VOLUME`.
- In phase 1 every edge is already `Live`, so confirm is a no-op.
- Surface only confirmed-`Live` opportunities. If nothing clears the bar,
  **abstain honestly** ("nothing above 3% / N volume right now") rather than
  padding the list.

Ranking output: a ranked list, each item carrying type (Triangulation | Flip),
legs/market, multiplier or spread %, feasible volume, value-in-divine, and
`confidence: Live`.

## Rate limiting

Reuse the existing `trade::limiter::RateLimiter`. Add an `Exchange` variant to the
`Endpoint` enum for the trade2 currency-exchange endpoint so its rate rules are
tracked independently of item search/fetch. The confirm budget (`ARB_CONFIRM_TOP_N`
× legs) is the only unbounded-looking cost and is explicitly capped.

## Error handling

Follows the project rule that background work never panics; `anyhow` + `tracing`
throughout. `/arb` degrades instead of failing:

- cxapi snapshot missing/stale (phase 2) → fall back to `WatchlistSource`, note
  reduced coverage in the embed.
- A trade2 confirm query fails or rate-limits on a leg → drop that opportunity
  (never surface an unconfirmed cycle), keep the rest.
- Confirm budget exhausted mid-pass → surface what was confirmed and **explicitly
  label the truncation** (no silent caps).
- Discord: `/arb` defers the response immediately (confirm takes seconds), then
  edits in the embed.

## Testing (offline by default)

- `graph.rs` cycle search + profit math: pure functions, unit-tested on synthetic
  edge sets — a known triangular arb, the property that a 2-cycle on taker rates
  is never profitable, integer-ratio rounding, and the stock-bottleneck calc.
- `spread.rs`: unit tests on hand-built books.
- cxapi + trade2-exchange parsing: committed JSON fixtures (established pattern),
  each with one `#[ignore]`d live smoke test.
- Abstention path: empty result → asserts the honest "nothing clears the bar"
  message.

## Configuration (additive; secrets stay out of git)

Phase 1 (no new secrets):

- `ARB_WATCHLIST` — comma-separated currency codes; sensible default set.
- `ARB_MIN_PROFIT_PCT` — minimum cycle profit to surface (default ≈ 3%).
- `ARB_MIN_VOLUME` — minimum feasible volume / market volume.
- `ARB_MAX_CYCLE_LEN` — max triangulation cycle length (default 4).
- `ARB_CONFIRM_TOP_N` — candidates to live-confirm (default ≈ 8).

Phase 2:

- `POE_OAUTH_CLIENT_ID` — GGG OAuth client id.
- `POE_OAUTH_CLIENT_SECRET` — **secret**; OAuth client secret.
- `ARB_SCREEN_INTERVAL_MINS` — cxapi screen refresh interval.

All documented in `.env.example` with placeholder values. Defaults are starting
points to tune against real data.

## Phasing (Approach A made concrete)

- **Phase 1 — ships now, no OAuth.** `CandidateSource` trait + `WatchlistSource`,
  the `graph`/`spread`/`confirm` engine (confirm a no-op since all-live), `/arb`
  command + embed, full offline test suite. Bounded to the watchlist.
- **Phase 2 — when GGG approves the OAuth app.** `CxapiSource` behind the same
  trait + background hourly refresher storing a whole-market snapshot in the store;
  the confirm stage activates for aggregated legs. Engine and command unchanged.
- **Phase 3 — later, scoped out here.** Background alerter that reuses the engine
  to post threshold-beating opportunities to a configured channel, with de-dup /
  cooldown noise control.

## Open questions to resolve during planning

- Exact trade2 currency-exchange endpoint path/shape for PoE2 (`/api/trade2/exchange/{league}`)
  and its rate-rule headers — capture a fixture before wiring `WatchlistSource`.
- Default `ARB_WATCHLIST` membership (which currencies are liquid enough to be
  worth polling).
- Whether `value_div` ranking should reuse the existing poe.ninja `currency_rates`
  map for the divine valuation (likely yes).
