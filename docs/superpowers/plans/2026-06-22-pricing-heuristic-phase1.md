# Phase 1 — Heuristic Price-Check (fix production) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the hedonic-regression value path (which priced a ~240-div staff at 0.2 div) with the operator's manual price-check: constrain on the item's affixes (cornerstones exact, the rest loosely banded), relax by dropping the weakest mod until enough comparables exist, and read the price off the cheapest matches.

**Architecture:** Parse mod `Tier` from the clipboard; `build_baseline` searches cornerstone affixes exact and orders affixes so relaxation drops the weakest first; `price()` becomes a relax-and-read percentile over the tightest non-empty query; the regression engine (`hedonic.rs`, `marginal_estimate`, the value-path progress/ETA) is removed. Stateless — observation logging, learning, and the harvester are later phases.

**Tech Stack:** Rust; existing `Comparables` seam, `gather_comparables` relaxation, `estimate_from` percentiles, throttle, and batched fetch — all reused.

**Design spec:** `docs/superpowers/specs/2026-06-22-pricing-heuristic-and-market-learning-design.md` (Phase 1).

## Global Constraints

- **Only knowns are hand-coded:** cornerstone affixes (searched exact) and pseudo-grouping (already present). No hand-tuned "low-tier cutoff" — relaxation handles weak mods. Band width stays the existing loose `band()`.
- **Cold-start relaxation order:** drop the weakest affix first (highest tier number; unknown tier = weakest); cornerstones dropped last. (Learned value-ordering arrives in Phase 4.)
- **Price read:** `p20/p50/p80` (Quick/Fair/Patient) over the cheapest matches of the tightest query that returned `≥ MIN_COMPARABLES (10)`; **no craftability filter** on the price path; keep the existing bottom-trim of dump/troll outliers.
- **Breakdown ("Break it down") is left as-is** (out of scope) — it keeps using `estimate`/`craftability_filter`/`EstimateBasis::{CraftTier,BroadMarket,AffixesOnly}`.
- **Never returns empty to the user:** a fully-relaxed thin result still yields a low-confidence estimate (`BroadMarket`), never "No comparable listings found".
- Binary crate, no lib target — verify with `cargo test` (never `--lib`). Final `cargo build` zero warnings; **CI runs `cargo clippy --all-targets -- -D warnings`** (stricter than a plain local clippy — run that exact command), must be clean.
- Commit trailer (after a blank line): `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Stage files by name; never `git add -A`.

## File structure

| File | Change |
|---|---|
| `src/itemtext.rs` | `ItemStat.tier: Option<u8>` + parse `(Tier: N)`; update ItemStat literals |
| `src/trade/query.rs` | `is_cornerstone`; `build_baseline` cornerstone-exact + tier ordering |
| `src/trade/ablation.rs` | add `price_check`; remove `marginal_estimate`/`PriceProgress`/`NoProgress` |
| `src/trade/mod.rs` | `price()` → relax-and-read; `PRICE_SAMPLE`; remove `mod hedonic`; rewire tests |
| delete `src/trade/hedonic.rs` | remove the regression |
| `src/trade/model.rs` | remove `EstimateBasis::Marginal` |
| `src/trade/limiter.rs` | remove `estimate` + `estimate_secs` (+ their test) |
| `src/discord/embeds.rs` | remove the `Marginal` arm in `tier_note` |
| `src/discord/paste.rs` | remove `ReplyProgress`; `run_pricing` drops the progress arg |

---

## Task 1: Parse mod Tier into `ItemStat`

**Files:**
- Modify: `src/itemtext.rs` (`ItemStat`, `parse`, a helper, tests)
- Modify: ItemStat literals in `src/trade/query.rs`, `src/trade/mod.rs`, `src/trade/pseudo.rs` (compiler-guided)

**Interfaces:**
- Produces: `ItemStat { raw: String, value: Option<f64>, affix: Option<Affix>, tier: Option<u8> }`. Tier is the `N` from a `{ … (Tier: N) … }` annotation, applied to the first explicit line of that block (like `affix`); continuation/untagged lines get `None`.

- [ ] **Step 1: Write the failing tier test**

Add to the `tests` module in `src/itemtext.rs`:

```rust
    #[test]
    fn captures_tier_from_advanced_annotation() {
        // RARE_BOOTS_ADVANCED already exists in this module: Hellion's (Tier 1)
        // movement speed, of the Maelstrom (Tier 3) lightning res, of Magma (Tier 2)
        // fire res, of Archaeology (Tier 1) rarity.
        let p = parse(RARE_BOOTS_ADVANCED).unwrap();
        let ms = p.explicits.iter().find(|s| s.raw.contains("Movement Speed")).unwrap();
        let light = p.explicits.iter().find(|s| s.raw.contains("Lightning Resistance")).unwrap();
        let fire = p.explicits.iter().find(|s| s.raw.contains("Fire Resistance")).unwrap();
        assert_eq!(ms.tier, Some(1));
        assert_eq!(light.tier, Some(3));
        assert_eq!(fire.tier, Some(2));
    }

    #[test]
    fn tier_absent_on_basic_clipboard() {
        // RARE_BASIC has no Advanced-Mode annotations.
        let p = parse(RARE_BASIC).unwrap();
        assert!(p.explicits.iter().all(|s| s.tier.is_none()));
    }
