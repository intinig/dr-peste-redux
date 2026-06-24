# Corpus Filter + Sub-1-div Floor — Design

**Goal:** Make value-model categories (Staff first) trustable by removing corpus
noise, not by tuning thresholds. A category clears the trust bar
(`sample_size ≥ 80 && loo_error ≤ 0.50`) only when its data genuinely supports a
prediction within 50% — never by relaxing the bar.

**Architecture (one sentence):** Capture (`parse_fetch`) drops only absurd troll
prices (`is_below_absurd_cap`), keeping sub-1-div listings; the value-model rebuild
learns only from priceable (`is_priceable`), timestamped, fresh rows — so the 1-div
floor is a consumption-only concern; and `/paste` short-circuits an item to a "not
worth pricing" line when its exact comparables' median is under 1 div.

**Tech stack:** Rust (no new deps). Touches `src/trade/client.rs` (capture),
a new `src/trade/quality.rs` (predicate), `src/trade/value/mod.rs` (consumption),
`src/discord/paste.rs` (+ `embeds.rs` if needed).

---

## Why (grounding in the live-corpus profile)

Profiling the live corpus (9,342 rows; Staff = 4,167) showed Staff is **not**
short on data — it fails the trust bar on **LOO error**, because ~45% of the rows
it learns from are noise or stale:

