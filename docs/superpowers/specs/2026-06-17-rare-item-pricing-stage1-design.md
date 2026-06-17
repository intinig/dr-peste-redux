# Rare-Item Pricing — Stage 1 Design

**Date:** 2026-06-17
**Status:** Approved (brainstorming) — ready for implementation planning
**Supersedes the original non-goal:** the initial bot design (`2026-06-16-...`) explicitly
did *not* price rares. This spec adds that capability as the first stage of a larger ambition.

---

## 1. Context: why this is Stage 1 of several

The long-term goal is an ML model that learns how item **characteristics** (and, later, meta/streamer
demand) drive the price of procedurally-generated **rare** PoE2 items — items poe.ninja does not price.

That ambition is really three subsystems: a **data foundation**, an **ML pipeline**, and a
**meta-demand signal**. It cannot be a single spec. This document is **Stage 1: the data foundation**,
delivered as a *useful product on its own* — on-demand rare price-checking — that simultaneously
accumulates the only dataset we are permitted to collect.

### 1.1 Feasibility findings that shaped this (load-bearing)

- **No public stash-tab API exists for PoE2.** In PoE1, poe.ninja and every pricing tool are fed by
  the *public stash river* (a bulk stream of all listed items). For PoE2 that endpoint does not exist;
  GGG's developer docs mark Public Stashes "PoE1 only." There is **no bulk market feed** to train on.
- **The official PoE2 developer API exposes nothing useful here.** It is OAuth-gated to the
  authenticated user's *own* account plus league/build-planner data and a *currency* exchange — **zero
  visibility into other players' rare listings.**
- **GGG's ToS prohibits "data gathering and extraction tools"** and requires respecting API call
  limits and not circumventing them. The only programmatic window into rare listings is the website's
  `trade2` search/fetch API (rate-limited, asking-prices-only).

**Conclusion:** a self-crawled longitudinal corpus is *not ToS-compliant and not operationally safe.*
The ML ambition is therefore forced into **small-data** territory, fed by an **on-demand flywheel**:
every user-initiated price-check legitimately fetches comparables, and we log them. The dataset accrues
from real usage, never from crawling.

Sources:
- Developer docs (endpoint coverage, OAuth, rate-limit header scheme): <https://www.pathofexile.com/developer/docs>
- ToS / data-extraction prohibition: <https://www.pathofexile.com/legal/terms-of-use-and-privacy-policy>
- No PoE2 public stash API (community indexer): <https://github.com/maximumstock/poe-stash-indexer>

---

## 2. Goal & success criteria

**Goal:** Given a pasted rare/magic item, return a market **price estimate + confidence**, and on
demand a **breakdown of which characteristics drive the price**, using a *bounded* number of
rate-limit-respecting `trade2` queries — and **log every probe** for the eventual model.

**Success criteria:**
1. `/paste` of a rare returns a price range + confidence in one query, instead of today's
   "not tracked" dead-end.
2. A "Break it down" action returns a ranked list of characteristic contributions plus a synergy flag,
   in a bounded query budget (~6 queries).
3. Every search/fetch we run is appended to a probe log suitable as future training data.
4. No member personal data is stored; the Privacy Policy needs no change.
5. All trade traffic respects `X-Rate-Limit-*` headers and is aggressively cached.

---

## 3. Scope

**In scope (v1):**
- Clipboard-driven full-item parsing (no external game-data DB).
- Pseudo-mod-aware `trade2` query builder.
- Lazy two-phase **ablation pricing** (instant estimate; on-demand breakdown).
- **Anonymous trade reads by default**, with an **optional** operator session (swappable
  `SessionProvider`, per-member-ready) to raise the rate-limit ceiling.
- Append-only probe log (the flywheel corpus).

**Deferred (explicitly YAGNI for v1):**
- Mod-**tier labeling**, **archetype classification**, and **DPS recomputation** — all need a
  maintained PoE2 game-data DB.
- **Per-member sessions** (Provider "B") — the abstraction exists; the encrypted store + privacy
  update + revocation UX do not.
- **Residential proxies** — unnecessary at guild volume; would only matter if a per-IP limit binds.
- The **offline ML model** itself and the **meta/streamer demand** signal — later stages.

---

## 4. Core concept: ablation pricing + flywheel

Rare value is **non-additive** (e.g. crit is worthless alone but a premium on a strong spellcaster
staff). We do not need a global model to capture this up front, because we price each item **against
the live market in its own context**:

