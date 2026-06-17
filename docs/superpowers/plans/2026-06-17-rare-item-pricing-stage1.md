# Rare-Item Pricing — Stage 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Price pasted rare/magic PoE2 items on demand by ablating live `trade2` queries, returning a price estimate + an on-demand characteristic breakdown, while logging every probe as future training data.

**Architecture:** A new isolated `src/trade/` module (parallel to `poeninja/`) owns everything trade-related: domain types, a pseudo-mod resolver, a query builder, a rate-limit-aware HTTP client behind a `TradeApi` trait, and an ablation engine behind a `Comparables` trait. The clipboard parser (`itemtext.rs`) is expanded to a full item. Discord (`paste.rs`) calls a `TradePricer` for rares; everything else is unchanged. Anonymous reads by default; an optional `POE_SESSID` raises rate limits.

**Tech Stack:** Rust 2021, tokio, reqwest (rustls), serde/serde_json, poise/serenity 0.12, anyhow, tracing, async-trait (new), tempfile (new dev-dep).

**Spec:** `docs/superpowers/specs/2026-06-17-rare-item-pricing-stage1-design.md`.

**Branch:** `feat/rare-pricing-stage1` (already checked out; the spec is committed there).

---

## File Structure

| File | Responsibility | Action |
|---|---|---|
| `Cargo.toml` | add `async-trait`; add `[dev-dependencies] tempfile` | modify |
| `src/itemtext.rs` | full-item clipboard parse → rich `ParsedItem` | expand |
| `src/trade/mod.rs` | module root + `TradePricer` (high-level orchestrator) | create |
| `src/trade/model.rs` | domain types (`TradeQuery`, `Listing`, `PriceEstimate`, `Breakdown`, `Probe`, …) | create |
| `src/trade/pseudo.rs` | stat → pseudo mapping + resolver | create |
| `src/trade/data/pseudo_map.json` | committed pseudo seed data | create |
| `src/trade/query.rs` | `ParsedItem` → `TradeQuery` → trade2 wire JSON | create |
| `src/trade/client.rs` | `TradeApi` trait, HTTP `TradeClient`, rate-limit parsing, `Comparables` impl | create |
| `src/trade/ablation.rs` | `Comparables` trait, `estimate`, `breakdown`, `gather_comparables` | create |
| `src/pricelog.rs` | append-only JSONL probe log | create |
| `src/store.rs` | `route()` returns `MatchOutcome::Rare` for rare/magic | modify |
| `src/config.rs` | optional `POE_SESSID` | modify |
| `src/discord/mod.rs` | add `pricer` to `Data` | modify |
| `src/discord/paste.rs` | rare branch: estimate embed + "Break it down" button | modify |
| `src/discord/embeds.rs` | `estimate_embed`, `breakdown_embed` + string helpers | modify |
| `src/main.rs` | construct `TradeClient`/`TradePricer`; `mod trade;` `mod pricelog;` | modify |
| `.env.example`, `CLAUDE.md` | document `POE_SESSID` + pseudo-map maintenance | modify |

### Shared types (canonical signatures — keep consistent across tasks)

Defined in `src/trade/model.rs` (Task 2) and `src/itemtext.rs` (Task 3):

```rust
// itemtext.rs
pub struct ItemStat { pub raw: String, pub value: Option<f64> }
pub struct ParsedItem {
    pub rarity: Rarity, pub name: String, pub base_type: Option<String>,
    pub item_class: Option<String>, pub item_level: Option<u32>, pub quality: Option<u32>,
    pub corrupted: bool,
    pub implicits: Vec<ItemStat>, pub enchants: Vec<ItemStat>,
    pub runes: Vec<ItemStat>, pub explicits: Vec<ItemStat>,
}

// trade/model.rs
pub enum Currency { Chaos, Exalted, Divine, Other(String) }
pub struct Money { pub amount: f64, pub currency: Currency }
pub struct Listing { pub price: Money, pub price_divine: f64 }
pub struct StatFilter { pub id: String, pub label: String, pub min: Option<f64>, pub max: Option<f64> }
pub struct MiscFilters { pub item_level_min: Option<u32>, pub quality_min: Option<u32>, pub corrupted: Option<bool> }
pub struct TradeQuery { pub league: String, pub category: Option<String>, pub type_line: Option<String>, pub stats: Vec<StatFilter>, pub misc: MiscFilters }
pub struct SearchResponse { pub id: String, pub total: u64, pub hashes: Vec<String> }
pub enum Confidence { High, Medium, Low }
pub struct PriceEstimate { pub low: f64, pub typical: f64, pub high: f64, pub listing_count: usize, pub confidence: Confidence }
pub enum AblationKind { Drop, Relax }
pub struct Contribution { pub characteristic: String, pub kind: AblationKind, pub delta_divine: f64 }
pub struct SynergyNote { pub a: String, pub b: String, pub extra_divine: f64 }
pub struct Breakdown { pub baseline: PriceEstimate, pub ranked: Vec<Contribution>, pub synergy: Option<SynergyNote>, pub trade_url: String }
pub struct Probe { pub query: TradeQuery, pub listing_count: usize, pub typical_divine: f64 }
```

All amounts in `*_divine` are normalized to Divine Orbs (the common unit). `Listing.price_divine` is computed at fetch time by the client.

> **Assumption flag:** the exact `trade2` request/response JSON (Tasks 6–7) is taken from community knowledge and **confirmed by the `#[ignore]`d live smoke test** during implementation. If the live test reveals different field names, adjust `to_payload`/`parse_fetch` only — no other task changes.

---

## Task 1: Dependencies + module skeleton

**Files:**
- Modify: `Cargo.toml`
- Create: `src/trade/mod.rs`
- Create: `src/pricelog.rs`
- Modify: `src/main.rs` (module declarations)

- [ ] **Step 1: Add dependencies**

In `Cargo.toml`, add to `[dependencies]`:

```toml
async-trait = "0.1"
```

Append a new section at the end of the file:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Create the trade module root**

Create `src/trade/mod.rs`:

```rust
//! On-demand rare-item pricing via live trade2 ablation. Isolated from
//! `poeninja`/`store`: data flows discord → trade, never sideways.

pub mod ablation;
pub mod client;
pub mod model;
pub mod pseudo;
pub mod query;
```

These submodules are created in later tasks; create empty placeholders now so the crate compiles:

Create `src/trade/model.rs`, `src/trade/pseudo.rs`, `src/trade/query.rs`, `src/trade/client.rs`, `src/trade/ablation.rs`, each containing only:

```rust
// filled in a later task
```

- [ ] **Step 3: Create the pricelog placeholder**

Create `src/pricelog.rs`:

```rust
// filled in Task 9
```

- [ ] **Step 4: Declare the modules in the crate**

In `src/main.rs`, add to the module declarations near the other `mod` lines (after `mod poeninja;`):

```rust
mod pricelog;
mod trade;
```

- [ ] **Step 5: Build to verify it compiles**

Run: `cargo build`
Expected: compiles (warnings about unused modules are fine).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml src/trade src/pricelog.rs src/main.rs
git commit -m "chore(trade): scaffold trade module + deps"
```

---

## Task 2: Domain types (`trade/model.rs`)

**Files:**
- Modify: `src/trade/model.rs`

- [ ] **Step 1: Write the failing test**

Put this at the bottom of `src/trade/model.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_to_divine_field_is_independent_of_currency() {
        let l = Listing {
            price: Money { amount: 5.0, currency: Currency::Exalted },
            price_divine: 0.5,
        };
        assert_eq!(l.price_divine, 0.5);
        assert!(matches!(l.price.currency, Currency::Exalted));
    }

    #[test]
    fn confidence_from_count_buckets() {
        assert_eq!(Confidence::from_count(15), Confidence::High);
        assert_eq!(Confidence::from_count(5), Confidence::Medium);
        assert_eq!(Confidence::from_count(1), Confidence::Low);
    }
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test --lib trade::model`
Expected: FAIL — types not defined.

- [ ] **Step 3: Implement the types**

Replace the contents of `src/trade/model.rs` (above the test module) with:

```rust
//! Domain types for trade pricing. Amounts in `*_divine` are normalized to
//! Divine Orbs, the common comparison unit.

