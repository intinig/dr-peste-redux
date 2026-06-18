# Rare-Item Pricing Accuracy: Craftability-Tier Comparables — Design

**Date:** 2026-06-18
**Status:** Approved in brainstorming; pending spec review before planning.
**Builds on:** the rare-item pricing engine (`src/trade/`) shipped in Stage 1 and
the per-member-session work (PR #10).

## 1. Problem

Rare-item estimates are far too low. Worked example (live, `Runes of Aldur`): a
clean **Sandsworn Sandals** (ES base) with very high resistances, 35% movement
speed, 16% increased Rarity, and **two open prefixes** prices at Quick `0.03` /
Fair `0.04` / Patient `0.05` div. Its real value is **≈2 div** — a ~40× miss.
The `/paste` breakdown also shows **~0.0 div for every affix**, which is the tell.

Two compounding causes (confirmed in code):

1. **Floor sampling.** The search sorts `price: asc` and the engine keeps only
   the **10 cheapest** listings (`LISTING_LIMIT = 10`), then reports p10/p25/p75
   of *those 10*. Every number therefore lives inside the bottom ~10 asks — the
   undercut/dump floor. (`src/trade/query.rs` `to_payload` sort; `src/trade/mod.rs`
   `LISTING_LIMIT`; `src/trade/ablation.rs` `estimate_from`.)
2. **No craftability awareness.** The engine prices by **affix content only**: it
   finds Sandsworn Sandals carrying res+MS+rarity and reports the cheapest. It
   cannot tell our *craftable, 2-open-prefix base* from a **bad-filled** boot that
   has the same suffixes plus two junk prefixes. The cheap floor is exactly those
   bad-filled boots — a different, inferior product that should not anchor ours.
   (`ParsedItem.explicits` is `Vec<ItemStat{raw,value}>` — no prefix/suffix or
   open-slot information at all.)

The ~0.0 breakdown deltas follow from (1): dropping a stat re-samples the same
floor, which barely moves.

## 2. Value model (the rationale, from the domain expert)

For a fixed base + suffix set, filled-affix **quality** orders value:

> **2 bad-filled  <  2 open  <  2 good-filled**

Open prefixes carry **no inherent premium** — they are *potential*, valued between
a bricked item and a finished one. Our boot is the **middle ("open") tier**.

Correct pricing therefore = **compare like-for-like by craftability state**. Pricing
a 2-open-prefix base means comparing it to *other 2-open-prefix bases* with the same
base + suffixes — which excludes both the cheaper bad-filled boots below it and the
pricier finished boots above it, landing the honest "open" value. We never add a
premium; we remove non-comparable products from the sample.

## 3. Non-goals (YAGNI)

- No premium/multiplier for open slots (see §2 — value is purely from comparables).
- No modeling of *which* craft is possible or its expected value.
- No per-listing prefix/suffix classification of **comparables** (we use affix
  *count* as the craftability proxy — see §4.3 and §9).
- No change to currency conversion, the trade2 client/session/proxy, or `/farm`.

## 4. Design

Mechanism in one line: **parse our item's affix structure → fetch a wider sample →
keep only comparables in the same craftability tier (affix count) → estimate over
that filtered set with honest, outlier-trimmed percentiles.**

### 4.1 Parse craftability (our item) — `src/itemtext.rs`

PoE2 **Advanced Item Description** clipboard annotates each explicit with its
generation type, e.g.:

```
{ Prefix Modifier "Glittering" (Tier: 3) — Defences }
12% increased Energy Shield
{ Suffix Modifier "of the Walrus" (Tier: 2) — Cold }
+41% to Cold Resistance
```

- Add `Affix { Prefix, Suffix }` and an `affix: Option<Affix>` field to `ItemStat`
  (only meaningful for explicits; `None` for implicit/enchant/rune and for
  basic-clipboard pastes that lack annotations).
- The parser already reads these annotation blocks for rolls (PR #7); extend it to
  record the prefix/suffix tag on the following stat line.
- Add a derived helper on `ParsedItem`:
  ```rust
  pub struct Craftability { pub filled_prefixes: u8, pub filled_suffixes: u8,
                            pub open_prefixes: u8, pub open_suffixes: u8,
                            pub explicit_count: u8 /* filled prefixes+suffixes */ }
  pub fn craftability(&self) -> Option<Craftability>; // None when affix tags absent
  ```
  Rares cap at 3 prefixes + 3 suffixes; `open_* = 3 - filled_*` (saturating at 0).
  Items with extra-affix mechanics still list every mod as an explicit line, so the
  filled counts stay correct and `open_*` floors at 0.
- **Basic-clipboard fallback:** if `affix` tags are absent, `craftability()` returns
  `None` → Part 2 filtering is skipped and the engine behaves as Part-1-only (§4.4),
  labelled accordingly.

### 4.2 Wider sample + comparable affix count — `src/trade/{model.rs,client.rs,mod.rs}`

- Raise the priced sample: introduce `COMPARABLE_SAMPLE = 30` (replaces the literal
  10 in the price path). The search still sorts `price: asc`; we fetch the cheapest
  `COMPARABLE_SAMPLE`.
- Add `explicit_count: usize` to `model::Listing`. Extend `TradeClient::parse_fetch`
  to read each result's explicit mods from the trade2 `fetch` response
  (`result[].item.explicitMods` — verify the exact field against the existing
  parse_fetch fixture and a live response during implementation) and store the
  count. Listings whose mods can't be read default to `explicit_count = 0` and are
  treated as "unknown" (kept, never used to *raise* the floor).

### 4.3 Craftability-tier filter (Part 2) — `src/trade/ablation.rs`

Given our item's `Craftability` and the fetched listings:

- Keep listings whose **`explicit_count <= our filled explicit_count`** — i.e. no
  meaningful extra explicit mods beyond the suffixes the search already pinned
  present. Because the search guarantees our suffixes are present, "same count, no
  extras" = same open-slot state = same craftability tier. This drops the bad-filled
  boots (more mods, cheaper, worse) **and** the finished boots (more mods, pricier,
  better).
- This is a pure function over `(listings, our_explicit_count)` → unit-testable.

### 4.4 Honest estimate over the filtered set (Part 1) — `src/trade/ablation.rs`

Replace the cheapest-10 percentile with: filter (§4.3) → trim bottom outliers →
percentiles over the survivors.

- `TRIM_BOTTOM_FRAC = 0.10`: when ≥ `TRIM_MIN_N (=8)` survivors, drop the cheapest
  10% (≥1) as dump/troll outliers; below that, no trim (too few to spare).
- Report: **Quick = p20, Fair = p50 (median), Patient = p80** of the trimmed set
  (was p10/p25/p75 of the unfiltered cheapest-10). Fair becomes the median going
  rate of comparable open-tier bases.
- **Confidence** scales with the *filtered* survivor count via the existing
  `Confidence::from_count`.
- **Fallback ladder:**
  - filtered survivors ≥ 1 → price off them (this is the normal path; small N just
    lowers confidence). Do **not** fall back to the broad pool merely for small N —
    that would re-introduce the bug.
  - filtered survivors == 0 (no comparable bases listed) → fall back to the
    unfiltered sample with the same percentile logic, set confidence low, and label
    the estimate "broad-market (no comparable open-base listings)".
  - `craftability()` == None (basic clipboard) → Part-1-only: percentiles over the
    unfiltered (but wider + trimmed) sample, labelled "affixes present; craftability
    not detected — paste in Advanced Mode for a sharper estimate".

### 4.5 Breakdown on the filtered set — `src/trade/ablation.rs`

`breakdown` already calls `estimate`; once `estimate` filters by craftability, the
ablation deltas are measured within the open-tier segment and become meaningful. No
separate change beyond threading the item's `explicit_count` into the ablation
queries (each single-drop estimate filters the same way). Ablation reuses the
baseline via the existing 60s cache; per-drop queries fetch `COMPARABLE_SAMPLE`.

### 4.6 Surface the tier in the embed — `src/discord/embeds.rs`

Add one line to the estimate embed footer/description so the user sees *what was
priced*, e.g.: `clean base · 2 open prefixes · 7 comparable listings` (or, on
fallback, `broad-market estimate — no comparable open-base listings`). Keeps the
number honest and debuggable.

## 5. Configuration & constants

All tunable, defined as consts (no env needed initially):

| Const | Default | Meaning |
|---|---|---|
| `COMPARABLE_SAMPLE` | 30 | cheapest listings fetched per query for pricing |
| `TRIM_BOTTOM_FRAC` | 0.10 | fraction of cheapest survivors dropped as outliers |
| `TRIM_MIN_N` | 8 | only trim when at least this many survivors |
| percentiles | p20 / p50 / p80 | Quick / Fair / Patient over the trimmed set |

(If tuning against real items shows these need to be operator-adjustable, promote to
env later — not now.)

## 6. Rate-limit / cost

`COMPARABLE_SAMPLE = 30` ≈ 3 `fetch` calls/query (10 hashes/call) vs 1 today. A
`/paste` breakdown runs baseline + up to `PROBE_CEILING` single-drops + 1 pairwise;
the 60s query cache dedupes the shared baseline. Net is a few× more `fetch` calls per
breakdown — affordable now that each member uses their own POESESSID + sticky
residential IP, and bounded by `PROBE_CEILING`. Search/fetch keep the existing 429
`Retry-After` backoff; never retry through a 429.

## 7. Calibration & testing

**Regression targets (the point of the feature):**
- The reference boot (clean Sandsworn Sandals, 2 open prefixes, high res + 35% MS +
  16% rarity) estimates **≈2 div** (Fair within a sane band, e.g. 1–3 div), not 0.05.
- A **bad-filled** version of the same boot (same suffixes + 2 junk prefixes →
  higher `explicit_count`) is excluded from the clean-base sample and **stays cheap**.

**Offline unit tests (pure logic — no network):**
- Parser: prefix/suffix tagging from Advanced-Mode fixtures; `craftability()` counts
  (incl. all-suffix item → open_prefixes computed; basic clipboard → `None`).
- `parse_fetch`: `explicit_count` populated from a fetch fixture (extend the existing
  parse_fetch test fixture with `explicitMods`).
- Craftability filter: keeps `explicit_count <= ours`, drops more-filled listings.
- Estimate: trim + p20/p50/p80 over a known set; fallback ladder (≥1 / ==0 / `None`).
- A synthetic end-to-end test via the mock `Comparables`: a mixed set (bad-filled
  cheap + open-tier + finished) yields the open-tier median, not the floor.

**Live smoke (`#[ignore]`):** price the reference boot through a real session/proxy
and assert it lands in the 1–3 div band.

## 8. Files touched

| File | Change |
|---|---|
| `src/itemtext.rs` | `Affix` enum, `ItemStat.affix`, parse prefix/suffix annotations, `ParsedItem::craftability()` |
| `src/trade/model.rs` | `Listing.explicit_count` |
| `src/trade/client.rs` | `parse_fetch` reads explicit-mod count |
| `src/trade/mod.rs` | `COMPARABLE_SAMPLE`; thread item craftability into pricing |
| `src/trade/ablation.rs` | craftability filter, trim+percentile rework, fallback ladder, thread `explicit_count` through `estimate`/`breakdown` |
| `src/discord/embeds.rs` | tier line in the estimate embed |
| `src/discord/paste.rs` | pass the parsed item's craftability into the pricer call |
| tests/fixtures | Advanced-Mode parse fixtures; fetch fixture with `explicitMods` |

## 9. Known approximations (accepted for v1 — "let's try how it goes")

- **Affix-count proxy.** We match comparables by explicit-mod *count*, not by
  prefix/suffix-resolved open-slot type. For this boot it's exact (all filled mods
  are suffixes, so the search's pinned suffixes + "no extras" ⇒ open prefixes). On
  items with mixed open slots it's coarser. Precise per-listing affix typing is a
  later upgrade if needed.
- **Pseudo aggregation vs count.** When the query uses a pseudo total (e.g. total
  elemental resistance) that our item satisfies via 2 resistance lines, a comparable
  satisfying it via 1 line has a lower `explicit_count`. Our `<=` filter keeps such
  cleaner comparables (more open) — acceptable (it biases toward the open tier), but
  noted as a source of slight over-inclusion.
- **Basic clipboard** can't classify affixes → Part-1-only estimate, clearly
  labelled. Advanced Mode is the supported path (the user already uses it).