```

(If `RARE_BASIC` is not the exact name of an existing no-annotation fixture in this module, use whichever basic-clipboard fixture exists — check the `tests` module; there is a basic rare fixture used by `craftability_none_for_basic_clipboard`.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test captures_tier_from_advanced_annotation`
Expected: compile error — `ItemStat` has no field `tier`.

- [ ] **Step 3: Add the field + a tier parser + wire it into `parse`**

In `src/itemtext.rs`, add `tier` to `ItemStat`:

```rust
pub struct ItemStat {
    pub raw: String,
    pub value: Option<f64>,
    /// Prefix/Suffix when known (Advanced-Mode `{ … Modifier … }` annotation);
    /// `None` for implicit/enchant/rune lines and for basic-clipboard pastes.
    pub affix: Option<Affix>,
    /// Affix tier `N` from a `{ … (Tier: N) … }` annotation (1 = best). `None`
    /// for basic clipboards and for continuation lines of a hybrid block.
    pub tier: Option<u8>,
}
```

Add a free helper near the other parse helpers (e.g. next to `split_tag`):

```rust
/// Extracts `N` from an Advanced-Mode annotation line like
/// `{ Prefix Modifier "Oppressor's" (Tier: 2) … }`. `None` if absent/unparseable.
fn parse_tier(annotation: &str) -> Option<u8> {
    let after = annotation.split("(Tier:").nth(1)?;
    let digits: String = after.trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}
```

In `parse`, alongside `current_affix`, track `current_tier`. Where the annotation
arm sets `current_affix`, also set `current_tier`:

```rust
        if raw_line.starts_with('{') {
            let lower = raw_line.to_lowercase();
            current_affix = if lower.contains("prefix modifier") {
                Some(Affix::Prefix)
            } else if lower.contains("suffix modifier") {
                Some(Affix::Suffix)
            } else {
                None
            };
            current_tier = parse_tier(raw_line);
            continue;
        }
```

Declare `let mut current_tier: Option<u8> = None;` next to `current_affix`, and on
the meta-line reset (`is_meta_line`) clear it too: `current_tier = None;`. In the
explicit arm, set the tier when consuming the affix:

```rust
            _ => {
                let mut s = stat;
                s.affix = current_affix.take();
                s.tier = current_tier.take(); // one tier per { … } block, like affix
                explicits.push(s);
            }
```

- [ ] **Step 4: Add `tier: None` to every other `ItemStat` literal**

Build will fail at each `ItemStat { … }` literal lacking `tier`. Add `tier: None`
to each (in `src/itemtext.rs` other tests, `src/trade/query.rs` tests,
`src/trade/mod.rs` tests, `src/trade/pseudo.rs`). The compiler lists them.

- [ ] **Step 5: Run to green**