- **Baseline:** query the item's meaningful characteristics → cheapest live comparables → "this item ≈ X".
- **Ablation:** re-run dropping/relaxing one characteristic at a time → the **price delta is that
  characteristic's marginal value *given the rest of this item*.** Context-conditional by construction:
  crit's delta comes out large on a strong staff, small on a junk one — for free.
- **Synergy:** a single pairwise probe on the top two characteristics (drop-both vs. drop-each) flags
  super-additive interaction when `value(drop both) ≠ value(drop A) + value(drop B)`.
- **Flywheel:** every probe is a labeled datapoint `(characteristic-set → market price, timestamp)`.
  Accumulated across usage, an offline regression later recovers **global** weights — bootstrapped with
  zero crawling.

Full interaction mapping is 2ⁿ and thus capped: we ablate only the **top-K** characteristics and do
**one** pairwise probe.

---

## 5. Item model — the characteristic taxonomy

The pricing unit is the **whole item**, not just explicit affixes. The PoE2 clipboard text already
carries nearly all of this (sections split by `--------`), so v1 needs **no game-data DB**.

| Characteristic | Price role | `trade2` filter | Ablatable? |
|---|---|---|---|
| Base type | Archetype, base stats, implicit, reqs | type/category | broaden only (→ any base in class) |
| Item level | Gates affix tiers; crafting-base value | ilvl range | relax |
| Quality | Boosts local stats | quality range | relax |
| Implicits (incl. corrupted/altered) | Innate mod, sometimes the whole value | implicit stat filters | drop |
| Runes / sockets | Socketed runes grant mods; socket count | stat + socket filters | drop rune mod / relax sockets |
| Enchants | Added mods, can be premium | enchant stat filters | drop |
| Corruption | Can't-modify status; premium or discount | corrupted flag | toggle |
| Explicit affixes | The random rolls | stat filters | drop / relax |
| Derived / pseudo (total res, total life, DPS, crit, APS) | **What buyers actually filter on** | pseudo/total filters | relax |

**Pseudo-mods are a primary search axis, not a footnote.** The market searches "+53% total Elemental
Resistance," not three separate resist lines; the individual lines are usually fungible. Pseudo-mods
also **sum across sources** (implicit + rune + affix all feed total ele res), which *simplifies*
valuation — we price the aggregate and ignore the source.

---

## 6. Architecture & module layout

Follows the existing decoupled flow (`poeninja → store → discord`, never sideways). A new isolated
`trade/` module is the **only** thing Discord calls for rares; Discord never performs HTTP itself.

```
src/
  itemtext.rs       EXPAND: full clipboard parse → rich ParsedItem
  trade/
    mod.rs          public API: price_item(), breakdown_item()  ← the boundary
    client.rs       trade2 search + fetch; rate-limit header handling; query cache
    session.rs      SessionProvider trait; OperatorSession (v1); per-member later (B)
    query.rs        ParsedItem → trade2 query JSON (stat / pseudo / misc filters)
    pseudo.rs       stat → pseudo mapping + summation
    ablation.rs     baseline + ablation/pairwise probes → characteristic contributions
    model.rs        TradeQuery, Listing, PriceEstimate, Breakdown, Probe
  pricelog.rs       append-only probe corpus (the flywheel)
  store.rs          route(): Rare/Magic → trade path (was MatchOutcome::NotTracked)
  discord/
    paste.rs        rare → price estimate; "Break it down" interaction
    embeds.rs       estimate embed + breakdown embed
```

**Why a new module rather than extending `poeninja`:** trade pricing is a *live, per-request* call,
architecturally unlike the background-refreshed snapshot. Keeping it in `trade/` preserves the
"discord reads the store only" rule by introducing one new service boundary
(`trade::price_item` / `trade::breakdown_item`).

### 6.1 Key types (`trade/model.rs`)

- `TradeQuery` — league + the filter set (status, type/category, stat filters, pseudo filters, misc:
  ilvl/quality/sockets/corrupted). Serializable to the `trade2` search JSON.
- `Listing` — one fetched result: price (amount + currency), normalized to a common unit via the
  existing `core.rates`-style conversion, plus the listing's own characteristics.
- `PriceEstimate` — `{ low, typical, high, currency, listing_count, confidence }`.
  `confidence` derives from listing count and price spread (e.g. IQR / median).