| Issue | Staff rows | Effect |
|---|---|---|
| dust (`< 0.1 div`) | 412 | fake near-zero prices |
| sub-1-div generally | (subsumes dust) | values we don't care about |
| "999" sell-walls | 135 | fake highs next to real 5–50 div items |
| absurd trolls (1.1M, 9999×2) | 3 | blow up relative error |
| undated, cheap-biased (pre-PR#22) | ~1,671 | median 20 vs 50 fresh → drags model cheap |
| **clean & fresh, in-band** | **~2,112** | what the model *should* learn from |

The weighted-median k-NN is inherently robust to a *minority* of high outliers, so
the dominant, fixable harm is the systematically-cheap contamination (dust +
undated). See memory `pricing-truth-seeking-not-tuning`, `value-model-cross-category-findings`,
`pricing-rework-phases`.

## Decisions (settled in brainstorming, 2026-06-24)

1. **Filter placement — split by nature, with the sub-1-div floor consumption-only.**
   The absurd-upper troll cap drops at *capture* (consistent with the existing
   mirror-tier/veiled drops in `parse_fetch`) — it is pure junk on every path. The
   **sub-1-div floor is applied only at consumption** (corpus learning), NOT at
   capture: the live `/paste` pricer must still *see* sub-1-div listings so it can
   detect and report a genuinely cheap item (see Component 4). Time-relative
   exclusion (freshness, undated legacy) is also consumption-only. Raw rows are never
   destroyed → Keep+Filter preserved.
   *(This corrects an earlier draft that dropped sub-1-div at capture; that made the
   Component-4 short-circuit unreachable, since the live median could then never fall
   below 1 div. Caught in whole-branch review.)*
2. **Sell-walls — conservative + re-measure.** Add an upper sanity backstop for the
   absurd trolls the mirror-tier filter misses; do **not** special-case round-number
   "999" walls (median absorbs ~6% high outliers). Ship, then re-measure Staff LOO;
   escalate to wall detection only if still > 0.50.
3. **Sub-1-div on `/paste` — median < 1 div.** If the live ablation's representative
   (p50) value is under 1 divine, show a clean "under 1 div" line and skip the
   precise breakdown + learned estimate. Rare/magic ablation path only.

---

## Components

### 1. Shared price-quality predicate — `src/trade/quality.rs` (new)

A small, single-responsibility, fully-offline-testable unit consumed by both the
capture path and the corpus rebuild:

```rust
/// Below this an item isn't worth pricing precisely; corpus rows under it carry
/// no signal for the value model.
pub const MIN_PRICEABLE_DIVINE: f64 = 1.0;

/// Backstop upper bound for absurd troll listings (e.g. 1,111,111 div) in the rare
/// case the mirror-tier filter can't run (mirror rate unavailable). Set far above
/// any legitimate single-item price in a league.
pub const ABSURD_DIVINE_CAP: f64 = 100_000.0;

/// In the band we LEARN from: `MIN_PRICEABLE_DIVINE <= price < ABSURD_DIVINE_CAP`.
/// Used at corpus consumption (`value::rebuild_into`).
pub fn is_priceable(price_divine: f64) -> bool;

/// Capture-time ceiling check: `price < ABSURD_DIVINE_CAP`. Imposes NO 1-div floor,
/// so sub-1-div listings reach the live `/paste` pricer (Component 4). Used in
/// `client::parse_fetch`.
pub fn is_below_absurd_cap(price_divine: f64) -> bool;
```

`MIN_PRICEABLE_DIVINE` is a floor on *what we bother pricing*, not a tuned model
parameter — it does not move with any observed/target price.

### 2. Capture — `src/trade/client.rs::parse_fetch`

After the existing currency-convert + `price_divine <= 0.0` + mirror-tier + veiled
drops, add: `if !crate::trade::quality::is_below_absurd_cap(price_divine) { return None; }`.
This drops only absurd troll prices the mirror-tier filter can't catch when the
mirror rate is unavailable — it does **not** impose the 1-div floor. Sub-1-div
listings are intentionally kept here so the live `/paste` pricer (Component 4) can
detect a genuinely cheap item; the 1-div floor is a corpus-learning concern applied
in Component 3. The existing `≥ 0.8 × mirror` filter stays as the primary upper bound;
`is_below_absurd_cap` only bites when mirror conversion was unavailable.

### 3. Consumption — `src/trade/value/mod.rs::rebuild_into`

Two changes to the learning filter, so the **existing** on-disk corpus is cleaned
retroactively (no re-harvest, no destructive file rewrite):

- **Price band:** also require `is_priceable(o.price_divine)` — excludes the dust
  and any legacy troll rows already logged.
- **Timestamp required:** learn only from rows with a *present, parseable,
  ≤ `MAX_LISTING_AGE_DAYS`* timestamp. Today `is_fresh_at(None, …)` returns `true`
  (undated kept); the model build must instead treat undated/unparseable as
  **not learnable** and drop them. This removes the 1,671 cheap-biased pre-PR#22
  rows.

`is_fresh_at`'s semantics are **not** changed globally — the live `gather_comparables`
path keeps "absent timestamp ⇒ kept" (live fetches are recent). The
"timestamp required" rule is local to model learning, expressed as a build-side
predicate (e.g. `o.indexed.as_deref().is_some_and(|t| parses && fresh)`).

### 4. `/paste` sub-1-div short-circuit — `src/discord/paste.rs` (+ `embeds.rs`)

After live ablation produces its representative estimate, branch on the **p50/median**:

- `p50 < MIN_PRICEABLE_DIVINE` → render a clean message — e.g.
  *"💸 **<item name>** (<rarity>) — worth under 1 divine, not worth pricing precisely."*
  — and **skip** the p20/p50/p80 breakdown and the learned estimate line.
- `p50 ≥ MIN_PRICEABLE_DIVINE` → unchanged (full breakdown + learned estimate as today),
  including the range-straddles-1-div case.

Snapshot-matched uniques/currency (the non-ablation `/paste` path) are unaffected —
they still show their real number.

---

## Data flow (unchanged in spirit)

trade2 fetch → `parse_fetch` (now also drops `!is_below_absurd_cap` — absurd trolls
only; sub-1-div listings are kept) → Listings → (a) live ablation `/paste`, (b)
`Observation` corpus JSONL → `rebuild_into` (drops `!is_priceable` — the 1-div floor —
+ undated) → `ValueModel`. The on-disk corpus records every captured row (incl.
sub-1-div) with its age; the 1-div floor is applied only at learning.

## Testing

- `quality::is_priceable`: boundaries — `1.0` priceable, `0.999` dropped, `0.0`/dust
  dropped, just-below `ABSURD_DIVINE_CAP` priceable, `ABSURD_DIVINE_CAP` and above
  dropped.
- `parse_fetch`: a fixture with two sub-1-div rows, an absurd-troll row (mirror rate
  absent), and an in-band row → the sub-1-div and in-band rows are **kept**, only the
  absurd troll is dropped (existing mirror/veiled cases still pass).
- `rebuild_into`: a mixed fixture (dust + undated-but-otherwise-valid + clean fresh
  in-band) → resulting `CategoryModel.sample_size` counts only the clean fresh
  in-band rows; undated and sub-1-div are excluded.
- `/paste` rendering: median `< 1 div` → short-circuit message, no breakdown / no
  learned line; median `≥ 1 div` (incl. p20 `< 1 ≤` p50) → full breakdown unchanged.
- **Regression:** an all-clean, all-fresh, all-in-band corpus → model byte-identical
  to today; a ≥1-div paste prices exactly as today.

## Success criteria

- On next refresh, Staff (and the other four categories) rebuild from clean, fresh,
  in-band rows with no re-harvest. Operator re-measures via `/insights`.
- If Staff still exceeds `0.50` LOO after cleaning, that is the honest signal the
  category is intrinsically hard (combination-dominant + Desecrated premium + wide
  spread) — **not** a reason to move the trust bar.
- No code path tunes a threshold to match the operator's price prior.

## Out of scope (v1)

- Frequency-spike / round-number wall detection (deferred; revisit only if
  post-clean re-measurement shows walls still dominate LOO).
- One-time JSONL cleanup/rewrite (kept non-destructive — legacy junk stays on disk
  but is filtered at consumption, per Keep+Filter).
- Changing the trust-bar constants (`TRUST_MIN_SAMPLE`, `TRUST_MAX_ERROR`).
- Sub-1-div handling for the snapshot/uniques `/paste` path and for `/price`/`/farm`.

## Risks & mitigations

- **Cleaning drops a category below the 80-row floor.** Mitigation: Staff's clean
  fresh count is ~2,112 — far above 80; thinner categories simply stay untrusted
  (correct). The build still works on any size.
- **`MIN_PRICEABLE_DIVINE` hides a genuinely-cheap-but-wanted item on `/paste`.**
  Mitigation: the message still names the item + rarity and states it's sub-1-div;
  the threshold reflects an explicit operator decision ("we don't care about
  sub-1-div items").
- **Undated-row exclusion silently shrinks the model.** Mitigation: rebuild already
  logs `categories=`; add per-category sample counts are already visible via
  `/insights`. All future captures are timestamped (since PR #22), so this only ever
  removes legacy rows.