Run: `cargo test itemtext::` then `cargo build`
Expected: the two new tests pass; existing parser tests (incl.
`craftability_of_advanced_boots`, hybrid) unchanged; build clean.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/itemtext.rs src/trade/query.rs src/trade/mod.rs src/trade/pseudo.rs
git commit -m "feat(parse): capture affix Tier from Advanced-Mode annotations"
# + trailer
```

---

## Task 2: Cornerstone detection helper

**Files:**
- Modify: `src/trade/query.rs` (`is_cornerstone` + test)

**Interfaces:**
- Produces: `fn is_cornerstone(raw: &str) -> bool` (module-private to `query.rs`) — true for movement-speed and skill-level mods (searched exact in Task 3).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/trade/query.rs`:

```rust
    #[test]
    fn cornerstone_detects_skill_levels_and_movement_speed() {
        assert!(is_cornerstone("+6 to Level of all Physical Spell Skills"));
        assert!(is_cornerstone("+1 to Level of all Spell Skills"));
        assert!(is_cornerstone("35% increased Movement Speed"));
        // Not cornerstones:
        assert!(!is_cornerstone("201% increased Spell Physical Damage"));
        assert!(!is_cornerstone("+298 to maximum Mana"));
        assert!(!is_cornerstone("52% increased Cast Speed"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test cornerstone_detects_skill_levels_and_movement_speed`
Expected: compile error — `is_cornerstone` not found.

- [ ] **Step 3: Implement**

Add near the top of `src/trade/query.rs` (module-private):

```rust
/// Cornerstone affixes are searched *exact* (min = roll, no max) because a worse
/// roll is a materially different item: `+X to skill levels` and movement speed.
/// This is the one hand-coded value-known; everything else is banded/relaxed.
fn is_cornerstone(raw: &str) -> bool {
    let l = raw.to_lowercase();
    l.contains("movement speed") || (l.contains("to level of") && l.contains("skill"))
}
```

- [ ] **Step 4: Run to green**

Run: `cargo test query::cornerstone`
Expected: PASS.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/trade/query.rs
git commit -m "feat(trade): cornerstone-affix detection (skill levels, movement speed)"
# + trailer
```

---

## Task 3: `build_baseline` — cornerstone-exact bands + weakest-last ordering

**Files:**
- Modify: `src/trade/query.rs` (`build_baseline` per-mod loop + ordering; tests)

**Interfaces:**
- Consumes: `is_cornerstone` (Task 2), `ItemStat.tier` (Task 1).
- Produces: `build_baseline` emits `stats` ordered `[cornerstones] ++ [pseudo aggregates] ++ [normal mods strongest→weakest by tier]`, with cornerstone filters using `min = Some(roll), max = None` and normal mods using `band()`. So `gather_comparables` (which pops the last stat) relaxes weakest-first and drops cornerstones last.

- [ ] **Step 1: Write the failing tests**

Add to `src/trade/query.rs` `tests` (the staff fixture exercises cornerstone-exact
+ ordering). Reuse the existing `StatCatalog::from_json(include_str!("fixtures/stats_sample.json"))`
pattern; if the sample catalog doesn't match these staff mods, the assertions that
depend on matched filters won't see them — so this test builds the item with mods
the sample catalog DOES match. Use a minimal item with one cornerstone + two
normal mods of different tiers whose labels the sample catalog matches (check
`fixtures/stats_sample.json` for ids; movement speed + two resistances are present
in the boots fixture's catalog usage):

```rust
    #[test]
    fn cornerstone_searched_exact_and_weakest_last() {
        let item = ParsedItem {
            rarity: crate::itemtext::Rarity::Rare,
            name: "Test".into(),
            base_type: Some("Sandsworn Sandals".into()),
            item_class: Some("Boots".into()),
            item_level: Some(80),
            quality: None,
            corrupted: false,
            energy_shield: None,
            armour: None,
            evasion: None,
            implicits: vec![],
            enchants: vec![],
            runes: vec![],
            explicits: vec![
                ItemStat { raw: "35% increased Movement Speed".into(), value: Some(35.0), affix: Some(crate::itemtext::Affix::Prefix), tier: Some(1) },
                ItemStat { raw: "+34% to Lightning Resistance".into(), value: Some(34.0), affix: Some(crate::itemtext::Affix::Suffix), tier: Some(3) },
                ItemStat { raw: "+39% to Fire Resistance".into(), value: Some(39.0), affix: Some(crate::itemtext::Affix::Suffix), tier: Some(2) },
            ],
        };
        let catalog = StatCatalog::from_json(include_str!("fixtures/stats_sample.json")).unwrap();
        let q = build_baseline(&item, &PseudoMap::load(), &catalog, "Standard");

        // Cornerstone (movement speed) is searched exact: min set, no max.
        let ms = q.stats.iter().find(|s| s.label.contains("Movement Speed"))
            .expect("movement speed filter present");
        assert_eq!(ms.min, Some(35.0));
        assert_eq!(ms.max, None);

        // Resistances pseudo-group into total elemental resistance (not per-mod),
        // so assert ordering via the cornerstone being before any non-cornerstone.
        let ms_idx = q.stats.iter().position(|s| s.label.contains("Movement Speed")).unwrap();
        assert!(q.stats.iter().enumerate().all(|(i, s)|
            s.label.contains("Movement Speed") || i > ms_idx),
            "cornerstone must precede all non-cornerstone filters");
    }