#[derive(Clone, Debug, PartialEq)]
pub enum Currency {
    Chaos,
    Exalted,
    Divine,
    Other(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Money {
    pub amount: f64,
    pub currency: Currency,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Listing {
    pub price: Money,
    /// Price normalized to Divine Orbs for comparison/ranking.
    pub price_divine: f64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct StatFilter {
    /// trade2 stat id, e.g. "explicit.stat_..." or "pseudo.pseudo_total_elemental_resistance".
    pub id: String,
    /// Human label for the breakdown UI.
    pub label: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct MiscFilters {
    pub item_level_min: Option<u32>,
    pub quality_min: Option<u32>,
    pub corrupted: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TradeQuery {
    pub league: String,
    /// trade2 category, e.g. "weapon.staff".
    pub category: Option<String>,
    /// Exact base type ("type"), e.g. "Expert Crackling Staff".
    pub type_line: Option<String>,
    pub stats: Vec<StatFilter>,
    pub misc: MiscFilters,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchResponse {
    pub id: String,
    pub total: u64,
    pub hashes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    /// High ≥ 10 listings, Medium ≥ 3, else Low.
    pub fn from_count(n: usize) -> Self {
        if n >= 10 {
            Confidence::High
        } else if n >= 3 {
            Confidence::Medium
        } else {
            Confidence::Low
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PriceEstimate {
    pub low: f64,
    pub typical: f64,
    pub high: f64,
    pub listing_count: usize,
    pub confidence: Confidence,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AblationKind {
    Drop,
    Relax,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Contribution {
    pub characteristic: String,
    pub kind: AblationKind,
    /// How many divine the price drops when this characteristic is removed/relaxed.
    pub delta_divine: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SynergyNote {
    pub a: String,
    pub b: String,
    /// Extra divine beyond the sum of the two individual contributions.
    pub extra_divine: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Breakdown {
    pub baseline: PriceEstimate,
    pub ranked: Vec<Contribution>,
    pub synergy: Option<SynergyNote>,
    pub trade_url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    pub query: TradeQuery,
    pub listing_count: usize,
    pub typical_divine: f64,
}
```

- [ ] **Step 4: Run the test to confirm it passes**

Run: `cargo test --lib trade::model`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/trade/model.rs
git commit -m "feat(trade): domain types"
```

---

## Task 3: Parser — scalar item properties

Expand `ParsedItem` with the full-item fields and parse the scalar ones (class, item level, quality, corrupted). Mods come in Task 4. Existing tests must keep passing.

**Files:**
- Modify: `src/itemtext.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/itemtext.rs`:

```rust
    const RARE_STAFF: &str = "Item Class: Staves\nRarity: Rare\nBramble Bite\nExpert Crackling Staff\n--------\nQuality: +20% (augmented)\n--------\nItem Level: 82\n--------\n+7 to Level of all Spell Skills\n--------\nCorrupted\n";

    #[test]
    fn parses_scalar_properties() {
        let p = parse(RARE_STAFF).unwrap();
        assert_eq!(p.rarity, Rarity::Rare);
        assert_eq!(p.name, "Bramble Bite");
        assert_eq!(p.base_type.as_deref(), Some("Expert Crackling Staff"));
        assert_eq!(p.item_class.as_deref(), Some("Staves"));
        assert_eq!(p.item_level, Some(82));
        assert_eq!(p.quality, Some(20));
        assert!(p.corrupted);
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test --lib itemtext::tests::parses_scalar_properties`
Expected: FAIL — fields don't exist.

- [ ] **Step 3: Extend `ItemStat` + `ParsedItem` and parse scalars**

In `src/itemtext.rs`, add the `ItemStat` struct after `ParsedItem` is declared, change `ParsedItem`'s derive (drop `Eq` — `f64` rolls aren't `Eq`), and add the new fields. Replace the `ParsedItem` struct with:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedItem {
    pub rarity: Rarity,
    pub name: String,
    pub base_type: Option<String>,
    pub item_class: Option<String>,
    pub item_level: Option<u32>,
    pub quality: Option<u32>,
    pub corrupted: bool,
    pub implicits: Vec<ItemStat>,
    pub enchants: Vec<ItemStat>,
    pub runes: Vec<ItemStat>,
    pub explicits: Vec<ItemStat>,
}

/// One stat line from the clipboard, with the first numeric roll extracted.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemStat {
    pub raw: String,
    pub value: Option<f64>,
}
```

Add these helpers above `parse`:

```rust
/// Extracts the first signed decimal number from a stat line, e.g.
/// "+7 to Level of all Spell Skills" -> 7.0, "12.5% increased ..." -> 12.5.
pub fn first_number(s: &str) -> Option<f64> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_digit() || ((c == '-' || c == '+') && i + 1 < bytes.len() && (bytes[i + 1] as char).is_ascii_digit())
        {
            let start = i;
            if c == '-' || c == '+' {
                i += 1;
            }
            while i < bytes.len() && ((bytes[i] as char).is_ascii_digit() || bytes[i] as char == '.') {
                i += 1;
            }
            return s[start..i].trim_start_matches('+').parse::<f64>().ok();
        }
        i += 1;
    }
    None
}

/// Reads the integer after a "Label:" prefix on the matching line.
fn labeled_u32(lines: &[&str], label: &str) -> Option<u32> {
    lines
        .iter()
        .find(|l| l.starts_with(label))
        .and_then(|l| first_number(l))
        .map(|n| n as u32)
}
```

Now update `parse` to fill the new fields. Replace the final `Some(ParsedItem { ... })` block with:

```rust
    let item_class = lines
        .iter()
        .find(|l| l.starts_with("Item Class:"))
        .map(|l| l.trim_start_matches("Item Class:").trim().to_string())
        .filter(|s| !s.is_empty());
    let item_level = labeled_u32(&lines, "Item Level:");
    let quality = labeled_u32(&lines, "Quality:");
    let corrupted = lines.iter().any(|l| *l == "Corrupted");

    Some(ParsedItem {
        rarity,
        name,
        base_type,
        item_class,
        item_level,
        quality,
        corrupted,
        implicits: Vec::new(),
        enchants: Vec::new(),
        runes: Vec::new(),
        explicits: Vec::new(),
    })
```

- [ ] **Step 4: Run the parser tests to confirm all pass**

Run: `cargo test --lib itemtext`
Expected: PASS — the new test and all pre-existing parser tests.

- [ ] **Step 5: Commit**

```bash
git add src/itemtext.rs
git commit -m "feat(itemtext): parse full-item scalar properties"
```

---

## Task 4: Parser — mod classification

Classify mod lines into implicits / enchants / runes / explicits using the clipboard's section tags. PoE2 tags lines with suffixes like `(implicit)`, `(enchant)`, `(rune)`; everything else that looks like a mod and isn't a property/header line is an explicit.

**Files:**
- Modify: `src/itemtext.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    const RARE_RING: &str = "Item Class: Rings\nRarity: Rare\nWoe Coil\nSapphire Ring\n--------\nRequirements:\nLevel: 60\n--------\n+25 to maximum Mana (implicit)\n--------\n+40 to maximum Life\n+32% to Fire Resistance\n+18% to Lightning Resistance\n+12% increased Rarity of Items found (rune)\n--------\nItem Level: 80\n";

    #[test]
    fn classifies_mods_by_section_tag() {
        let p = parse(RARE_RING).unwrap();
        assert_eq!(p.implicits.len(), 1);
        assert_eq!(p.implicits[0].value, Some(25.0));
        assert_eq!(p.runes.len(), 1);
        assert_eq!(p.runes[0].value, Some(12.0));
        // life + 2 resists, rune line excluded, implicit excluded
        assert_eq!(p.explicits.len(), 3);
        let fire = p.explicits.iter().find(|s| s.raw.contains("Fire Resistance")).unwrap();
        assert_eq!(fire.value, Some(32.0));
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test --lib itemtext::tests::classifies_mods_by_section_tag`
Expected: FAIL — mod vectors are empty.

- [ ] **Step 3: Implement mod classification**

Add this helper above `parse`:

```rust
/// True for lines that are headers/properties/requirements rather than mods.
fn is_meta_line(l: &str) -> bool {
    const PREFIXES: [&str; 9] = [
        "Item Class:", "Rarity:", "Requirements:", "Level:", "Item Level:",
        "Quality:", "Sockets:", "Stack Size:", "Str", // "Strength", "Dex"/"Int" reqs start with these
    ];
    l.is_empty()
        || is_separator(l)
        || l == "Corrupted"
        || l == "Unidentified"
        || PREFIXES.iter().any(|p| l.starts_with(p))
}

/// Strips a trailing parenthetical tag like " (implicit)" and returns
/// (clean_text, tag). Tag is lowercased; None if no recognized tag.
fn split_tag(l: &str) -> (String, Option<String>) {
    if let Some(open) = l.rfind('(') {
        if l.ends_with(')') {
            let tag = l[open + 1..l.len() - 1].to_lowercase();
            let clean = l[..open].trim().to_string();
            return (clean, Some(tag));
        }
    }
    (l.to_string(), None)
}
```

In `parse`, after computing `corrupted` and before the `Some(ParsedItem { .. })`, build the mod vectors. The name/base_type lines (`idx + 1`, `idx + 2`) must be excluded from mods:

```rust
    let mut implicits = Vec::new();
    let mut enchants = Vec::new();
    let mut runes = Vec::new();
    let mut explicits = Vec::new();

    for (i, raw_line) in lines.iter().enumerate() {
        if i == idx || i == idx + 1 || i == idx + 2 {
            continue; // rarity, name, base type
        }
        if is_meta_line(raw_line) {
            continue;
        }
        let (clean, tag) = split_tag(raw_line);
        let stat = ItemStat {
            value: first_number(&clean),
            raw: clean,
        };
        match tag.as_deref() {
            Some("implicit") => implicits.push(stat),
            Some("enchant") => enchants.push(stat),
            Some("rune") => runes.push(stat),
            _ => explicits.push(stat),
        }
    }
```

Then change the struct literal's four `Vec::new()` lines to use the locals:

```rust
        implicits,
        enchants,
        runes,
        explicits,
```

- [ ] **Step 4: Run the parser tests to confirm all pass**

Run: `cargo test --lib itemtext`
Expected: PASS — new test plus all earlier ones (the scalar test's `+7 to Level of all Spell Skills` line lands in `explicits`, which it doesn't assert, so it stays green).

- [ ] **Step 5: Commit**

```bash
git add src/itemtext.rs
git commit -m "feat(itemtext): classify implicits/enchants/runes/explicits"
```

---

## Task 5: Pseudo-mod resolver (`trade/pseudo.rs` + data)

**Files:**
- Create: `src/trade/data/pseudo_map.json`
- Modify: `src/trade/pseudo.rs`

- [ ] **Step 1: Create the seed data**

Create `src/trade/data/pseudo_map.json`:

```json
[
  {
    "pseudo_id": "pseudo.pseudo_total_elemental_resistance",
    "label": "Total Elemental Resistance",
    "patterns": ["to Fire Resistance", "to Cold Resistance", "to Lightning Resistance", "to Fire and Lightning Resistance", "to Fire and Cold Resistance", "to Cold and Lightning Resistance", "to all Elemental Resistances"]
  },
  {
    "pseudo_id": "pseudo.pseudo_total_resistance",
    "label": "Total Resistance (incl. Chaos)",
    "patterns": ["to Fire Resistance", "to Cold Resistance", "to Lightning Resistance", "to Fire and Lightning Resistance", "to Fire and Cold Resistance", "to Cold and Lightning Resistance", "to all Elemental Resistances", "to Chaos Resistance"]
  },
  {
    "pseudo_id": "pseudo.pseudo_total_life",
    "label": "Total maximum Life",
    "patterns": ["to maximum Life"]
  },
  {
    "pseudo_id": "pseudo.pseudo_total_attributes",
    "label": "Total Attributes",
    "patterns": ["to Strength", "to Dexterity", "to Intelligence", "to all Attributes"]
  },
  {
    "pseudo_id": "pseudo.pseudo_adds_to_all_spell_skills",
    "label": "+# to Level of all Spell Skills",
    "patterns": ["to Level of all Spell Skills"]
  }
]
```

- [ ] **Step 2: Write the failing test**

Put in `src/trade/pseudo.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::itemtext::ItemStat;

    fn stat(raw: &str, v: f64) -> ItemStat {
        ItemStat { raw: raw.to_string(), value: Some(v) }
    }

    #[test]
    fn sums_elemental_resistances_across_lines() {
        let map = PseudoMap::load();
        let stats = vec![
            stat("+32% to Fire Resistance", 32.0),
            stat("+18% to Lightning Resistance", 18.0),
            stat("+40 to maximum Life", 40.0),
        ];
        let resolved = map.resolve(&stats);
        let ele = resolved.iter().find(|p| p.id == "pseudo.pseudo_total_elemental_resistance").unwrap();
        assert_eq!(ele.total, 50.0);
        let life = resolved.iter().find(|p| p.id == "pseudo.pseudo_total_life").unwrap();
        assert_eq!(life.total, 40.0);
    }

    #[test]
    fn omits_pseudos_with_no_matching_lines() {
        let map = PseudoMap::load();
        let resolved = map.resolve(&[stat("+10 to Strength", 10.0)]);
        assert!(resolved.iter().all(|p| p.id != "pseudo.pseudo_total_life"));
        assert!(resolved.iter().any(|p| p.id == "pseudo.pseudo_total_attributes"));
    }
}
```

- [ ] **Step 3: Run it to confirm it fails**

Run: `cargo test --lib trade::pseudo`
Expected: FAIL — `PseudoMap` undefined.

- [ ] **Step 4: Implement the resolver**

Put above the test module in `src/trade/pseudo.rs`:

```rust
//! Maps individual stat lines into market "pseudo" aggregates (e.g. total
//! elemental resistance), which is how buyers actually search. Seeded from
//! `data/pseudo_map.json`; re-check each major patch.

use serde::Deserialize;

use crate::itemtext::ItemStat;

#[derive(Debug, Clone, Deserialize)]
pub struct PseudoRule {
    pub pseudo_id: String,
    pub label: String,
    /// Substrings; a stat line matching any contributes its value to the sum.
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PseudoMap {
    pub rules: Vec<PseudoRule>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PseudoStat {
    pub id: String,
    pub label: String,
    pub total: f64,
}

impl PseudoMap {
    /// Loads the committed seed map. Panics only on a malformed committed file
    /// (a build-time bug, caught by tests), never at runtime on user input.
    pub fn load() -> Self {
        let rules: Vec<PseudoRule> =
            serde_json::from_str(include_str!("data/pseudo_map.json"))
                .expect("pseudo_map.json is valid");
        PseudoMap { rules }
    }

    /// Sums each pseudo over all stat lines that match its patterns. A pseudo
    /// with no matching lines yields total 0.0 (still returned, so callers can
    /// see it is available); callers filter by `total > 0` when needed.
    pub fn resolve(&self, stats: &[ItemStat]) -> Vec<PseudoStat> {
        self.rules
            .iter()
            .map(|rule| {
                let total = stats
                    .iter()
                    .filter(|s| rule.patterns.iter().any(|p| s.raw.contains(p.as_str())))
                    .filter_map(|s| s.value)
                    .sum();
                PseudoStat {
                    id: rule.pseudo_id.clone(),
                    label: rule.label.clone(),
                    total,
                }
            })
            .collect()
    }
}
```

- [ ] **Step 5: Run the test to confirm it passes**

Run: `cargo test --lib trade::pseudo`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add src/trade/pseudo.rs src/trade/data/pseudo_map.json
git commit -m "feat(trade): pseudo-mod resolver + seed map"
```

---

## Task 6: Query builder (`trade/query.rs`)

Build a `TradeQuery` from a `ParsedItem`, preferring pseudo aggregates for fungible groups (resistances), and serialize it to the trade2 wire JSON.

**Files:**
- Modify: `src/trade/query.rs`

- [ ] **Step 1: Write the failing test**

Put in `src/trade/query.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::itemtext::{ItemStat, ParsedItem, Rarity};
    use crate::trade::pseudo::PseudoMap;

    fn ring() -> ParsedItem {
        ParsedItem {
            rarity: Rarity::Rare,
            name: "Woe Coil".into(),
            base_type: Some("Sapphire Ring".into()),
            item_class: Some("Rings".into()),
            item_level: Some(80),
            quality: None,
            corrupted: false,
            implicits: vec![],
            enchants: vec![],
            runes: vec![],
            explicits: vec![
                ItemStat { raw: "+40 to maximum Life".into(), value: Some(40.0) },
                ItemStat { raw: "+32% to Fire Resistance".into(), value: Some(32.0) },
                ItemStat { raw: "+18% to Lightning Resistance".into(), value: Some(18.0) },
            ],
        }
    }

    #[test]
    fn baseline_prefers_pseudo_resistance_over_individual_lines() {
        let q = build_baseline(&ring(), &PseudoMap::load(), "Standard");
        assert_eq!(q.league, "Standard");
        assert_eq!(q.type_line.as_deref(), Some("Sapphire Ring"));
        // total ele res pseudo present with min = 50
        let ele = q.stats.iter().find(|s| s.id == "pseudo.pseudo_total_elemental_resistance").unwrap();
        assert_eq!(ele.min, Some(50.0));
        // individual resist lines NOT added as separate stat filters
        assert!(!q.stats.iter().any(|s| s.label.contains("Fire Resistance")));
        // non-fungible life kept as its own pseudo filter
        assert!(q.stats.iter().any(|s| s.id == "pseudo.pseudo_total_life" && s.min == Some(40.0)));
    }

    #[test]
    fn payload_has_status_type_and_sort() {
        let q = build_baseline(&ring(), &PseudoMap::load(), "Standard");
        let payload = to_payload(&q);
        assert_eq!(payload["query"]["status"]["option"], "online");
        assert_eq!(payload["query"]["type"], "Sapphire Ring");
        assert_eq!(payload["sort"]["price"], "asc");
    }
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test --lib trade::query`
Expected: FAIL — `build_baseline`/`to_payload` undefined.

- [ ] **Step 3: Implement the builder**

Put above the test module in `src/trade/query.rs`:

```rust
//! Builds a `TradeQuery` from a parsed item (pseudo-preferred for fungible
//! groups) and serializes it to the trade2 search payload.

use serde_json::{json, Value};

use crate::itemtext::ParsedItem;
use crate::trade::model::{MiscFilters, StatFilter, TradeQuery};
use crate::trade::pseudo::PseudoMap;

/// Pseudo ids that represent fungible groups whose individual lines we suppress
/// in favor of the aggregate.
const FUNGIBLE_PSEUDOS: [&str; 3] = [
    "pseudo.pseudo_total_elemental_resistance",
    "pseudo.pseudo_total_life",
    "pseudo.pseudo_total_attributes",
];

pub fn build_baseline(item: &ParsedItem, pseudo: &PseudoMap, league: &str) -> TradeQuery {
    let all_stats: Vec<_> = item
        .implicits
        .iter()
        .chain(&item.enchants)
        .chain(&item.runes)
        .chain(&item.explicits)
        .cloned()
        .collect();

    let mut stats: Vec<StatFilter> = Vec::new();

    // Pseudo aggregates with a positive total become min-bounded filters.
    for ps in pseudo.resolve(&all_stats) {
        if ps.total > 0.0 {
            stats.push(StatFilter {
                id: ps.id,
                label: ps.label,
                min: Some(ps.total),
                max: None,
            });
        }
    }

    TradeQuery {
        league: league.to_string(),
        category: None, // category inference deferred (needs a base→category table)
        type_line: item.base_type.clone(),
        stats,
        misc: MiscFilters {
            item_level_min: item.item_level,
            quality_min: item.quality,
            corrupted: Some(item.corrupted),
        },
    }
}

/// True if a pseudo id is one of the fungible aggregates (kept for callers that
/// want to drill from aggregate to constituent in the breakdown).
pub fn is_fungible(pseudo_id: &str) -> bool {
    FUNGIBLE_PSEUDOS.contains(&pseudo_id)
}

/// Serializes a `TradeQuery` to the trade2 search request body.
///
/// Assumption (confirmed by the live smoke test in Task 7): trade2 expects
/// `{ query: { status, type, filters: { type_filters, misc_filters }, stats }, sort }`.
pub fn to_payload(q: &TradeQuery) -> Value {
    let stat_filters: Vec<Value> = q
        .stats
        .iter()
        .map(|s| {
            let mut value = json!({});
            if let Some(m) = s.min {
                value["min"] = json!(m);
            }
            if let Some(m) = s.max {
                value["max"] = json!(m);
            }
            json!({ "id": s.id, "value": value })
        })
        .collect();

    let mut type_filters = json!({});
    if let Some(c) = &q.category {
        type_filters["category"] = json!({ "option": c });
    }
    if let Some(min) = q.misc.item_level_min {
        type_filters["ilvl"] = json!({ "min": min });
    }
    if let Some(min) = q.misc.quality_min {
        type_filters["quality"] = json!({ "min": min });
    }

    let mut misc_filters = json!({});
    if let Some(c) = q.misc.corrupted {
        misc_filters["corrupted"] = json!({ "option": c });
    }

    let mut query = json!({
        "status": { "option": "online" },
        "stats": [ { "type": "and", "filters": stat_filters } ],
        "filters": {
            "type_filters": { "filters": type_filters },
            "misc_filters": { "filters": misc_filters },
        }
    });
    if let Some(t) = &q.type_line {
        query["type"] = json!(t);
    }

    json!({ "query": query, "sort": { "price": "asc" } })
}
```

- [ ] **Step 4: Run the test to confirm it passes**

Run: `cargo test --lib trade::query`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/trade/query.rs
git commit -m "feat(trade): pseudo-preferred query builder + wire payload"
```

---

## Task 7: HTTP client + rate-limit parsing (`trade/client.rs`)

Define the `TradeApi` trait, the rate-limit header parser (pure, unit-tested), and the HTTP `TradeClient`. The live search/fetch round-trip is exercised only by an `#[ignore]`d smoke test.

**Files:**
- Modify: `src/trade/client.rs`

- [ ] **Step 1: Write the failing test (rate-limit parser)**

Put in `src/trade/client.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rate_limit_rule_triples() {
        // "max:period:restriction" — 5 hits per 10s, 60s restriction.
        let rules = parse_rate_rules("5:10:60,15:60:120");
        assert_eq!(rules, vec![RateRule { max: 5, period: 10, restriction: 60 }, RateRule { max: 15, period: 60, restriction: 120 }]);
    }

    #[test]
    fn backoff_is_zero_when_under_limit_and_period_when_at_limit() {
        let rule = RateRule { max: 5, period: 10, restriction: 60 };
        assert_eq!(backoff_secs(&[rule.clone()], 3), 0);
        assert_eq!(backoff_secs(&[rule], 5), 10);
    }

    #[test]
    fn retry_after_prefers_retry_after_header() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, "12".parse().unwrap());
        assert_eq!(retry_after_secs(&h), 12);
    }

    #[test]
    fn retry_after_falls_back_to_rule_period() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("X-Rate-Limit-Ip", "5:10:60".parse().unwrap());
        assert_eq!(retry_after_secs(&h), 10);
    }

    #[tokio::test]
    #[ignore = "hits the live trade2 API"]
    async fn live_search_fetch_smoke() {
        use crate::trade::model::{MiscFilters, TradeQuery};
        let client = TradeClient::new(None).unwrap();
        let q = TradeQuery {
            league: live_league().await,
            category: None,
            type_line: Some("Sapphire Ring".into()),
            stats: vec![],
            misc: MiscFilters::default(),
        };
        let resp = client.search(&q).await.unwrap();
        assert!(resp.total > 0);
        let listings = client.fetch(&resp.id, &resp.hashes[..resp.hashes.len().min(5)]).await.unwrap();
        assert!(!listings.is_empty());
        assert!(listings.iter().all(|l| l.price_divine >= 0.0));
    }

    #[cfg(test)]
    async fn live_league() -> String {
        // The smoke test needs a real league name; reuse poe.ninja detection.
        let nc = crate::poeninja::NinjaClient::new().unwrap();
        nc.current_league().await.unwrap().name
    }
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test --lib trade::client`
Expected: FAIL — `parse_rate_rules`/`RateRule`/`backoff_secs`/`TradeClient` undefined.

- [ ] **Step 3: Implement the client**

Put above the test module in `src/trade/client.rs`:

```rust
//! trade2 HTTP client behind the `TradeApi` trait, with rate-limit-header
//! parsing. Anonymous by default; an optional POESESSID raises the ceiling.

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::{header, Client};
use serde_json::Value;

use crate::trade::model::{Currency, Listing, Money, SearchResponse, TradeQuery};
use crate::trade::query::to_payload;

const TRADE_BASE: &str = "https://www.pathofexile.com/api/trade2";
const USER_AGENT: &str =
    "dr-peste-redux/0.1 (Discord guild price bot; not affiliated with Grinding Gear Games)";

/// Divine-Orb conversion rates. v1 defaults; refreshing from the live currency
/// market is a later refinement.
#[derive(Clone, Debug)]
pub struct CurrencyRates {
    pub exalted_per_divine: f64,
    pub chaos_per_divine: f64,
}

impl Default for CurrencyRates {
    fn default() -> Self {
        // Conservative placeholders; overridden once live currency data is wired.
        CurrencyRates { exalted_per_divine: 180.0, chaos_per_divine: 2000.0 }
    }
}

impl CurrencyRates {
    pub fn to_divine(&self, m: &Money) -> f64 {
        match m.currency {
            Currency::Divine => m.amount,
            Currency::Exalted => m.amount / self.exalted_per_divine,
            Currency::Chaos => m.amount / self.chaos_per_divine,
            Currency::Other(_) => 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RateRule {
    pub max: u32,
    pub period: u32,
    pub restriction: u32,
}

/// Parses an `X-Rate-Limit-*` value: comma-separated `max:period:restriction`.
pub fn parse_rate_rules(header_value: &str) -> Vec<RateRule> {
    header_value
        .split(',')
        .filter_map(|triple| {
            let mut it = triple.split(':');
            Some(RateRule {
                max: it.next()?.trim().parse().ok()?,
                period: it.next()?.trim().parse().ok()?,
                restriction: it.next()?.trim().parse().ok()?,
            })
        })
        .collect()
}

/// Seconds to wait before the next request given the strictest rule and the
/// current hit count in the window. 0 while under any limit.
pub fn backoff_secs(rules: &[RateRule], current_hits: u32) -> u64 {
    rules
        .iter()
        .filter(|r| current_hits >= r.max)
        .map(|r| r.period as u64)
        .max()
        .unwrap_or(0)
}

/// Seconds to wait after a 429: the standard `Retry-After` header if present,
/// else the largest period from the rate-limit rule headers, clamped to [1,120].
pub fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> u64 {
    if let Some(v) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
    {
        return v.clamp(1, 120);
    }
    for name in ["X-Rate-Limit-Ip", "X-Rate-Limit-Account"] {
        if let Some(period) = headers
            .get(name)
            .and_then(|h| h.to_str().ok())
            .and_then(|v| parse_rate_rules(v).into_iter().map(|r| r.period as u64).max())
        {
            return period.clamp(1, 120);
        }
    }
    5
}

#[async_trait]
pub trait TradeApi {
    async fn search(&self, query: &TradeQuery) -> Result<SearchResponse>;
    async fn fetch(&self, query_id: &str, hashes: &[String]) -> Result<Vec<Listing>>;
}

pub struct TradeClient {
    http: Client,
    rates: CurrencyRates,
}

impl TradeClient {
    /// `poe_sessid` optional: when present it is sent as the POESESSID cookie to
    /// raise the rate-limit ceiling; otherwise requests are anonymous.
    pub fn new(poe_sessid: Option<String>) -> Result<Self> {
        let mut builder = Client::builder().user_agent(USER_AGENT);
        if let Some(sess) = poe_sessid.filter(|s| !s.is_empty()) {
            let mut headers = header::HeaderMap::new();
            let cookie = format!("POESESSID={sess}");
            headers.insert(header::COOKIE, header::HeaderValue::from_str(&cookie)?);
            builder = builder.default_headers(headers);
        }
        Ok(Self { http: builder.build()?, rates: CurrencyRates::default() })
    }

    fn parse_currency(s: &str) -> Currency {
        match s {
            "divine" => Currency::Divine,
            "exalted" => Currency::Exalted,
            "chaos" => Currency::Chaos,
            other => Currency::Other(other.to_string()),
        }
    }

    /// Parses a /fetch response body into listings. Assumption (smoke-verified):
    /// `{ result: [ { listing: { price: { amount, currency } } } ] }`.
    fn parse_fetch(&self, v: &Value) -> Vec<Listing> {
        v.get("result")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|entry| {
                        let price = entry.get("listing")?.get("price")?;
                        let amount = price.get("amount")?.as_f64()?;
                        let currency = Self::parse_currency(price.get("currency")?.as_str()?);
                        let money = Money { amount, currency };
                        let price_divine = self.rates.to_divine(&money);
                        Some(Listing { price: money, price_divine })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Sends a request, retrying up to twice on HTTP 429 after sleeping for the
    /// server-advised period. Other errors propagate immediately.
    async fn send_with_retry<F>(&self, build: F) -> Result<reqwest::Response>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut attempt = 0u32;
        loop {
            let resp = build().send().await?;
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < 2 {
                let wait = retry_after_secs(resp.headers());
                tracing::warn!(wait_secs = wait, "trade2 rate-limited; backing off");
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                attempt += 1;
                continue;
            }
            return Ok(resp.error_for_status()?);
        }
    }
}

#[async_trait]
impl TradeApi for TradeClient {
    async fn search(&self, query: &TradeQuery) -> Result<SearchResponse> {
        let url = format!("{TRADE_BASE}/search/{}", query.league);
        let payload = to_payload(query);
        let resp = self
            .send_with_retry(|| self.http.post(&url).json(&payload))
            .await
            .context("trade2 search failed")?;
        let v: Value = resp.json().await?;
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string();
        let total = v.get("total").and_then(|x| x.as_u64()).unwrap_or(0);
        let hashes = v
            .get("result")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|h| h.as_str().map(String::from)).collect())
            .unwrap_or_default();
        Ok(SearchResponse { id, total, hashes })
    }

    async fn fetch(&self, query_id: &str, hashes: &[String]) -> Result<Vec<Listing>> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let csv = hashes.join(",");
        let url = format!("{TRADE_BASE}/fetch/{csv}?query={query_id}");
        let v: Value = self
            .send_with_retry(|| self.http.get(&url))
            .await
            .context("trade2 fetch failed")?
            .json()
            .await?;
        Ok(self.parse_fetch(&v))
    }
}
```

- [ ] **Step 4: Run the unit tests to confirm they pass**

Run: `cargo test --lib trade::client`
Expected: PASS (4 non-ignored tests; the smoke test is skipped).

- [ ] **Step 5: Run the live smoke test manually (optional but recommended)**

Run: `cargo test --lib trade::client -- --ignored live_search_fetch_smoke`
Expected: PASS if the assumed trade2 shapes hold. **If it fails, fix only `to_payload` (Task 6) and `parse_fetch`/`search` here** to match the real JSON, then re-run. Record any field-name corrections in a commit.

- [ ] **Step 6: Commit**

```bash
git add src/trade/client.rs
git commit -m "feat(trade): trade2 client + rate-limit parsing"
```

---

## Task 8: Comparables + relax-until-k (`trade/ablation.rs`, part 1)

**Files:**
- Modify: `src/trade/ablation.rs`

- [ ] **Step 1: Write the failing test**

Put in `src/trade/ablation.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::{Currency, Listing, MiscFilters, Money, SearchResponse, StatFilter, TradeQuery};
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn listing(divine: f64) -> Listing {
        Listing { price: Money { amount: divine, currency: Currency::Divine }, price_divine: divine }
    }

    /// Fake low-level API: returns listings whose count/prices depend on how
    /// many stat filters the query still carries (more constraints → fewer,
    /// pricier listings). Records the queries it saw.
    struct FakeApi {
        seen: Mutex<Vec<TradeQuery>>,
    }

    #[async_trait]
    impl TradeApi for FakeApi {
        async fn search(&self, q: &TradeQuery) -> anyhow::Result<SearchResponse> {
            self.seen.lock().unwrap().push(q.clone());
            // count grows as constraints drop
            let n = 1 + (3usize.saturating_sub(q.stats.len())) * 4;
            let hashes = (0..n).map(|i| format!("h{i}")).collect::<Vec<_>>();
            Ok(SearchResponse { id: "qid".into(), total: n as u64, hashes })
        }
        async fn fetch(&self, _id: &str, hashes: &[String]) -> anyhow::Result<Vec<Listing>> {
            // base price 10 div, +5 per remaining hash beyond first (cheap→expensive)
            Ok(hashes.iter().enumerate().map(|(i, _)| listing(10.0 + i as f64)).collect())
        }
    }

    fn q_with(n_stats: usize) -> TradeQuery {
        TradeQuery {
            league: "Standard".into(),
            category: None,
            type_line: Some("Sapphire Ring".into()),
            stats: (0..n_stats)
                .map(|i| StatFilter { id: format!("s{i}"), label: format!("s{i}"), min: Some(10.0), max: None })
                .collect(),
            misc: MiscFilters::default(),
        }
    }

    #[tokio::test]
    async fn relaxes_until_min_listings_reached() {
        let api = FakeApi { seen: Mutex::new(vec![]) };
        // 3 stats → 1 listing (< k=5). Must relax (drop a stat) until ≥ 5.
        let got = gather_comparables(&api, &q_with(3), 5, 3).await.unwrap();
        assert!(got.len() >= 5);
    }
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test --lib trade::ablation`
Expected: FAIL — `gather_comparables`/`Comparables` undefined.

- [ ] **Step 3: Implement the gatherer + trait**

Put above the test module in `src/trade/ablation.rs`:

```rust
//! Ablation pricing: gather comparables (relaxing thin queries), estimate a
//! price, and break a price down into per-characteristic contributions.

use anyhow::Result;
use async_trait::async_trait;

use crate::trade::client::TradeApi;
use crate::trade::model::{Listing, TradeQuery};

/// High-level seam the pricer depends on. `TradeClient` implements it via
/// `gather_comparables`; tests fake it directly.
#[async_trait]
pub trait Comparables {
    async fn comparables(&self, query: &TradeQuery, limit: usize) -> Result<Vec<Listing>>;
}

/// Searches + fetches up to `limit` cheapest listings. If fewer than `limit`
/// are found, relaxes the query (drops the last stat filter) and retries, up to
/// `max_relax` times. Returns whatever it has (possibly empty).
pub async fn gather_comparables<A: TradeApi + ?Sized>(
    api: &A,
    query: &TradeQuery,
    limit: usize,
    max_relax: usize,
) -> Result<Vec<Listing>> {
    let mut q = query.clone();
    let mut relaxations = 0;
    loop {
        let resp = api.search(&q).await?;
        let take = resp.hashes.len().min(limit);
        let mut listings = api.fetch(&resp.id, &resp.hashes[..take]).await?;
        listings.sort_by(|a, b| a.price_divine.partial_cmp(&b.price_divine).unwrap_or(std::cmp::Ordering::Equal));
        if listings.len() >= limit || relaxations >= max_relax || q.stats.is_empty() {
            return Ok(listings);
        }
        q.stats.pop(); // relax the loosest-to-add constraint
        relaxations += 1;
    }
}
```

Make `TradeClient` implement `Comparables`. Append to `src/trade/client.rs` (below the `impl TradeApi for TradeClient` block, outside the test module):

```rust
#[async_trait]
impl crate::trade::ablation::Comparables for TradeClient {
    async fn comparables(
        &self,
        query: &crate::trade::model::TradeQuery,
        limit: usize,
    ) -> anyhow::Result<Vec<crate::trade::model::Listing>> {
        crate::trade::ablation::gather_comparables(self, query, limit, 3).await
    }
}
```

- [ ] **Step 4: Run the test to confirm it passes**

Run: `cargo test --lib trade::ablation`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/trade/ablation.rs src/trade/client.rs
git commit -m "feat(trade): comparables trait + relax-until-k gatherer"
```

---

## Task 9: Estimate + breakdown (`trade/ablation.rs`, part 2)

**Files:**
- Modify: `src/trade/ablation.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/trade/ablation.rs`:

```rust
    use crate::trade::model::{AblationKind, Confidence};

    /// Fake high-level Comparables: maps a query to a fixed price based on which
    /// stat ids are present, so ablation deltas are deterministic.
    struct FakePricer;

    #[async_trait]
    impl Comparables for FakePricer {
        async fn comparables(&self, q: &TradeQuery, _limit: usize) -> anyhow::Result<Vec<Listing>> {
            // base 5; +10 if "spell" present; +2 if "crit" present; +6 extra if BOTH (synergy)
            let has_spell = q.stats.iter().any(|s| s.id.contains("spell"));
            let has_crit = q.stats.iter().any(|s| s.id.contains("crit"));
            let mut price = 5.0;
            if has_spell { price += 10.0; }
            if has_crit { price += 2.0; }
            if has_spell && has_crit { price += 6.0; }
            Ok(vec![listing(price); 12]) // 12 listings → High confidence
        }
    }

    fn two_stat_query() -> TradeQuery {
        TradeQuery {
            league: "Standard".into(),
            category: None,
            type_line: Some("Expert Crackling Staff".into()),
            stats: vec![
                StatFilter { id: "explicit.spell".into(), label: "+to all Spell Skills".into(), min: Some(7.0), max: None },
                StatFilter { id: "explicit.crit".into(), label: "Critical Chance".into(), min: Some(80.0), max: None },
            ],
            misc: MiscFilters::default(),
        }
    }

    #[tokio::test]
    async fn estimate_reports_typical_and_confidence() {
        let est = estimate(&FakePricer, &two_stat_query(), 10).await.unwrap();
        assert_eq!(est.listing_count, 12);
        assert_eq!(est.confidence, Confidence::High);
        // both stats present → 5+10+2+6 = 23
        assert_eq!(est.typical, 23.0);
    }

    #[tokio::test]
    async fn breakdown_ranks_contributions_and_flags_synergy() {
        let bd = breakdown(&FakePricer, &two_stat_query(), 10, 2).await.unwrap();
        // baseline 23; drop spell → 5+2 = 7 (delta 16); drop crit → 5+10 = 15 (delta 8)
        assert_eq!(bd.ranked[0].characteristic, "+to all Spell Skills");
        assert_eq!(bd.ranked[0].delta_divine, 16.0);
        assert_eq!(bd.ranked[0].kind, AblationKind::Drop);
        assert_eq!(bd.ranked[1].delta_divine, 8.0);
        // synergy: drop-both → 5 (delta 18). individual deltas sum 16+8=24.
        // extra = baseline - dropboth - (sum of (baseline - each_single))? See impl.
        let syn = bd.synergy.unwrap();
        assert_eq!(syn.extra_divine, 6.0);
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test --lib trade::ablation`
Expected: FAIL — `estimate`/`breakdown` undefined.

- [ ] **Step 3: Implement estimate + breakdown**

Add to `src/trade/ablation.rs` (above the test module):

```rust
use crate::trade::model::{
    AblationKind, Breakdown, Confidence, Contribution, PriceEstimate, SynergyNote,
};

/// Cheapest, typical (low-percentile), and high prices over the comparables,
/// all in divine. `typical` is the cheapest (asking-price floor) — the most
/// defensible single number for "what it sells for".
pub async fn estimate<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
) -> Result<PriceEstimate> {
    let listings = c.comparables(query, limit).await?;
    Ok(estimate_from(&listings))
}

fn estimate_from(listings: &[Listing]) -> PriceEstimate {
    let mut prices: Vec<f64> = listings.iter().map(|l| l.price_divine).collect();
    prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = prices.len();
    let (low, typical, high) = if n == 0 {
        (0.0, 0.0, 0.0)
    } else {
        let low = prices[0];
        let typical = prices[0];
        let high = prices[(n * 3 / 4).min(n - 1)]; // ~75th percentile
        (low, typical, high)
    };
    PriceEstimate {
        low,
        typical,
        high,
        listing_count: n,
        confidence: Confidence::from_count(n),
    }
}

/// Ablate the top-`k` stat filters (single-drop), ranked by delta, plus one
/// pairwise probe on the top two to flag synergy.
pub async fn breakdown<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    k: usize,
) -> Result<Breakdown> {
    let baseline = estimate(c, query, limit).await?;

    let mut ranked: Vec<Contribution> = Vec::new();
    for (i, sf) in query.stats.iter().enumerate() {
        let mut q = query.clone();
        q.stats.remove(i);
        let without = estimate(c, &q, limit).await?;
        ranked.push(Contribution {
            characteristic: sf.label.clone(),
            kind: AblationKind::Drop,
            delta_divine: baseline.typical - without.typical,
        });
    }
    ranked.sort_by(|a, b| b.delta_divine.partial_cmp(&a.delta_divine).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(k.max(1));

    // Pairwise synergy on the top two (by name → find their indices in query).
    let synergy = if ranked.len() >= 2 {
        let a_label = ranked[0].characteristic.clone();
        let b_label = ranked[1].characteristic.clone();
        let a_idx = query.stats.iter().position(|s| s.label == a_label);
        let b_idx = query.stats.iter().position(|s| s.label == b_label);
        match (a_idx, b_idx) {
            (Some(ai), Some(bi)) if ai != bi => {
                let mut q = query.clone();
                // remove higher index first to keep the other valid
                let (hi, lo) = if ai > bi { (ai, bi) } else { (bi, ai) };
                q.stats.remove(hi);
                q.stats.remove(lo);
                let without_both = estimate(c, &q, limit).await?;
                let drop_both = baseline.typical - without_both.typical;
                let sum_individual = ranked[0].delta_divine + ranked[1].delta_divine;
                // Super-additive synergy: removing both costs more than the sum
                // of removing each individually.
                let extra = sum_individual - drop_both;
                if extra.abs() > f64::EPSILON {
                    Some(SynergyNote { a: a_label, b: b_label, extra_divine: extra })
                } else {
                    None
                }
            }
            _ => None,
        }
    } else {
        None
    };

    Ok(Breakdown {
        baseline,
        ranked,
        synergy,
        trade_url: trade_url(query),
    })
}

/// Human-clickable trade2 search URL for the item's league (a fresh search; the
/// API search id is ephemeral, so we link to the site search page instead).
pub fn trade_url(query: &TradeQuery) -> String {
    format!("https://www.pathofexile.com/trade2/search/{}", query.league)
}
```

(The fake yields: baseline 23; drop-spell delta 16; drop-crit delta 8; drop-both delta 18; `extra = (16 + 8) − 18 = 6`, matching the test.)

- [ ] **Step 4: Run the test to confirm it passes**

Run: `cargo test --lib trade::ablation`
Expected: PASS (all ablation tests).

- [ ] **Step 5: Commit**

```bash
git add src/trade/ablation.rs
git commit -m "feat(trade): price estimate + ablation breakdown with synergy"
```

---

## Task 10: Probe log (`pricelog.rs`)

**Files:**
- Modify: `src/pricelog.rs`

- [ ] **Step 1: Write the failing test**

Put in `src/pricelog.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::{MiscFilters, Probe, TradeQuery};

    fn probe(typical: f64) -> Probe {
        Probe {
            query: TradeQuery {
                league: "Standard".into(),
                category: None,
                type_line: Some("Sapphire Ring".into()),
                stats: vec![],
                misc: MiscFilters::default(),
            },
            listing_count: 7,
            typical_divine: typical,
        }
    }

    #[test]
    fn appends_one_json_line_per_probe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probes.jsonl");
        let log = ProbeLog::new(&path);
        log.append(&probe(10.0)).unwrap();
        log.append(&probe(20.0)).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"typical_divine\":10"));
        assert!(lines[1].contains("Sapphire Ring"));
    }
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test --lib pricelog`
Expected: FAIL — `ProbeLog` undefined; also `Probe`/`TradeQuery`/`MiscFilters` need `Serialize`.

- [ ] **Step 3: Add `Serialize` to the logged types**

In `src/trade/model.rs`, add `serde::Serialize` to the derives of the types written to the log: `Currency`, `StatFilter`, `MiscFilters`, `TradeQuery`, `Probe`. Change each derive line to include `Serialize`, e.g.:

```rust
use serde::Serialize;
// ...
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Currency { /* ... */ }
```

Apply the same `Serialize` addition to `StatFilter`, `MiscFilters`, `TradeQuery`, and `Probe`. (Leave the others unchanged.)

- [ ] **Step 4: Implement the log**

Replace the contents of `src/pricelog.rs` (above the test module) with:

```rust
//! Append-only JSONL log of every trade probe — the corpus that bootstraps the
//! later pricing model. Market data only; no Discord user data is written.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;

use crate::trade::model::Probe;

pub struct ProbeLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl ProbeLog {
    pub fn new(path: impl AsRef<Path>) -> Self {
        ProbeLog { path: path.as_ref().to_path_buf(), lock: Mutex::new(()) }
    }

    /// Appends one probe as a JSON line. Errors are returned, never panicked, so
    /// a logging failure can be downgraded to a warning by the caller.
    pub fn append(&self, probe: &Probe) -> Result<()> {
        let line = serde_json::to_string(probe)?;
        let _guard = self.lock.lock().unwrap();
        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(f, "{line}")?;
        Ok(())
    }
}
```

- [ ] **Step 5: Run the test to confirm it passes**

Run: `cargo test --lib pricelog`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/pricelog.rs src/trade/model.rs
git commit -m "feat(pricelog): append-only JSONL probe log"
```

---

## Task 11: High-level `TradePricer` (`trade/mod.rs`)

Orchestrator that the Discord layer calls: builds the query, gets the estimate/breakdown via `Comparables`, and logs probes. Generic over `Comparables` for testability.

**Files:**
- Modify: `src/trade/mod.rs`

- [ ] **Step 1: Write the failing test**

Add at the bottom of `src/trade/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::itemtext::{ItemStat, ParsedItem, Rarity};
    use crate::trade::ablation::Comparables;
    use crate::trade::model::{Currency, Listing, Money, TradeQuery};
    use async_trait::async_trait;

    struct Flat(f64);
    #[async_trait]
    impl Comparables for Flat {
        async fn comparables(&self, _q: &TradeQuery, _l: usize) -> anyhow::Result<Vec<Listing>> {
            Ok(vec![Listing { price: Money { amount: self.0, currency: Currency::Divine }, price_divine: self.0 }; 8])
        }
    }

    fn ring() -> ParsedItem {
        ParsedItem {
            rarity: Rarity::Rare, name: "Woe Coil".into(), base_type: Some("Sapphire Ring".into()),
            item_class: Some("Rings".into()), item_level: Some(80), quality: None, corrupted: false,
            implicits: vec![], enchants: vec![], runes: vec![],
            explicits: vec![ItemStat { raw: "+40 to maximum Life".into(), value: Some(40.0) }],
        }
    }

    #[tokio::test]
    async fn price_logs_a_probe_and_returns_estimate() {
        let dir = tempfile::tempdir().unwrap();
        let log = crate::pricelog::ProbeLog::new(dir.path().join("p.jsonl"));
        let pricer = TradePricer::new(Flat(12.0), crate::trade::pseudo::PseudoMap::load(), log);
        let est = pricer.price(&ring(), "Standard").await.unwrap();
        assert_eq!(est.typical, 12.0);
        let contents = std::fs::read_to_string(dir.path().join("p.jsonl")).unwrap();
        assert_eq!(contents.lines().count(), 1);
    }
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test --lib trade::tests`
Expected: FAIL — `TradePricer` undefined.

- [ ] **Step 3: Implement `TradePricer`**

Add to `src/trade/mod.rs` (above the test module, after the `pub mod` lines):

```rust
use anyhow::Result;

use crate::itemtext::ParsedItem;
use crate::pricelog::ProbeLog;
use crate::trade::ablation::{breakdown, estimate, Comparables};
use crate::trade::model::{Breakdown, PriceEstimate, Probe};
use crate::trade::pseudo::PseudoMap;
use crate::trade::query::build_baseline;

/// Number of cheapest listings to consider per query.
const LISTING_LIMIT: usize = 10;
/// Number of characteristics to ablate in a breakdown.
const TOP_K: usize = 4;

pub struct TradePricer<C: Comparables> {
    comparables: C,
    pseudo: PseudoMap,
    log: ProbeLog,
}

impl<C: Comparables> TradePricer<C> {
    pub fn new(comparables: C, pseudo: PseudoMap, log: ProbeLog) -> Self {
        TradePricer { comparables, pseudo, log }
    }

    pub async fn price(&self, item: &ParsedItem, league: &str) -> Result<PriceEstimate> {
        let query = build_baseline(item, &self.pseudo, league);
        let est = estimate(&self.comparables, &query, LISTING_LIMIT).await?;
        self.record(&query, &est);
        Ok(est)
    }

    pub async fn breakdown(&self, item: &ParsedItem, league: &str) -> Result<Breakdown> {
        let query = build_baseline(item, &self.pseudo, league);
        let bd = breakdown(&self.comparables, &query, LISTING_LIMIT, TOP_K).await?;
        self.record(&query, &bd.baseline);
        Ok(bd)
    }

    fn record(&self, query: &crate::trade::model::TradeQuery, est: &PriceEstimate) {
        let probe = Probe {
            query: query.clone(),
            listing_count: est.listing_count,
            typical_divine: est.typical,
        };
        if let Err(e) = self.log.append(&probe) {
            tracing::warn!(error = %e, "failed to append probe to price log");
        }
    }
}
```

- [ ] **Step 4: Run the test to confirm it passes**

Run: `cargo test --lib trade::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/trade/mod.rs
git commit -m "feat(trade): TradePricer orchestrator"
```

---

## Task 12: Route rare/magic to the trade path (`store.rs`)

**Files:**
- Modify: `src/store.rs`

- [ ] **Step 1: Update the route test**

In `src/store.rs`'s `tests` module, find the test asserting `NotTracked` for rare/magic and change its expectation to the new `Rare` variant. If none exists, add:

```rust
    #[test]
    fn routes_rare_to_trade_path() {
        let parsed = ParsedItem {
            rarity: Rarity::Rare,
            name: "Woe Coil".into(),
            base_type: Some("Sapphire Ring".into()),
            item_class: None, item_level: None, quality: None, corrupted: false,
            implicits: vec![], enchants: vec![], runes: vec![], explicits: vec![],
        };
        assert!(matches!(route(&[], &parsed), MatchOutcome::Rare));
    }
```

(If an existing `NotTracked` route test is present, update its `matches!(..., MatchOutcome::NotTracked)` to `MatchOutcome::Rare`.)

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test --lib store`
Expected: FAIL — `MatchOutcome::Rare` undefined.

- [ ] **Step 3: Rename the variant**

In `src/store.rs`, in the `MatchOutcome` enum, rename `NotTracked` to `Rare`:

```rust
#[derive(Debug)]
pub enum MatchOutcome<'a> {
    Found(&'a PricedItem),
    Suggestions(Vec<&'a PricedItem>),
    Rare,
    NotFound,
}
```

In `route`, change the early return:

```rust
    if matches!(parsed.rarity, Rarity::Magic | Rarity::Rare) {
        return MatchOutcome::Rare;
    }
```

- [ ] **Step 4: Run the store tests to confirm they pass**

Run: `cargo test --lib store`
Expected: PASS. (`cargo build` will now error in `paste.rs` until Task 14 — that's expected; do not "fix" it here.)

- [ ] **Step 5: Commit**

```bash
git add src/store.rs
git commit -m "feat(store): route rare/magic to trade pricing path"
```

---

## Task 13: Embeds for estimate + breakdown (`discord/embeds.rs`)

Follow the existing pattern: TDD the string helpers; build the embeds from them (embeds themselves untested, like `item_embed`).

**Files:**
- Modify: `src/discord/embeds.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/discord/embeds.rs`:

```rust
    use crate::trade::model::{AblationKind, Confidence, Contribution, PriceEstimate};

    #[test]
    fn estimate_value_string_formats_range_and_confidence() {
        let est = PriceEstimate { low: 8.0, typical: 8.0, high: 15.0, listing_count: 12, confidence: Confidence::High };
        let s = estimate_value_string(&est);
        assert!(s.contains("8"));
        assert!(s.contains("15"));
        assert_eq!(confidence_string(&est.confidence), "High");
    }

    #[test]
    fn contribution_line_shows_label_and_delta() {
        let c = Contribution { characteristic: "+to all Spell Skills".into(), kind: AblationKind::Drop, delta_divine: 16.0 };
        let line = contribution_line(&c);
        assert!(line.contains("+to all Spell Skills"));
        assert!(line.contains("16"));
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test --lib discord::embeds`
Expected: FAIL — helpers undefined.

- [ ] **Step 3: Implement helpers + embeds**

Add to `src/discord/embeds.rs` (the file already has `use` for serenity + `League`; add imports as needed at the top):

```rust
use crate::itemtext::ParsedItem;
use crate::trade::model::{Breakdown, Confidence, Contribution, PriceEstimate};

pub fn estimate_value_string(est: &PriceEstimate) -> String {
    if est.listing_count == 0 {
        return "No comparable listings".to_string();
    }
    if (est.high - est.low).abs() < f64::EPSILON {
        format!("~{:.1} div", est.typical)
    } else {
        format!("{:.1}–{:.1} div", est.low, est.high)
    }
}

pub fn confidence_string(c: &Confidence) -> String {
    match c {
        Confidence::High => "High",
        Confidence::Medium => "Medium",
        Confidence::Low => "Low",
    }
    .to_string()
}

pub fn contribution_line(c: &Contribution) -> String {
    format!("• {} — ~{:.1} div", c.characteristic, c.delta_divine)
}

pub fn estimate_embed(parsed: &ParsedItem, est: &PriceEstimate, league: &League) -> serenity::CreateEmbed {
    let title = parsed.base_type.clone().unwrap_or_else(|| parsed.name.clone());
    serenity::CreateEmbed::default()
        .title(title)
        .description(format!("**{}**", parsed.name))
        .field("Estimated value", estimate_value_string(est), true)
        .field(
            "Confidence",
            format!("{} ({} listings)", confidence_string(&est.confidence), est.listing_count),
            true,
        )
        .footer(serenity::CreateEmbedFooter::new(format!(
            "live trade • {} • not affiliated with GGG",
            league.name
        )))
}

pub fn breakdown_embed(parsed: &ParsedItem, bd: &Breakdown, league: &League) -> serenity::CreateEmbed {
    let mut lines: Vec<String> = bd.ranked.iter().map(contribution_line).collect();
    if let Some(syn) = &bd.synergy {
        lines.push(format!(
            "✨ synergy: **{}** + **{}** add ~{:.1} div together",
            syn.a, syn.b, syn.extra_divine
        ));
    }
    let body = if lines.is_empty() { "No drivers identified.".to_string() } else { lines.join("\n") };
    serenity::CreateEmbed::default()
        .title(format!("What drives the price — {}", parsed.name))
        .description(body)
        .url(bd.trade_url.clone())
        .footer(serenity::CreateEmbedFooter::new(format!(
            "live trade • {} • not affiliated with GGG",
            league.name
        )))
}
```

(Check the file's existing `use` block first; `serenity`, `League`, and `PricedItem` are already imported — only add the lines above that are missing.)

- [ ] **Step 4: Run the test to confirm it passes**

Run: `cargo test --lib discord::embeds`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/discord/embeds.rs
git commit -m "feat(embeds): estimate + breakdown embeds"
```

---

## Task 14: Wire it together (config, Data, main, paste)

**Files:**
- Modify: `src/config.rs`
- Modify: `src/discord/mod.rs`
- Modify: `src/discord/paste.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Config test for the optional session**

Add to the `tests` module in `src/config.rs`:

```rust
    #[test]
    fn reads_optional_poe_sessid() {
        let cfg = Config::from_lookup(|k| match k {
            "DISCORD_TOKEN" => Some("t".into()),
            "GUILD_ID" => Some("1".into()),
            "POE_SESSID" => Some("abc".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg.poe_sessid.as_deref(), Some("abc"));

        let cfg2 = Config::from_lookup(|k| match k {
            "DISCORD_TOKEN" => Some("t".into()),
            "GUILD_ID" => Some("1".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg2.poe_sessid, None);
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test --lib config`
Expected: FAIL — `poe_sessid` field missing.

- [ ] **Step 3: Add the field + loader**

In `src/config.rs`, add to the `Config` struct:

```rust
    pub poe_sessid: Option<String>,
```

In `from_lookup`, before the final `Ok(Self { ... })`, add:

```rust
        let poe_sessid = get("POE_SESSID").filter(|s| !s.is_empty());
```

and add `poe_sessid,` to the struct literal. (The `impl std::fmt::Debug for Config` is hand-written — if it lists fields explicitly, do **not** add `poe_sessid` there to avoid leaking the secret in logs; leave Debug as-is.)

- [ ] **Step 4: Run the config test to confirm it passes**

Run: `cargo test --lib config`
Expected: PASS.

- [ ] **Step 5: Add the pricer to `Data`**

In `src/discord/mod.rs`, add imports and the field. The pricer's `Comparables` is the concrete `TradeClient`, so the type is `TradePricer<TradeClient>`; wrap in `Arc` for cheap cloning into the framework:

```rust
use std::sync::Arc;
use crate::trade::client::TradeClient;
use crate::trade::TradePricer;
```

```rust
pub struct Data {
    pub store: PriceStore,
    pub config: Config,
    pub pricer: Arc<TradePricer<TradeClient>>,
}
```

- [ ] **Step 6: Construct the pricer in `main.rs`**

In `src/main.rs`, add imports near the others:

```rust
use std::sync::Arc;
use trade::client::TradeClient;
use trade::pseudo::PseudoMap;
use trade::TradePricer;
use pricelog::ProbeLog;
```

In `main`, after `let client = NinjaClient::new()?;` and before building the framework, construct the pricer:

```rust
    let trade_client = TradeClient::new(config.poe_sessid.clone())?;
    let pricer = Arc::new(TradePricer::new(
        trade_client,
        PseudoMap::load(),
        ProbeLog::new("probes.jsonl"),
    ));
```

In the `.setup(...)` closure, include `pricer` in the returned `Data`. Change the closure capture to also move `pricer` in, and update the struct literal:

```rust
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id)
                    .await?;
                tracing::info!("commands registered; bot ready");
                Ok(Data { store, config, pricer })
            })
        })
```

- [ ] **Step 7: Handle the rare branch in `paste.rs`**

In `src/discord/paste.rs`, replace the `MatchOutcome::NotTracked => { ... }` arm with a `MatchOutcome::Rare => { ... }` arm that prices the item, sends the estimate with a button, and waits for a breakdown click. Add imports at the top of the file:

```rust
use std::time::Duration;
use crate::poeninja::League;
```

Replace the arm:

```rust
        MatchOutcome::Rare => {
            price_rare(&ctx, &parsed, &snap.league).await?;
        }
```

Add this function below `paste` (above `format_not_found`):

```rust
async fn price_rare(
    ctx: &Context<'_>,
    parsed: &itemtext::ParsedItem,
    league: &League,
) -> Result<(), Error> {
    let pricer = ctx.data().pricer.clone();
    let est = match pricer.price(parsed, &league.name).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "trade price failed");
            ctx.say("Couldn't reach trade right now — try again shortly.").await?;
            return Ok(());
        }
    };

    let button = serenity::CreateButton::new("drp_breakdown")
        .label("Break it down")
        .style(serenity::ButtonStyle::Secondary);
    let row = serenity::CreateActionRow::Buttons(vec![button]);

    let reply = ctx
        .send(
            poise::CreateReply::default()
                .embed(embeds::estimate_embed(parsed, &est, league))
                .components(vec![row]),
        )
        .await?;

    // Wait up to 120s for the breakdown click.
    let msg = reply.message().await?;
    let interaction = msg
        .await_component_interaction(ctx.serenity_context().shard.clone())
        .timeout(Duration::from_secs(120))
        .custom_ids(vec!["drp_breakdown".to_string()])
        .await;

    match interaction {
        Some(mci) => {
            mci.defer(ctx.serenity_context()).await?;
            match pricer.breakdown(parsed, &league.name).await {
                Ok(bd) => {
                    mci.create_followup(
                        ctx.serenity_context(),
                        serenity::CreateInteractionResponseFollowup::default()
                            .embed(embeds::breakdown_embed(parsed, &bd, league)),
                    )
                    .await?;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "trade breakdown failed");
                    mci.create_followup(
                        ctx.serenity_context(),
                        serenity::CreateInteractionResponseFollowup::default()
                            .content("Couldn't break that down right now."),
                    )
                    .await?;
                }
            }
            // Remove the button so it can't be clicked again.
            reply
                .edit(*ctx, poise::CreateReply::default()
                    .embed(embeds::estimate_embed(parsed, &est, league))
                    .components(vec![]))
                .await?;
        }
        None => {
            // Timed out: drop the button.
            reply
                .edit(*ctx, poise::CreateReply::default()
                    .embed(embeds::estimate_embed(parsed, &est, league))
                    .components(vec![]))
                .await?;
        }
    }
    Ok(())
}
```

> **Runtime-verify (serenity 0.12 component API):** the collector builder (`await_component_interaction`, `.custom_ids`, `.timeout`) and follow-up/edit calls are the area most likely to need small signature tweaks against the installed serenity. If a method name differs, adjust to the equivalent in `cargo doc -p serenity --open`; the *shape* (send with button → await one component → defer → follow up with breakdown → strip button) stays the same.

- [ ] **Step 8: Build and run the whole suite**

Run: `cargo build`
Expected: compiles.
Run: `cargo test`
Expected: all tests pass (ignored live tests skipped).

- [ ] **Step 9: Commit**

```bash
git add src/config.rs src/discord/mod.rs src/discord/paste.rs src/main.rs
git commit -m "feat(discord): price rares on /paste with breakdown button"
```

---

## Task 15: Documentation

**Files:**
- Modify: `.env.example`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Document `POE_SESSID` in `.env.example`**

Append to `.env.example`:

```dotenv
# Optional. A Path of Exile session cookie (POESESSID) for the operator's own
# account. Leave blank to run anonymously (works, but lower trade rate limits).
# Treat as a SECRET — never commit a real value.
POE_SESSID=
```

- [ ] **Step 2: Note the new behavior + pseudo-map upkeep in `CLAUDE.md`**

In `CLAUDE.md`, under the `/paste` bullet in "Command surfaces", append:

```markdown
  Rare/magic items are priced live via the official PoE2 `trade2` API (ablation
  pricing); other items use the poe.ninja snapshot.
```

Add a short subsection under "Configuration":

```markdown
- `POE_SESSID` — optional PoE session cookie to raise trade rate limits.
  **Secret.** Anonymous reads work without it (tighter limits).
```

Add under "Conventions":

```markdown
- `src/trade/data/pseudo_map.json` is a maintained artifact — re-check the
  stat→pseudo mappings against `trade2/data/stats` after each major PoE2 patch.
```

- [ ] **Step 3: Commit**

```bash
git add .env.example CLAUDE.md
git commit -m "docs: document POE_SESSID and pseudo-map maintenance"
```

---

## Task 16: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Format**

Run: `cargo fmt`
Then: `git diff --stat` — if anything changed, `git add -u && git commit -m "style: cargo fmt"`.

- [ ] **Step 2: Lint**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings. Fix any inline, commit as `chore: clippy`.

- [ ] **Step 3: Full test suite**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 4: Live smoke test (manual gate)**

Run: `cargo test -- --ignored`
Expected: the trade2 smoke test passes (or reveals JSON-shape corrections to apply to Task 6/7 only). The poe.ninja live test also runs.

- [ ] **Step 5: Confirm clean tree**

Run: `git status` — only `.claude/` untracked expected.

---

## Notes for the implementer

- **Backward compatibility:** `ParsedItem` gained fields and dropped `Eq`. All construction sites are in `itemtext.rs`, `store.rs` tests, and `trade/` tests — each updated by the tasks above. If you find another constructor, fill the new fields with defaults (`None`/`false`/`vec![]`).
- **Don't over-build:** category inference, tier labeling, archetype classification, DPS recomputation, per-member sessions, and live currency-rate refresh are **deferred** (see spec §3, §13). Leave the `CurrencyRates` defaults and the `category: None` as-is.
- **ToS/politeness:** keep the bounded query budget (`LISTING_LIMIT`, `TOP_K`), the single User-Agent with the "not affiliated" string, and the optional-session model. Do not add proxies or pool sessions.
- **Adaptive budget (spec §8):** v1 realizes "adapt to remaining headroom" as **reactive 429 backoff** (`send_with_retry`) plus a **fixed conservative budget** (`TOP_K = 4`). Dynamically scaling `TOP_K` to live header-reported headroom is a deliberate later refinement — not in this plan.