- `Contribution` — `{ characteristic, kind (drop|relax), delta, basis }`.
- `Breakdown` — `{ ranked: Vec<Contribution>, synergy: Option<SynergyNote>, trade_url }`.
- `Probe` — `{ query, listings, estimate, timestamp }` — the logged datapoint.

### 6.2 `SessionProvider` (auth abstraction — A now, B-ready)

```
trait SessionProvider {
    fn session_for(&self, ctx: &RequestCtx) -> Result<Session>;  // POESESSID + User-Agent
}
```
- v1 default: `AnonymousSession` — no cookie, just a compliant `User-Agent`. Works, but limited.
- v1 optional: `OperatorSession` — if `POE_SESSID` is set in env, attach it to raise the limit;
  otherwise fall back to anonymous. Ignores `ctx`.
- later (B): `PerMemberSession` — resolves the invoking member's session from an encrypted store.
The client depends only on the trait, so B drops in without touching `client.rs`.

---

## 7. Data flow

### 7.1 Price (instant, Phase 1)
1. `/paste` → `itemtext::parse` → rich `ParsedItem`.
2. `store::route`: Unique/Currency → existing snapshot path; **Rare/Magic → `trade::price_item`**.
3. `price_item(parsed)`:
   a. `query::build_baseline(parsed)` → `TradeQuery` (pseudo-preferred for fungible groups).
   b. `client::search(query)` → result hashes + total (respect rate-limit headers; cache by query).
   c. `client::fetch(hashes[..N])` → `Vec<Listing>` (batch ≤ fetch limit).
   d. **relax-until-≥k** fallback: if fewer than `k` listings, progressively relax the loosest
      numeric filters and re-search (bounded retries) so thin markets still yield an estimate.
   e. compute `PriceEstimate` (low-percentile + range; confidence from count & spread).
   f. `pricelog::append(Probe{..})`.
   g. return estimate + a cached query context (handle for breakdown).
4. Discord renders the estimate embed + a **"Break it down"** button.

### 7.2 Breakdown (on demand, Phase 2)
5. Button → `trade::breakdown_item(parsed, ctx)`:
   a. choose the **ablation basis** (pseudo aggregate for fungible groups; individual stat only when
      drilling) to avoid double-counting.
   b. for the **top-K** characteristics (ranked by a **clipboard-only** heuristic — build-enabling
      stats like "+# to all spell skills", large pseudo aggregates such as total resistance/life, and
      rarer stat types — since tier labels are deferred): one single-drop/relax probe each → `delta`.
   c. **one pairwise probe** on the top two → synergy flag.
   d. each probe logged via `pricelog::append`.
   e. return `Breakdown` (ranked contributions + synergy note + trade URL).
6. Discord renders the breakdown embed.

**Query budget:** Phase 1 = 1 search (+1 fetch); Phase 2 ≈ K single-drops + 1 pairwise ≈ 6 searches.
Both bounded; both cached.

---

## 8. Trade client: endpoints, rate limits, caching

> Exact request/response shapes are confirmed against the live API by the `#[ignore]`d smoke test
> during implementation; the structure below is the working assumption.

- **Search:** `POST /api/trade2/search/{league}` with the query JSON → `{ id, result: [hashes], total }`.
- **Fetch:** `GET /api/trade2/fetch/{comma-separated hashes}?query={id}` in batches up to the fetch
  limit → listing details.