```

(If pseudo-grouping collapses both resistances into one filter, the ordering
assertion still holds because the cornerstone is first. If the sample catalog
doesn't match movement speed, replace the base/mods with ones it matches — verify
against `fixtures/stats_sample.json` while writing; the test must exercise a real
matched cornerstone filter.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test cornerstone_searched_exact_and_weakest_last`
Expected: FAIL — movement speed currently uses `band()` (has a `max`), and ordering
is build-order not cornerstone-first.

- [ ] **Step 3: Implement cornerstone-exact band + ordering**

In `src/trade/query.rs` `build_baseline`, the per-mod `buckets` loop currently
pushes a banded `StatFilter` per matched explicit. Change it to (a) tag each with
its tier + cornerstone flag, and (b) use an exact band for cornerstones; then
reorder. Replace the per-mod loop body so that, instead of pushing directly into
`stats`, it collects into a temporary `Vec<(bool /*cornerstone*/, Option<u8> /*tier*/, StatFilter)>`:

```rust
    // Per-mod explicit filters, tagged for ordering. Cornerstones are searched
    // exact (min = roll, no max); everything else uses the loose band.
    let mut mod_filters: Vec<(bool, Option<u8>, StatFilter)> = Vec::new();
    for m in &item.explicits {
        if pseudo.covers(&m.raw) {
            continue;
        }
        if let Some(id) = catalog.match_stat(&m.raw, StatGroup::Explicit) {
            let corner = is_cornerstone(&m.raw);
            let (min, max) = if corner {
                (m.value, None) // exact: at least this roll, no upper bound
            } else {
                m.value.map(band).unwrap_or((None, None))
            };
            mod_filters.push((corner, m.tier, StatFilter { id, label: m.raw.clone(), min, max }));
        } else {
            tracing::debug!(item_mod = %m.raw, "no trade2 stat match; skipping filter");
        }
    }
    // Order: cornerstones first (dropped last in relaxation), then normal mods
    // strongest→weakest by tier (lower tier number = stronger; unknown tier =
    // weakest, sorted last). `gather_comparables` pops the last stat, so the
    // weakest normal mod is dropped first and cornerstones survive longest.
    mod_filters.sort_by_key(|(corner, tier, _)| (!*corner, tier.unwrap_or(u8::MAX)));
    // `stats` already holds the pseudo-aggregate filters; append the ordered mods
    // after them, but keep cornerstones ahead of pseudo so they're dropped last.
    let (corners, normals): (Vec<_>, Vec<_>) = mod_filters.into_iter().partition(|(c, _, _)| *c);
    let mut ordered: Vec<StatFilter> = corners.into_iter().map(|(_, _, f)| f).collect();
    ordered.append(&mut stats); // pseudo aggregates in the middle
    ordered.extend(normals.into_iter().map(|(_, _, f)| f));
    let stats = ordered;
```

