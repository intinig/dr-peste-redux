# Rare-Pricing Craftability — Follow-ups (C4 sample depth + C5 hybrid counting) — Design

**Date:** 2026-06-18
**Status:** Approved in brainstorming; pending spec review before planning.
**Builds on:** the craftability-tier pricing shipped in PR #11
(`2026-06-18-rare-pricing-craftability-design.md`). These are the two follow-ups
deferred from that PR's review.

## Problem

PR #11's review surfaced two real gaps, both deferred:

- **C4 — junk floor can crowd out craft-tier comps.** The estimate fetches only
  the cheapest `COMPARABLE_SAMPLE = 50` price-sorted listings, then filters to the
  item's craftability tier. If the cheapest 50 are all more-filled "junk" boots,
  zero craft-tier comps survive → `BroadMarket` fallback prices the floor — the
  exact thing the feature excludes. (codex P1 on PR #11.)
- **C5 — hybrid affixes are overcounted.** The parser tags every Advanced-Mode
  stat *line* with its prefix/suffix type, but a hybrid affix is one
  `{ Prefix/Suffix Modifier … }` block with **two** stat lines. So a hybrid counts
  as two filled slots, making the item look less craftable than it is (and slightly
  skewing the comparable filter, which keys on `explicit_count`). (codex P2.)

## Design

### C4 — fetch the whole result, then filter

Raise the price-path sample to the API result cap so craft-tier comps in the tail
aren't missed:

- `COMPARABLE_SAMPLE`: **50 → 100** (`src/trade/mod.rs`). `gather_comparables`
  already fetches `min(hashes, limit)` cheapest, so this fetches up to the full
  constrained-search result (typically far fewer than 100 for a base + 3-suffix +
  banded query) and filters once. No scan loop.
- Rate-limit posture is unchanged in shape (still bounded — at most ~10 `fetch`
  calls per query, `Retry-After` backoff intact, 60s cache dedupes the breakdown
  baseline). The per-member sessions + sticky residential proxy give the headroom.
- `BroadMarket` fallback now triggers only when **no** craft-tier base exists in the
  whole result — and PR #11 already made that path honest (Low confidence + "no
  comparable open-base listings" label).

(A lazy batched scan was considered and set aside as unnecessary complexity for
the gain, since constrained searches return small result sets.)

### C5 — count affix blocks, not stat lines (both sides)

**Our item (parser, `src/itemtext.rs`):** count one filled prefix/suffix per
`{ … Modifier }` *block*. Concretely, tag only the **first** explicit stat line
after a `{ Prefix/Suffix Modifier }` annotation; continuation lines (a hybrid
affix's second stat) get `affix: None`. Mechanically this is `current_affix.take()`
when tagging an explicit (consume the type so it applies once per block), with the
type re-set on the next `{ … }` line. Continuation lines remain in `explicits`
(they're still real stat filters for the query) — they just don't count as another
slot. `craftability()` then counts `affix.is_some()` explicits = affix blocks.

**Comparables (`parse_fetch`, `src/trade/client.rs`):** derive `explicit_count`
from the best available signal, in order:
1. `item.extended.prefixes + item.extended.suffixes` (uint counts — exact filled
   affix count, hybrid-safe),
2. else `item.extended.mods.explicit.len()` (one entry per affix),
3. else `item.explicitMods.len()` (per display line — today's behavior),
4. else `0` (unknown → excluded by the craftability filter, per PR #11's fix).

Both sides become per-affix → hybrid-safe. The craftability filter
(`explicit_count ≤ ours`) and the embed's open-slot counts are now correct for
hybrid items. For non-hybrid items (e.g. the PR #11 reference boot: 1 prefix + 3
suffixes, no hybrids) the counts are unchanged.

**Verification dependency:** the official docs mark `extended` as "Public Stash
API," but trade `fetch` responses include it in practice. The layered fallback
makes the code correct regardless of which layer fires; confirm on the next live
paste which one is active (capture one `fetch` JSON).

## Non-goals (YAGNI)

- No lazy/paginated deep-scan (C4 chose the simpler whole-result fetch).
- No change to the filter rule, percentiles, fallback ladder, sessions, or proxy.
- No prefix-vs-suffix-specific matching yet — even though layer (1) would hand us
  exact prefix/suffix counts, the filter still keys on total filled affixes
  (`explicit_count`). Tightening to "match open *prefixes* specifically" stays a
  later option, not part of this change.

## Testing

Offline unit tests:
- **Parser hybrid:** a fixture with a `{ Prefix Modifier … }` block followed by two
  stat lines → `filled_prefixes == 1`, `open_prefixes == 2`, both lines still in
  `explicits` (as query filters), only the first tagged. Existing
  `craftability_of_advanced_boots` (no hybrids) still passes unchanged.
- **`parse_fetch` layering:** fixtures exercising each layer — `extended.prefixes`+
  `suffixes` present → their sum; only `extended.mods.explicit` → its len; only
  `explicitMods` → its len; none → 0. Extend the existing parse_fetch test.
- **C4:** `COMPARABLE_SAMPLE == 100`; the existing filter/fallback tests are
  sample-size-agnostic and stay green.

Live acceptance (manual): re-paste the reference boot (unchanged result, no
hybrids) and a hybrid-affix item; confirm the "Priced as · N open prefixes" line
reflects affix blocks, not lines.

## Files touched

| File | Change |
|---|---|
| `src/trade/mod.rs` | `COMPARABLE_SAMPLE` 50 → 100 |
| `src/itemtext.rs` | `current_affix.take()` block-scoping; hybrid test |
| `src/trade/client.rs` | `parse_fetch` layered `explicit_count` extraction; fixtures |