- **Stats list:** `GET /api/trade2/data/stats` → canonical stat ids incl. the `pseudo` group
  (used to seed §9's mapping).
- **League:** reuse the existing active-league detection (already derived for poe.ninja) for the
  `{league}` path segment.

**Rate limits:** parse `X-Rate-Limit-Policy` / `X-Rate-Limit-Rules` / per-rule `X-Rate-Limit-{rule}`
headers (`max:period:restriction` triples); enforce a token-bucket/backoff that respects the
*observed* limits (they are only known from responses). Never tight-loop. Honor the existing
"be polite to upstream" rule.

**Anonymous vs. authenticated:** anonymous reads work but carry **stricter limits**, so frugality is
not optional — it is what makes anonymous viable. The lazy two-phase flow, relax-until-≥k, and the
query cache are the levers. The **breakdown budget adapts to remaining headroom** (from the rate-limit
headers): under pressure, reduce `K` and skip the pairwise probe rather than getting throttled. An
optional operator `POE_SESSID` simply raises the ceiling without changing any of this.

**Caching:** in-memory TTL cache keyed by the serialized query (Cloudflare also caches `trade2`).
Identical baseline/ablation queries within the TTL hit the cache, sharply cutting real requests.

---

## 9. Data artifacts (the one new maintained dependency)

A committed **stat → pseudo mapping** (consumed by `pseudo.rs`): which raw stat lines sum into which
pseudo (e.g. "+#% to Fire and Lightning Resistance" feeds fire res, lightning res, and total ele res),
plus the summation rules. Seeded from `trade2/data/stats` + curated rules. This is lighter than a
mod-tier DB but **is** a maintained artifact (re-check each major patch). Stored as committed JSON and
loaded at startup.

No other data files in v1. (The game-data DB for tiers/archetype/DPS is deferred to a later stage.)

---

## 10. Error handling

`anyhow` + `tracing` throughout, matching the project convention; **never panic the process**.
Each failure mode degrades to a clear, ephemeral Discord reply:
- **Rate-limited:** back off per headers; if still blocked, reply "trade is busy, try again shortly."
- **Session expired/invalid:** log for the operator; reply "price lookup is temporarily unavailable."
- **Network/HTTP error:** reply "couldn't reach trade right now."
- **Zero listings after relaxation:** reply "no comparable listings found for this item."
A breakdown probe that fails is dropped from the ranking rather than failing the whole breakdown.

---

## 11. Privacy & ToS posture

- **v1 stores no member personal data.** Anonymous by default, optional operator session → the
  existing Privacy Policy ("no database, stores no personal data") **remains accurate**. The probe log holds item
  characteristics + prices + timestamps (market data), **not** Discord user IDs.
- Keep the "not affiliated with or endorsed by Grinding Gear Games" statement in the User-Agent and in
  user-facing output, per GGG's API policy.
- Stay within published rate limits; do not pool sessions or rotate IPs to exceed them.
- If/when Provider B (per-member sessions) is implemented, it requires a **Privacy Policy update**,
  encryption at rest, and revocation — tracked as a later stage, out of scope here.

---

## 12. Testing strategy

Offline-by-default, per project convention (network tests `#[ignore]`d).

- **Parser (`itemtext`):** fixture tests on real pasted items — a spellcaster staff, a resist ring, a
  martial weapon, a corrupted item, an item with runes, an item with an enchant — asserting every
  parsed characteristic (base, ilvl, quality, implicits, runes/sockets, enchants, corruption,
  explicit affixes).
- **Query builder (`query`):** `ParsedItem` → expected `trade2` query JSON (golden fixtures),
  including pseudo-preferred filter selection.
- **Pseudo resolver (`pseudo`):** stat lines → expected pseudo totals (incl. cross-source summation).
- **Ablation (`ablation`):** a **mocked client** returns canned listings per query; assert deltas,
  ranking, relax-until-≥k behavior, and pairwise synergy detection.
- **Client (`client`):** unit-test `X-Rate-Limit-*` header parsing and backoff math.
- **Live smoke test:** one `#[ignore]`d test hitting real `trade2` (search → fetch), mirroring the
  existing live poe.ninja test, used to confirm §8's request/response shapes.

---

## 13. Later stages (recorded, out of scope)

1. **Provider B — per-member sessions:** encrypted secret store, opt-in flow, expiry/refresh,
   revocation, Privacy Policy update.
2. **Game-data DB:** mod-tier labeling, per-base implicits, archetype classification, DPS
   recomputation — unlocks richer features and global tier-aware pricing.
3. **Offline pricing model:** train an interaction-aware, archetype-keyed regressor (gradient-boosted
   trees / factorization machines — never plain linear) over the accumulated probe corpus.
4. **Meta / streamer demand signal:** a time-varying demand index per archetype from public signals
   (Twitch/poe.ninja build stats/ladder), joined to the probe time-series.

---

## 14. Assumptions & open questions

- `trade2` search/fetch request and response shapes are taken as the §8 working assumption and
  **confirmed by the live smoke test** during implementation.
- Anonymous `trade2` reads work but are **rate-limited more tightly** than authenticated ones. v1
  therefore runs anonymously by default and treats an operator `POE_SESSID` as an *optional* secret
  that raises the ceiling. The exact anonymous limits are observed from the rate-limit headers at
  runtime and drive the adaptive breakdown budget (§8).
- `k` (min listings), `N` (fetch batch), `K` (top characteristics to ablate), and cache TTL are tunable
  constants chosen during implementation and surfaced as config where sensible.
