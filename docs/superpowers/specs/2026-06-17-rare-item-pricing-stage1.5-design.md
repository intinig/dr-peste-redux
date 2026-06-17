# Rare-Item Pricing — Stage 1.5 Design & Plan

**Date:** 2026-06-17
**Status:** Approved — folds into the open PR #5 branch `feat/rare-pricing-stage1` (not merged until this lands)
**Builds on:** `2026-06-17-rare-item-pricing-stage1-design.md`

Stage 1 shipped on-demand rare pricing but with three limitations the operator judged unacceptable for *proper* pricing. Stage 1.5 fixes all three, plus the pricing-philosophy refinement below. Implementer prompts carry the exact code/tests; this doc locks the decisions, interfaces, and per-task acceptance.

## Decisions (locked)

1. **Live currency conversion (was: only div/ex/chaos).** Convert *every* currency using the poe.ninja economy data the bot already fetches. Confirmed feasible: the exchange overview's `lines[].id` is the trade2 currency code and `lines[].primaryValue` is already **divine-denominated** (Divine=1.0); a name/code join covers 49/49 currencies. No scraping change.
2. **Full explicit-mod filters (was: pseudo-only).** Map every parsed mod to its `trade2` stat id via the `data/stats` catalog and emit real stat filters — essential for accurate comparables.
3. **Impact-ranked breakdown (was: first-K).** Probe every characteristic, rank by *measured* delta, display the top-K.
4. **Band-based pricing that balances earnings vs. sale speed.** Don't price at the undercut floor. For each numeric stat, build a band with **your roll at the 20th percentile**: `min = round(0.9·v)`, `max = round(1.4·v)` (width `k=0.5`). Price off the band's price distribution and present the tradeoff:
   - **Quick sale** = 10th-percentile price · **Fair** (headline) = 25th · **Patient/max** = 75th.

All constants (`BAND_K=0.5`, `BAND_PCTL=0.2`, price percentiles 10/25/75, probe ceiling) are named consts, tunable.

## Architecture deltas (Stage 1 stays intact)

- **`poeninja`** gains a way to expose a `currency code → divine` rate table from the exchange data.
- A shared `Arc<RwLock<RateTable>>` (plain `HashMap<String,f64>`, defined in `trade/`) is updated by the refresher each cycle and read by `TradeClient::to_divine`. Keeps `trade/` decoupled from `poeninja` (it receives a map, not poeninja types).
- **`trade/stats.rs`** (new): the stat catalog + matcher. Fetched from `trade2/data/stats` at startup, with a small committed fixture for offline tests. `TradePricer` holds it (alongside `PseudoMap`).
- Query builder, estimate, and embeds change per Decisions 2–4.

## File structure

| File | Change |
|---|---|
| `src/poeninja/mod.rs` | add `NinjaClient::currency_rates(league) -> HashMap<String,f64>` (code→divine from exchange) |
| `src/trade/rates.rs` (new) | `RateTable` newtype over `HashMap<String,f64>` + `to_divine(amount, code)`; shared via `Arc<RwLock<…>>` |
| `src/trade/client.rs` | `TradeClient` holds the shared rate table; `parse_fetch` converts via it (drop only truly unknown codes) |
| `src/trade/stats.rs` (new) | `StatCatalog` (fetch + fixture) + `match_stat(line, group) -> Option<StatId>` via `#`-normalization |
| `src/trade/query.rs` | band filters (0.9×–1.4×) for all numeric filters; emit individual matched-mod filters + pseudos for fungible groups |
| `src/trade/ablation.rs` | percentile estimate (10/25/75); breakdown probes all (bounded) and ranks by measured delta |
| `src/trade/mod.rs` | `TradePricer` holds `StatCatalog`; uses it in query building |
| `src/discord/embeds.rs` | Quick / Fair / Patient framing |
| `src/main.rs` | build rate-table Arc, fetch stat catalog at startup, update rates in the refresher |
| `src/trade/fixtures/stats_sample.json` (new) | small representative `data/stats` subset for tests |

## Tasks (each: TDD, `cargo test <filter>`, commit; reviewed before next)