(The existing code builds the pseudo-aggregate filters into `stats` *before* the
buckets loop — keep that. The snippet above replaces the buckets loop and the
final ordering; `stats` is rebound to the ordered vec. Adjust the surrounding
`let mut stats` / final `TradeQuery { … stats … }` so the rebinding compiles —
`stats` must not be `mut`-borrowed after rebind.)

The `buckets` array and its outer loop (over implicits/enchants/runes/explicits)
no longer apply — Phase-0 already reduced it to explicits-only; replace that loop
entirely with the `mod_filters` loop above.

- [ ] **Step 4: Run to green**

Run: `cargo test query::` then `cargo test`
Expected: the new test passes; existing `build_baseline` tests pass (resistances
still pseudo-group; the explicit-only contract from the prior phase is unchanged).
If an existing test asserted a specific *order* of `stats`, update it to the new
cornerstone-first/weakest-last order.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/trade/query.rs
git commit -m "feat(trade): cornerstone-exact search bands + weakest-last affix ordering"
# + trailer
```

---

## Task 4: Relax-and-read price path; remove the regression engine

This is one atomic task: `price()`'s new engine and the removal of the old one
must land together to keep the crate compiling and clippy-clean.

**Files:**
- Modify: `src/trade/ablation.rs` (add `price_check`; remove `marginal_estimate`, `PriceProgress`, `NoProgress`, now-unused imports; update tests)
- Modify: `src/trade/mod.rs` (`price()` → relax-and-read; `PRICE_SAMPLE`; remove `pub mod hedonic`; update imports + tests)
- Modify: `src/trade/query.rs` (remove now-orphaned `base_query` + its test)
- Delete: `src/trade/hedonic.rs`
- Modify: `src/trade/model.rs` (remove `EstimateBasis::Marginal`)
- Modify: `src/discord/embeds.rs` (remove the `Marginal` arm)
- Modify: `src/trade/limiter.rs` (remove `estimate` + `estimate_secs` + their test)
- Modify: `src/discord/paste.rs` (remove `ReplyProgress`; `run_pricing` drops the progress arg)

**Interfaces:**
- Consumes: `gather_comparables` via the `Comparables` seam, `estimate_from`, `MIN_COMPARABLES` (all existing in `ablation.rs`).
- Produces: `pub async fn price_check<C: Comparables + ?Sized>(c: &C, query: &TradeQuery, limit: usize, max_relax: usize, session: &TradeSession) -> Result<PriceEstimate>`; `TradePricer::price(&self, item, league, session) -> Result<PriceEstimate>` (no `progress` param).

- [ ] **Step 1: Write the failing price-path test**

In `src/trade/mod.rs` `tests`, add (a fake that returns a fixed comparable set so
the percentile read is deterministic):

```rust
    #[tokio::test]
    async fn price_reads_percentiles_over_comparables_no_progress_arg() {
        // 12 comparables 1.0..12.0 div; price() reads p20/p50/p80 over them.
        struct Comps;
        #[async_trait]
        impl Comparables for Comps {
            async fn comparables(&self, _q: &TradeQuery, _l: usize, _mr: usize, _s: &TradeSession)
                -> anyhow::Result<Vec<Listing>> {
                Ok((1..=12).map(|i| make_listing(i as f64, 1, &format!("c{i}"))).collect())
            }
        }
        let pricer = make_pricer(Comps);
        let est = pricer.price(&ring(), "Standard", &TradeSession::for_test()).await.unwrap();
        assert!(est.typical > 0.0 && est.typical <= 12.0);
        assert!(est.low <= est.typical && est.typical <= est.high);
        assert_eq!(est.listing_count, 12);
        assert_eq!(est.basis, EstimateBasis::CraftTier); // >= MIN_COMPARABLES found
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test price_reads_percentiles_over_comparables_no_progress_arg`
Expected: compile error — `price()` still takes a `progress` arg.

- [ ] **Step 3: Add `price_check` to `ablation.rs`**

Add (near `estimate`):

```rust
/// Relax-and-read price-check: gather comparables (relaxing the query, weakest
/// affix first, until `MIN_COMPARABLES` exist or `max_relax` is hit), then read
/// p20/p50/p80 over the cheapest matches. No craftability filter — the query
/// constraint plus the cheapest-first read define the comparable set. Never empty.
pub async fn price_check<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    max_relax: usize,
    session: &TradeSession,
) -> Result<PriceEstimate> {
    let listings = c.comparables(query, limit, max_relax, session).await?;
    let basis = if listings.len() >= MIN_COMPARABLES {
        EstimateBasis::CraftTier
    } else {
        EstimateBasis::BroadMarket
    };
    Ok(estimate_from(&listings, basis))
}
```

- [ ] **Step 4: Remove the regression engine from `ablation.rs`**

Delete `marginal_estimate` (lines ~49–159), `PriceProgress` (the trait, ~34–38),
and `NoProgress` (~40–47). Remove now-unused imports: `use std::time::Duration;`,
`use crate::trade::limiter::Endpoint;`, and `use crate::trade::query::base_query;`
(verify none are still referenced — `base_query`/`Endpoint`/`Duration` were used
only by `marginal_estimate`). Keep `gather_comparables`, `estimate`,
`craftability_filter`, `estimate_from`, `percentile`, `modal_currency`,
`breakdown`, `MIN_COMPARABLES`, `PROBE_CEILING`.

**Also remove the now-orphaned `base_query` from `src/trade/query.rs`** — its only
caller was `marginal_estimate`. Delete the `pub fn base_query` and its test
(`base_query_clears_stats_keeps_type_and_misc`); leaving it would fail CI's
`-D dead_code`. (Phase 3's harvester will re-introduce a base/category query when
it actually needs one.)

In `ablation.rs` `tests`, delete the marginal tests and their fakes:
`CountingFake`, `RecProgress`, `marginal_estimate_fits_and_reports_progress`,
`marginal_estimate_captures_pseudo_feature_via_provenance`, and any other test
referencing `marginal_estimate`/`NoProgress`/`hedonic`. Add a `price_check` test:

```rust
    #[tokio::test]
    async fn price_check_relaxes_then_reads_percentiles() {
        // Fake returns few for the full query, enough once relaxed.
        struct Relaxer;
        #[async_trait]
        impl Comparables for Relaxer {
            async fn comparables(&self, q: &TradeQuery, _l: usize, max_relax: usize, _s: &TradeSession)
                -> anyhow::Result<Vec<Listing>> {
                // Mimic gather: with relaxation available, return a healthy set.
                let n = if max_relax > 0 { 12 } else { 2 };
                Ok((0..n).map(|i| listing(2.0 + i as f64)).collect())
            }
        }
        let q = two_stat_query();
        let est = price_check(&Relaxer, &q, 40, q.stats.len(), &TradeSession::for_test()).await.unwrap();
        assert_eq!(est.basis, EstimateBasis::CraftTier);
        assert!(est.low <= est.typical && est.typical <= est.high);
    }
