# Rare-Pricing Craftability Follow-ups (C4 + C5) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make craftability pricing hybrid-safe (count affix blocks, not stat lines) and stop the junk floor from crowding out craft-tier comparables (fetch the whole constrained result before filtering).

**Architecture:** Two small, independent changes to the existing rare-pricing engine — a parser tweak + a layered `parse_fetch` count (C5), and a sample-size bump (C4).

**Tech Stack:** Rust; `src/itemtext.rs` parser + `src/trade/{client,mod}.rs`.

**Design spec:** `docs/superpowers/specs/2026-06-18-rare-pricing-craftability-followups-design.md`.

## Global Constraints

- **Value model unchanged:** filter keeps comparables with `explicit_count ≤ ours`; counts must be **per-affix (block), hybrid-safe**, on both sides.
- **Comparable count layering (exact order):** `extended.prefixes + extended.suffixes` → `extended.mods.explicit.len()` → `explicitMods.len()` → `0` (unknown → excluded by the existing filter).
- **Parser:** one filled prefix/suffix per `{ … Modifier }` block (consume the affix type after the first explicit line of a block).
- **`COMPARABLE_SAMPLE = 100`** (was 50).
- Binary crate, no lib target — `cargo test` (never `--lib`); build stays **zero warnings**.
- Commit trailer (after a blank line): `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. Stage files by name; never `git add -A`.

---

## Task 1: C5 — hybrid-safe affix counting (parser blocks + layered fetch count)

**Files:**
- Modify: `src/itemtext.rs` (parse loop + a test)
- Modify: `src/trade/client.rs` (`parse_fetch` + tests)

**Interfaces:**
- Consumes: `Affix`, `ItemStat.affix`, `ParsedItem::craftability()` (already exist); `Listing.explicit_count` (already exists).
- Produces: no new public API; `craftability()` counts now reflect affix blocks; `parse_fetch` populates `explicit_count` via the layered rule.

- [ ] **Step 1: Parser — write the failing hybrid test**

Add to the `tests` module in `src/itemtext.rs`:

```rust
    // One prefix BLOCK with two stat lines (a hybrid affix) + one suffix block.
    const RARE_HYBRID: &str = "Item Class: Body Armours\nRarity: Rare\nHybrid Test\nVaal Regalia\n--------\nItem Level: 80\n--------\n{ Prefix Modifier \"Of the Bear\" (Tier: 1) }\n+50 to maximum Life\n+30 to maximum Mana\n{ Suffix Modifier \"of Magma\" (Tier: 2) }\n+40% to Fire Resistance\n";

    #[test]
    fn hybrid_affix_counts_as_one_block() {
        let p = parse(RARE_HYBRID).unwrap();
        // Both hybrid stat lines are kept as query filters...
        assert_eq!(p.explicits.len(), 3); // life, mana, fire res
        // ...but only the first carries the prefix tag (one block = one slot).
        let life = p.explicits.iter().find(|s| s.raw.contains("maximum Life")).unwrap();
        let mana = p.explicits.iter().find(|s| s.raw.contains("maximum Mana")).unwrap();
        assert_eq!(life.affix, Some(Affix::Prefix));
        assert_eq!(mana.affix, None); // continuation line of the same block
        let c = p.craftability().expect("advanced-mode tags present");
        assert_eq!(c.filled_prefixes, 1);
        assert_eq!(c.open_prefixes, 2);
        assert_eq!(c.filled_suffixes, 1);
        assert_eq!(c.explicit_count, 2); // 1 prefix block + 1 suffix block
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test hybrid_affix_counts_as_one_block`
Expected: FAIL — currently both hybrid lines are tagged `Prefix`, so `filled_prefixes == 2` / `explicit_count == 3`.

- [ ] **Step 3: Parser — consume the affix per block**

In `src/itemtext.rs` `parse`, change the explicit arm so the affix type is taken once per block:

```rust
            _ => {
                let mut s = stat;
                s.affix = current_affix.take(); // one slot per { … } block; hybrid continuation lines get None
                explicits.push(s);
            }
```

(`current_affix` is already `let mut`. `.take()` returns the current value and resets it to `None`, so only the first explicit after a `{ Prefix/Suffix Modifier }` block is tagged; the next `{ … }` line re-sets it.)

- [ ] **Step 4: Run parser tests to green**

Run: `cargo test itemtext::`
Expected: PASS — the new hybrid test plus the existing `craftability_of_advanced_boots` (no hybrids → unchanged: 1 prefix, 3 suffixes, explicit_count 4) and `craftability_none_for_basic_clipboard`.

- [ ] **Step 5: `parse_fetch` — write the failing layered-count test**

In `src/trade/client.rs` `tests`, add:

```rust
    #[test]
    fn parse_fetch_affix_count_layers() {
        let client = test_client();
        let v = serde_json::json!({
            "result": [
                // Layer 1: extended.prefixes + suffixes (exact, hybrid-safe)
                { "listing": { "price": { "amount": 1.0, "currency": "divine" } },
                  "item": { "extended": { "prefixes": 2, "suffixes": 3 },
                            "explicitMods": ["a","b","c","d","e","f"] } },
                // Layer 2: extended.mods.explicit (one entry per affix)
                { "listing": { "price": { "amount": 2.0, "currency": "divine" } },
                  "item": { "extended": { "mods": { "explicit": ["x","y"] } },
                            "explicitMods": ["x1","x2","y1"] } },
                // Layer 3: explicitMods only (display lines)
                { "listing": { "price": { "amount": 3.0, "currency": "divine" } },
                  "item": { "explicitMods": ["p","q","r","s"] } },
                // Layer 4: nothing → 0 (unknown)
                { "listing": { "price": { "amount": 4.0, "currency": "divine" } },
                  "item": {} }
            ]
        });
        let ls = client.parse_fetch(&v);
        assert_eq!(ls.len(), 4);
        let ec = |amt: f64| ls.iter().find(|l| l.price.amount == amt).unwrap().explicit_count;
        assert_eq!(ec(1.0), 5); // 2 + 3, NOT the 6 explicitMods lines
        assert_eq!(ec(2.0), 2); // per-affix, NOT the 3 lines
        assert_eq!(ec(3.0), 4); // line count
        assert_eq!(ec(4.0), 0); // unknown
    }
```

- [ ] **Step 6: Run to verify failure**

Run: `cargo test parse_fetch_affix_count_layers`
Expected: FAIL — current code only reads `explicitMods`, so `ec(1.0)==6` and `ec(2.0)==3`.

- [ ] **Step 7: `parse_fetch` — layered extraction**

In `src/trade/client.rs`, add a free helper (module level, near `parse_rate_rules`):

```rust
/// Filled prefix+suffix affix count for a fetched item, hybrid-safe, from the
/// best signal the trade2 fetch response carries:
/// `extended.prefixes+suffixes` → `extended.mods.explicit` → `explicitMods` → 0.
fn affix_count(item: &Value) -> usize {
    if let Some(ext) = item.get("extended") {
        let p = ext.get("prefixes").and_then(|v| v.as_u64());
        let s = ext.get("suffixes").and_then(|v| v.as_u64());
        if p.is_some() || s.is_some() {
            return (p.unwrap_or(0) + s.unwrap_or(0)) as usize;
        }
        if let Some(n) = ext
            .get("mods")
            .and_then(|m| m.get("explicit"))
            .and_then(|e| e.as_array())
            .map(|a| a.len())
        {
            return n;
        }
    }
    item.get("explicitMods")
        .and_then(|m| m.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}
```

Then in `parse_fetch`, replace the current `explicit_count` block:

```rust
                        let explicit_count = entry
                            .get("item")
                            .and_then(|it| it.get("explicitMods"))
                            .and_then(|m| m.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
```

with:

```rust
                        let explicit_count =
                            entry.get("item").map(affix_count).unwrap_or(0);
```

- [ ] **Step 8: Run to green**

Run: `cargo test client::` then `cargo test`
Expected: PASS — the new layered test, the existing `parse_fetch_drops_unconvertible_currency_listings` (its items use `explicitMods` only → layer 3, counts unchanged: 3 and 4), and the full suite. Build zero warnings.

- [ ] **Step 9: Format, lint, commit**

```bash
cargo fmt && cargo clippy
git add src/itemtext.rs src/trade/client.rs
git commit -m "fix(trade): count affix blocks not display lines (hybrid-safe craftability)"
# + trailer
```

---

## Task 2: C4 — fetch the whole constrained result before filtering

**Files:**
- Modify: `src/trade/mod.rs`

**Interfaces:**
- Consumes/Produces: none new — only the value of `COMPARABLE_SAMPLE` changes.

- [ ] **Step 1: Bump the sample**

In `src/trade/mod.rs`, replace:

```rust
/// Number of cheapest listings to fetch per query before craftability filtering.
/// Widened so craft-tier comparables aren't crowded out by a deep junk floor before
/// the filter runs. (A fuller fix — paginating further when no craft-tier survivors
/// are found — is tracked as a follow-up.)
const COMPARABLE_SAMPLE: usize = 50;
```

with:

```rust
/// Number of cheapest listings to fetch per query before craftability filtering.
/// Set to the practical search-result cap so the whole constrained result is
/// considered and craft-tier comparables in the tail aren't crowded out by the
/// junk floor. `gather_comparables` fetches `min(result, limit)`, so smaller
/// result sets cost no more; only the BroadMarket fallback (no craft-tier base in
/// the whole result) prices broadly, and that path is already low-confidence + labelled.
const COMPARABLE_SAMPLE: usize = 100;
```

- [ ] **Step 2: Verify the suite is unaffected**

Run: `cargo build` (zero warnings) then `cargo test`
Expected: PASS — the filter/fallback/estimate tests pass an explicit `limit` and are sample-size-agnostic, so the const change doesn't affect them. (No dedicated test: a bare `assert_eq!(COMPARABLE_SAMPLE, 100)` would assert a constant against itself; the behavior is exercised by the existing craftability tests and the manual live paste.)

- [ ] **Step 3: Format, lint, commit**

```bash
cargo fmt && cargo clippy
git add src/trade/mod.rs
git commit -m "perf(trade): fetch whole constrained result (sample 50->100) before craftability filter"
# + trailer
```

---

## Final verification (after both tasks)

- [ ] `cargo fmt --check` clean; `cargo clippy` clean; `cargo test` green; `cargo build` zero warnings.
- [ ] **Manual live acceptance:** re-paste the reference boot (no hybrids → unchanged) and a **hybrid-affix** item; confirm the embed's "Priced as · N open prefixes" reflects affix blocks. Capture one live `fetch` JSON and note which `affix_count` layer fires (confirms whether `extended` is present on trade2 fetch responses).