**T1 — Currency rate table (Decision 1).**
- `NinjaClient::currency_rates(&self, league) -> Result<HashMap<String,f64>>`: GET `economy/exchange/current/overview?type=Currency`, return `{line.id: line.primaryValue}`. Fixture test (reuse/extend `exchange_currency.json`).
- `src/trade/rates.rs`: `RateTable(HashMap<String,f64>)` with `to_divine(&self, amount: f64, code: &str) -> Option<f64>` (None if unknown). Unit test.
- `TradeClient`: replace the hardcoded `CurrencyRates` with `Arc<RwLock<RateTable>>`; `parse_fetch` uses it and drops a listing only when the code is unknown OR value ≤ 0. `new(poe_sessid, rates: Arc<RwLock<RateTable>>)`.
- Wire in `main.rs` + refresher: build the Arc, populate from `currency_rates` at startup and on each refresh cycle.
- **Accept:** a divine/exalted/chaos/aug listing all convert to correct divine; unknown code dropped. Live smoke still passes.

**T2 — Band filters + percentile estimate + Quick/Fair/Patient (Decision 4).**
- `query.rs`: every numeric `StatFilter` (pseudo or explicit) gets `min = round(0.9·v)`, `max = round(1.4·v)` via consts `BAND_K`/`BAND_PCTL`. Update query tests.
- `ablation.rs` `estimate_from`: `low = pctl(prices,10)`, `typical = pctl(prices,25)`, `high = pctl(prices,75)`; add a `percentile` helper + tests (incl. empty/1-element).
- `embeds.rs`: estimate embed shows **Quick sale** / **Fair** (headline) / **Patient**; update string-helper tests.
- **Accept:** estimate returns the three percentiles; embed labels them by sale-speed.

**T3 — Stat catalog + matcher (Decision 2, part 1).**
- `src/trade/stats.rs`: `StatCatalog` = parsed `data/stats` grouped by type. `StatCatalog::from_json(&str)` (used with the committed fixture in tests) and `StatCatalog::fetch(&TradeClient)` (live, at startup). `match_stat(&self, raw_line: &str, group: StatGroup) -> Option<String>` (stat id): normalize the line (numbers→`#`, trim sign/space) and look up in the group; `StatGroup` ∈ {Explicit, Implicit, Enchant, Rune, Pseudo}.
- Commit `src/trade/fixtures/stats_sample.json` with a handful of representative entries (spell damage, crit chance, +to spell skills, a resist, an "Adds # to #" mod).
- **Accept:** common single-number mods match their id; an unmatchable line returns None.

**T4 — Wire matcher into the query builder (Decision 2, part 2).**
- `query.rs` `build_baseline(item, pseudo, catalog, rates?, league)`: for each parsed mod, if it belongs to a fungible group keep using the pseudo aggregate; otherwise `match_stat` → add an individual banded stat filter (value from `first_number`, or the average for multi-number mods). Unmatched mods are logged + skipped. Keep the resistance-pseudo de-dup from Stage 1.
- **Accept:** a staff with "+#% increased Spell Damage" produces a real `explicit.*` stat filter (banded); resists still collapse to the total-res pseudo.

**T5 — Impact-ranked breakdown (Decision 3).**
- `ablation.rs` `breakdown`: probe **all** stat filters up to a `PROBE_CEILING` (e.g. 16), rank by measured delta, then truncate the *display* to `k`. Ablation probes use reduced relaxation (they widen, so rarely relax); the 60s cache dedups. Update the budget test: assert the probe count ≤ `1 + min(n, ceiling) + 1`.
- **Accept:** for a 6-stat item, ranked drivers reflect measured deltas (highest first); query count bounded and cached.

**T6 — Integration + final verification.**
- `TradePricer` holds `StatCatalog`; `main` fetches it at startup (graceful fallback to empty → pseudo-only if the fetch fails) and threads the rate Arc through the refresher.
- `cargo fmt`; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` green; `cargo test -- --ignored` (live smoke) passes; a fresh manual probe confirms a real rare prices with converted currency + matched mods and sane Quick/Fair/Patient numbers.
- Update PR #5 body to note Stage 1.5 landed and which limitations are now resolved.

## Out of scope (still deferred)

Mod-tier labeling, archetype classification, weapon DPS recompute (need a game-data DB); per-member sessions; the offline ML model and streamer/meta signal. Multi-number mod matching is best-effort in v1.5 (id matched; value approximated).