```

(Use the existing `listing(divine)` / `two_stat_query()` test helpers in
`ablation.rs`; if `two_stat_query` was removed with the marginal tests, build a
small `TradeQuery` inline with two `StatFilter`s.)

- [ ] **Step 5: Rewire `price()` in `mod.rs` + add `PRICE_SAMPLE` + drop `mod hedonic`**

In `src/trade/mod.rs`:

- Remove `pub mod hedonic;` (line 6).
- Change the imports: `use crate::trade::ablation::{price_check, Comparables};`
  (drop `estimate` and `MIN_COMPARABLES` — no longer used here; `breakdown` is
  referenced as `crate::trade::ablation::breakdown`).
- Add the const near `COMPARABLE_SAMPLE`:

```rust
/// Cheapest matches fetched for the price-check percentile read. Smaller than the
/// breakdown's COMPARABLE_SAMPLE: p20/p50/p80 over the cheapest ~40 is stable and
/// keeps the relax-and-read latency low (≤4 fetch batches per relaxation step).
const PRICE_SAMPLE: usize = 40;
```

- Replace `price()` entirely:

```rust
    pub async fn price(
        &self,
        item: &ParsedItem,
        league: &str,
        session: &TradeSession,
    ) -> Result<PriceEstimate> {
        let query = build_baseline(item, &self.pseudo, &self.catalog, league);
        // Relax up to the number of stat filters so the query can broaden all the
        // way to the bare base if needed; build_baseline ordered them weakest-last
        // so relaxation drops the weakest affix first and cornerstones last.
        let max_relax = query.stats.len();
        let est = price_check(&self.comparables, &query, PRICE_SAMPLE, max_relax, session).await?;
        self.record(&query, &est);
        Ok(est)
    }
```

- [ ] **Step 6: Remove `EstimateBasis::Marginal` + its embed arm**

In `src/trade/model.rs`, delete the `Marginal` variant from `EstimateBasis`.
In `src/discord/embeds.rs` `tier_note`, delete the `Marginal => …` arm (line ~141).

- [ ] **Step 7: Remove the limiter ETA (`estimate`/`estimate_secs`)**

In `src/trade/limiter.rs`, delete the `pub fn estimate` method (~157), the free
`fn estimate_secs` (~231), and the `estimate_scales_with_n_and_rules` test (~393).
Keep `acquire`, `observe`, `wait_secs`, `Bucket`, etc. (Verify nothing else calls
`estimate`/`estimate_secs` — only `marginal_estimate` did.)

- [ ] **Step 8: Simplify `run_pricing` in `paste.rs`**

Delete the `ReplyProgress` struct + its `impl PriceProgress` (lines ~100–121).
In `run_pricing`, drop the `progress` value and pass no progress to `price`:

```rust
    let pricer = ctx.data().pricer.clone();
    let reply = ctx
        .send(poise::CreateReply::default().content("⏳ Pricing…"))
        .await?;
    let est = match pricer.price(parsed, &league.name, session).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "trade price failed");
            reply
                .edit(*ctx, poise::CreateReply::default()
                    .content("Couldn't reach trade right now — try again shortly."))
                .await?;
            return Ok(());
        }
    };
```

(The rest of `run_pricing` — secondary rate, embed edit, breakdown collector —
is unchanged.)

- [ ] **Step 9: Update `mod.rs` tests for the new `price()` signature**

Remove the value-path routing tests (`routing_thin_exact_takes_value_path`,
`routing_fat_exact_takes_fast_path`, `basic_clipboard_thin_exact_enters_value_path`)
and their fakes (`RoutingFake`, `FatFake`, inline `ThinFake`/`ThinFakeBasic`) and
any `EstimateBasis::Marginal` / `NoProgress` references. Update
`price_logs_a_probe_and_returns_estimate` to drop the `&NoProgress` arg:

```rust
        let est = pricer
            .price(&ring(), "Standard", &TradeSession::for_test())
            .await
            .unwrap();
```

Keep `make_listing`, `make_pricer`, `ring`, `craftable_ring`, `Flat`, plus the new
`price_reads_percentiles_over_comparables_no_progress_arg` from Step 1.

- [ ] **Step 10: Build, full suite, strict clippy**

Run: `cargo build` then `cargo test` then `cargo clippy --all-targets -- -D warnings`
Expected: zero build warnings; suite green; clippy clean (no dead code from the
removals — `hedonic.rs` gone, `estimate`/`estimate_secs`/`PriceProgress` removed,
`run_pricing` no longer references `ReplyProgress`).

- [ ] **Step 11: Format, commit**

```bash
cargo fmt
git add src/trade/ablation.rs src/trade/mod.rs src/trade/query.rs src/trade/model.rs src/trade/limiter.rs src/discord/embeds.rs src/discord/paste.rs
git rm src/trade/hedonic.rs
git commit -m "feat(trade): relax-and-read price-check; remove hedonic regression value path"
# + trailer
```

---

## Final verification (after all tasks)

- [ ] `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` green; `cargo build` zero warnings.
- [ ] **Manual live acceptance** (after deploy): `/paste` the Chiming Staff → a **sane div estimate** (tens of div, not 0.2), Quick/Fair/Patient spread, no "No comparable listings found", and no `trade2 fetch failed`/429 in `docker logs`. A common rare (boot) still prices sensibly.
- [ ] Confirm "Break it down" still works (breakdown path untouched).
- [ ] Note for Phase 2: `price()` still logs via the old `Probe`/`ProbeLog`; the per-listing observation corpus + mounted volume replace it next.
