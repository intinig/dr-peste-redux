# Phase 4 — Market Learning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a per-category ValueModel from the observation corpus that surfaces value-drivers via `/insights` and feeds learned value back into the price-check's relaxation order and band tightness.

**Architecture:** A new `src/trade/value.rs` builds an in-memory `ValueModel` (descriptive aggregation: lift, top-decile frequency, co-occurrence, plus a greedy conditional-lift deconfounding pass for the insights ranking) by streaming the append-only JSONL corpus. It is rebuilt at startup, on a periodic timer, and after each harvest, held behind `Arc<RwLock<ValueModel>>`. Two consumers read it: a new `/insights` command and the existing `build_baseline` price-check query builder.

**Tech Stack:** Rust, tokio, poise/serenity, serde/serde_json, anyhow, tracing. No new dependencies.

## Global Constraints

- **No new crates.** Use what `Cargo.toml` already has (serde, serde_json, anyhow, tracing, tokio, futures, tempfile).
- **Binary crate, no lib target:** run `cargo test` (never `--lib`). CI runs `cargo clippy --all-targets -- -D warnings` on a stricter toolchain — **every new function/field MUST have a real (non-`#[cfg(test)]`) caller/reader by the end of its task**, or the dead-code lint fails CI. This is why each task below is integration-complete (build + a real consumer in the same task).
- **Canonical category = the trade2 category text** (e.g. `"Staff"`), never the clipboard plural (`"Staves"`). `canonical_category()` folds the clipboard Item Class to the trade2 text via a static alias map and is applied both when building the model (read authority) and when paste logs an observation (write alignment).
- **Empty/untrusted ValueModel ⇒ price-check behaves byte-identically to today.** This is a hard requirement, regression-tested in Task 3.
- **Deconfounding (conditional lift) affects only the `/insights` ranking, never pricing.** Pricing uses raw univariate `lift`.
- **Learning is best-effort and never blocks or panics pricing:** corrupt corpus lines are skipped on read; a build failure keeps the last good model; a missing/thin model routes pricing to the cold-start path.
- **rust-analyzer diagnostics are known-stale mid-edit in this repo** (E0560/E0063/dead_code). Trust `cargo build`/`cargo test`/`cargo clippy`, not the editor squiggles.
- Errors via `anyhow`; log via `tracing`. Async throughout (tokio). Commit only the files each step names (never `git add -A`); verify no secrets.

## File Structure

- `src/trade/value.rs` *(new)* — `canonical_category()` + alias table; `ValueModel`, `CategoryModel`, `StatValue`, `ModPair`; `ValueModel::build(&[Observation])`; `rebuild_into(&ObservationLog, &RwLock<ValueModel>)`; value-driver / mod-strength helpers; module constants (`MIN_CATEGORY_SAMPLE`, `MIN_STAT_SAMPLE`, `DRIVER_LIFT`, `VALUE_REFRESH_MINS`, display caps).
- `src/observe.rs` *(modify)* — add `ObservationLog::read_all()` (skips corrupt lines).
- `src/trade/mod.rs` *(modify)* — register `pub mod value`; `TradePricer` gains `value: Arc<RwLock<ValueModel>>` + reads it in `price()`/`breakdown()` (Task 3); `log_observations` writes the canonical category (Task 1).
- `src/trade/stats.rs` *(modify, Task 2)* — `label_for(stat_id) -> Option<&str>` reverse lookup.
- `src/trade/query.rs` *(modify, Task 3)* — `build_baseline` gains a `&ValueModel` param; value-driver band tier + relax ordering.
- `src/discord/insights.rs` *(new)* — `/insights [category]` command, embed, category autocomplete.
- `src/discord/mod.rs` *(modify)* — `pub mod insights`; `Data` gains `value: Arc<RwLock<ValueModel>>`.
- `src/main.rs` *(modify)* — build model at startup, `spawn_value_refresher`, register `/insights`, share the `Arc` into `Data` and `TradePricer`.

---

## Task 1: ValueModel skeleton + lifecycle + category `/insights`

Build the model's grouping/base-median layer, its read path and refresh lifecycle, and a minimal `/insights` that lists categories — the first real reader, so nothing is dead code. Driver metrics (lift/top-decile/co-occurrence/deconfounding) come in Task 2.

**Files:**
- Create: `src/trade/value.rs`
- Test: inline `#[cfg(test)]` in `src/trade/value.rs` and `src/observe.rs`
- Create: `src/discord/insights.rs`
- Modify: `src/observe.rs` (add `read_all`)
- Modify: `src/trade/mod.rs` (`pub mod value`; `log_observations` canonical category)
- Modify: `src/discord/mod.rs` (`pub mod insights`; `Data.value`)
- Modify: `src/main.rs` (startup build, refresher, register command, share Arc)

**Interfaces:**
- Produces:
  - `pub fn canonical_category(raw: &str) -> String`
  - `pub struct ValueModel { categories: HashMap<String, CategoryModel> }` with `pub fn category(&self, canon: &str) -> Option<&CategoryModel>` and `pub fn categories_sorted(&self) -> Vec<&CategoryModel>` (descending by `sample_size`).
  - `pub struct CategoryModel { pub category: String, pub sample_size: usize, pub base_median: f64 }` (more fields added in Task 2).
  - `pub fn ValueModel::build(observations: &[crate::observe::Observation]) -> ValueModel`
  - `pub fn rebuild_into(log: &crate::observe::ObservationLog, slot: &std::sync::RwLock<ValueModel>)` — read_all + build + swap; best-effort, logs on failure.
  - `pub const MIN_CATEGORY_SAMPLE: usize = 50;` `pub const VALUE_REFRESH_MINS: u64 = 60;`
  - `impl ObservationLog { pub fn read_all(&self) -> Vec<Observation> }`
  - `pub fn insights()` poise command in `discord::insights`.
- Consumes: `crate::observe::{Observation, ObservationLog}`, `crate::discord::{Context, Error, Data}`.

- [ ] **Step 1: Write the failing test for `canonical_category`**

In a new file `src/trade/value.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_category_folds_clipboard_plurals() {
        assert_eq!(canonical_category("Staves"), "Staff");
        assert_eq!(canonical_category("staves"), "Staff"); // case-insensitive
        assert_eq!(canonical_category("Wands"), "Wand");
        assert_eq!(canonical_category("Amulets"), "Amulet");
        // Already-canonical trade2 text is idempotent.
        assert_eq!(canonical_category("Staff"), "Staff");
        // Unknown passes through trimmed.
        assert_eq!(canonical_category("  Fishing Rod  "), "Fishing Rod");
    }
}
```

- [ ] **Step 2: Run it to confirm it fails to compile (function missing)**

Run: `cargo test canonical_category_folds_clipboard_plurals 2>&1 | tail -5`
Expected: compile error `cannot find function canonical_category`.

- [ ] **Step 3: Implement `canonical_category` + alias table**

At the top of `src/trade/value.rs`:

```rust
//! Descriptive market ValueModel mined from the observation corpus: per-category
//! value-drivers (lift, top-decile frequency, co-occurrence) plus a deconfounded
//! ranking for `/insights`, and learned value fed back into the price-check.
//! Market data only — never any member secret.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::observe::{Observation, ObservationLog};

/// A category needs at least this many listings before it is "trusted" for
/// pricing feedback (insights still renders a thin-data note below it).
pub const MIN_CATEGORY_SAMPLE: usize = 50;
/// Periodic ValueModel rebuild interval, minutes.
pub const VALUE_REFRESH_MINS: u64 = 60;

/// Folds a category string to the canonical trade2 category text. The clipboard
/// `Item Class` is plural ("Staves") while the trade2 category is singular
/// ("Staff"); harvest already logs the trade2 text. The PoE item-class taxonomy
/// is a closed, known set, so this static map is a maintained artifact (re-check
/// after a major PoE2 patch). Unknown input passes through trimmed.
pub fn canonical_category(raw: &str) -> String {
    let key = raw.trim().to_lowercase();
    let canon = match key.as_str() {
        "staves" | "staff" => "Staff",
        "wands" | "wand" => "Wand",
        "sceptres" | "sceptre" => "Sceptre",
        "quarterstaves" | "quarterstaff" => "Quarterstaff",
        "bows" | "bow" => "Bow",
        "crossbows" | "crossbow" => "Crossbow",
        "amulets" | "amulet" => "Amulet",
        "rings" | "ring" => "Ring",
        "belts" | "belt" => "Belt",
        "body armours" | "body armour" => "Body Armour",
        "helmets" | "helmet" => "Helmet",
        "gloves" => "Gloves",
        "boots" => "Boots",
        "shields" | "shield" => "Shield",
        "foci" | "focus" => "Focus",
        "quivers" | "quiver" => "Quiver",
        _ => return raw.trim().to_string(),
    };
    canon.to_string()
}
```

- [ ] **Step 4: Run the test to confirm it passes**

Run: `cargo test canonical_category_folds_clipboard_plurals 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`.

- [ ] **Step 5: Write the failing test for `ObservationLog::read_all`**

In `src/observe.rs`, inside the existing `#[cfg(test)] mod tests`, add (reuse the existing `obs(price)` helper):

```rust
    #[test]
    fn read_all_returns_observations_and_skips_corrupt_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obs.jsonl");
        let log = ObservationLog::new(&path);
        log.append(&obs(10.0)).unwrap();
        // A corrupt line between two good ones must be skipped, not fatal.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{ not json\n")
            .unwrap();
        log.append(&obs(20.0)).unwrap();

        let all = log.read_all();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].price_divine, 10.0);
        assert_eq!(all[1].price_divine, 20.0);
    }

    #[test]
    fn read_all_on_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log = ObservationLog::new(dir.path().join("nope.jsonl"));
        assert!(log.read_all().is_empty());
    }
```

- [ ] **Step 6: Run it to confirm it fails**

Run: `cargo test read_all 2>&1 | tail -5`
Expected: compile error `no method named read_all`.

- [ ] **Step 7: Implement `ObservationLog::read_all`**

In `src/observe.rs`, add to `impl ObservationLog` (the file already imports `std::io::Write`; add `std::io::Read` is not needed — use `std::fs::read_to_string`):

```rust
    /// Reads every well-formed observation from the log. Corrupt/partial lines
    /// are skipped (best-effort); a missing file yields an empty Vec. Never
    /// panics — the learning layer must degrade gracefully.
    pub fn read_all(&self) -> Vec<Observation> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let body = match std::fs::read_to_string(&self.path) {
            Ok(b) => b,
            Err(_) => return Vec::new(),
        };
        body.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Observation>(l).ok())
            .collect()
    }
```

- [ ] **Step 8: Run the read_all tests**

Run: `cargo test read_all 2>&1 | tail -5`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 9: Write the failing test for `ValueModel::build` (grouping + base median)**

In `src/trade/value.rs` tests, add a corpus helper and a test:

```rust
    use crate::observe::{Observation, Source};
    use crate::trade::model::ListingMod;

    fn ob(category: &str, price: f64, stats: &[&str]) -> Observation {
        Observation {
            timestamp_unix: 0,
            league: "Standard".into(),
            base_type: Some("Chiming Staff".into()),
            category: Some(category.into()),
            mods: stats
                .iter()
                .map(|s| ListingMod { stat_id: (*s).into(), tier: None, roll: None })
                .collect(),
            price_divine: price,
            source: Source::Harvest,
        }
    }

    #[test]
    fn build_groups_by_canonical_category_with_base_median() {
        // "Staves" (paste) and "Staff" (harvest) must fold to one "Staff" group.
        let corpus = vec![
            ob("Staves", 1.0, &["explicit.a"]),
            ob("Staff", 3.0, &["explicit.a"]),
            ob("Staff", 5.0, &["explicit.b"]),
        ];
        let model = ValueModel::build(&corpus);
        let cat = model.category("Staff").expect("Staff category present");
        assert_eq!(cat.sample_size, 3);
        assert_eq!(cat.base_median, 3.0); // median of [1,3,5]
        assert!(model.category("Staves").is_none()); // folded, not a separate key
    }
```

- [ ] **Step 10: Run it to confirm it fails**

Run: `cargo test build_groups_by_canonical 2>&1 | tail -5`
Expected: compile error (`ValueModel`/`build`/`category` missing).

- [ ] **Step 11: Implement the ValueModel skeleton + `build` + accessors + `median`**

In `src/trade/value.rs` (after `canonical_category`):

```rust
/// Per-category descriptive value model. Keyed by canonical trade2 category text.
#[derive(Debug, Default, Clone)]
pub struct ValueModel {
    categories: HashMap<String, CategoryModel>,
}

/// Aggregated value signal for one category. Driver metrics are added in Task 2.
#[derive(Debug, Default, Clone)]
pub struct CategoryModel {
    pub category: String,
    pub sample_size: usize,
    pub base_median: f64,
}

impl ValueModel {
    pub fn category(&self, canon: &str) -> Option<&CategoryModel> {
        self.categories.get(canon)
    }

    /// Categories ordered by descending sample size (largest corpus first).
    pub fn categories_sorted(&self) -> Vec<&CategoryModel> {
        let mut v: Vec<&CategoryModel> = self.categories.values().collect();
        v.sort_by(|a, b| b.sample_size.cmp(&a.sample_size));
        v
    }

    pub fn build(observations: &[Observation]) -> ValueModel {
        // Group prices by canonical category (skip observations with no class).
        let mut by_cat: HashMap<String, Vec<f64>> = HashMap::new();
        for o in observations {
            let Some(raw) = o.category.as_deref() else { continue };
            by_cat
                .entry(canonical_category(raw))
                .or_default()
                .push(o.price_divine);
        }
        let mut categories = HashMap::new();
        for (category, mut prices) in by_cat {
            prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let sample_size = prices.len();
            let base_median = median(&prices);
            categories.insert(
                category.clone(),
                CategoryModel { category, sample_size, base_median },
            );
        }
        ValueModel { categories }
    }
}

/// Median of a slice. Sorts a copy; returns 0.0 for an empty slice.
fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}
```

- [ ] **Step 12: Run the build test**

Run: `cargo test build_groups_by_canonical 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`.

- [ ] **Step 13: Implement `rebuild_into` (used by the refresher + startup)**

In `src/trade/value.rs`:

```rust
/// Rebuilds the ValueModel from the corpus and swaps it into `slot`. Best-effort:
/// reads are corrupt-line-tolerant; a poisoned lock is recovered, never panicked.
pub fn rebuild_into(log: &ObservationLog, slot: &RwLock<ValueModel>) {
    let model = ValueModel::build(&log.read_all());
    let n = model.categories.len();
    *slot.write().unwrap_or_else(|e| e.into_inner()) = model;
    tracing::info!(categories = n, "value model rebuilt");
}
```

- [ ] **Step 14: Register the module and write the canonical category on paste**

In `src/trade/mod.rs`, add to the module list (alphabetical, after `stats` or near `query`):

```rust
pub mod value;
```

Then in `log_observations` (same file), change the category written so paste logs the canonical trade2 category instead of the raw clipboard class:

```rust
                category: item
                    .item_class
                    .as_deref()
                    .map(crate::trade::value::canonical_category),
```

(replacing the prior `category: item.item_class.clone(),`).

- [ ] **Step 15: Build to confirm the module wires in**

Run: `cargo build 2>&1 | tail -8`
Expected: compiles. (`rebuild_into`/`categories_sorted` are not yet called from non-test code — they get real callers in Steps 16–18 within this same task, so do not commit until then.)

- [ ] **Step 16: Add `Data.value` and the `/insights` minimal command**

In `src/discord/mod.rs`, add `pub mod insights;` to the module list and a field to `Data`:

```rust
    pub value: Arc<RwLock<crate::trade::value::ValueModel>>,
```

Create `src/discord/insights.rs`:

```rust
//! `/insights [category]` — surfaces the learned ValueModel: which mods drive
//! price for a category. Read-only; open to everyone (non-secret market data).

use super::{Context, Error};
use crate::trade::value::{canonical_category, MIN_CATEGORY_SAMPLE};
use futures::Stream;

/// Autocomplete: canonical category names present in the model, prefix-matched.
pub async fn autocomplete_insights_category<'a>(
    ctx: Context<'a>,
    partial: &'a str,
) -> impl Stream<Item = String> + 'a {
    let p = partial.to_lowercase();
    let names: Vec<String> = {
        let model = ctx.data().value.read().unwrap_or_else(|e| e.into_inner());
        model
            .categories_sorted()
            .into_iter()
            .map(|c| c.category.clone())
            .filter(|name| name.to_lowercase().contains(&p))
            .take(25)
            .collect()
    };
    futures::stream::iter(names)
}

/// Show learned value-drivers for a category (or list categories with no arg).
#[poise::command(slash_command)]
pub async fn insights(
    ctx: Context<'_>,
    #[description = "Item category (e.g. Staff). Omit to list categories."]
    #[autocomplete = "autocomplete_insights_category"]
    category: Option<String>,
) -> Result<(), Error> {
    let model = ctx.data().value.read().unwrap_or_else(|e| e.into_inner());

    let Some(category) = category else {
        // No arg: list categories with their sample sizes.
        let cats = model.categories_sorted();
        if cats.is_empty() {
            ctx.say("No market data yet — run `/harvest <category>` or price some rares first.")
                .await?;
            return Ok(());
        }
        let mut lines = String::from("**Categories with market data:**\n");
        for c in cats.iter().take(25) {
            lines.push_str(&format!(
                "• **{}** — {} listings (median {:.1} div)\n",
                c.category, c.sample_size, c.base_median
            ));
        }
        lines.push_str("\nPass one, e.g. `/insights category:Staff`.");
        ctx.say(lines).await?;
        return Ok(());
    };

    let canon = canonical_category(&category);
    let Some(cat) = model.category(&canon) else {
        ctx.say(format!("No market data yet for **{canon}**.")).await?;
        return Ok(());
    };
    if cat.sample_size < MIN_CATEGORY_SAMPLE {
        ctx.say(format!(
            "Only {} listings for **{canon}** so far (need ≥{MIN_CATEGORY_SAMPLE} for reliable insights). Harvest more.",
            cat.sample_size
        ))
        .await?;
        return Ok(());
    }
    // Driver detail arrives in Task 2; for now confirm the category is tracked.
    ctx.say(format!(
        "**{canon}** — {} listings, median {:.1} div. Driver insights coming online.",
        cat.sample_size, cat.base_median
    ))
    .await?;
    Ok(())
}
```

- [ ] **Step 17: Wire startup build + periodic refresher + post-harvest rebuild + register command**

In `src/main.rs`:

Add near the other `use` lines:

```rust
use trade::value::{rebuild_into, ValueModel, VALUE_REFRESH_MINS};
```

Add a refresher spawner next to `spawn_refresher`:

```rust
fn spawn_value_refresher(
    log: ObservationLog,
    value: std::sync::Arc<std::sync::RwLock<ValueModel>>,
    interval: Duration,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            rebuild_into(&log, &value);
        }
    });
}
```

In `main`, after the `pricer` is constructed (it owns the only `ObservationLog`; build a second `ObservationLog` handle to the same path for the model — `ObservationLog::new` just stores a path):

```rust
    let value = std::sync::Arc::new(std::sync::RwLock::new(ValueModel::default()));
    rebuild_into(&ObservationLog::new(&config.observation_log_path), &value); // startup build
    spawn_value_refresher(
        ObservationLog::new(&config.observation_log_path),
        value.clone(),
        Duration::from_secs(VALUE_REFRESH_MINS * 60),
    );
```

Add `discord::insights::insights(),` to the `commands: vec![...]`.

Add `value: value.clone(),` to the `Data { ... }` constructor.

Post-harvest rebuild — in `src/discord/harvest.rs`, after a successful harvest (the `Ok(n) =>` arm, before/after editing the reply), trigger a rebuild so insights reflect the new data immediately:

```rust
            crate::trade::value::rebuild_into(
                &crate::observe::ObservationLog::new(&data.config.observation_log_path),
                &data.value,
            );
```

- [ ] **Step 18: Build, then run the full suite**

Run: `cargo build 2>&1 | tail -8`
Expected: compiles with no dead-code warnings (every new symbol now has a real caller: `rebuild_into` from startup/refresher/harvest, `categories_sorted`/`category` from `/insights`).

Run: `cargo test 2>&1 | tail -6`
Expected: all tests pass (existing + the new value/observe tests).

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | tail -8`
Expected: clean.

- [ ] **Step 19: Commit**

```bash
git add src/trade/value.rs src/observe.rs src/trade/mod.rs src/discord/mod.rs src/discord/insights.rs src/discord/harvest.rs src/main.rs
git commit -m "feat(value): ValueModel skeleton + lifecycle + category /insights (Phase 4 Task 1)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: Driver metrics (lift, top-decile, co-occurrence) + deconfounding + enriched `/insights`

Extend the build to compute per-stat value signal and the deconfounded driver ranking, and enrich `/insights` to display them (the real reader of the new fields). Add the StatCatalog reverse-label lookup for readable output.

**Files:**
- Modify: `src/trade/value.rs` (add `StatValue`, `ModPair`, driver/co-occurrence/deconfounding logic to `build`)
- Modify: `src/trade/stats.rs` (add `label_for`)
- Modify: `src/discord/insights.rs` (render drivers)
- Test: inline in `src/trade/value.rs` and `src/trade/stats.rs`

**Interfaces:**
- Produces:
  - `pub struct StatValue { pub stat_id: String, pub label: Option<String>, pub count: usize, pub median_with: f64, pub lift: f64, pub conditional_lift: Option<f64>, pub top_decile_freq: f64 }`
  - `pub struct ModPair { pub a: String, pub b: String, pub count: usize }`
  - `CategoryModel` gains `pub stats: Vec<StatValue>` (deconfounded rank order) and `pub cooccurrences: Vec<ModPair>`.
  - `pub const MIN_STAT_SAMPLE: usize = 15;` `pub const DRIVER_LIFT: f64 = 1.5;`
  - `CategoryModel::drivers(&self) -> impl Iterator<Item = &StatValue>` (stats with `lift >= DRIVER_LIFT && count >= MIN_STAT_SAMPLE`).
  - `impl StatCatalog { pub fn label_for(&self, stat_id: &str) -> Option<&str> }`
- Consumes: Task 1 `ValueModel`, `canonical_category`, `MIN_CATEGORY_SAMPLE`.

- [ ] **Step 1: Write the failing test — lift + top-decile recover a planted driver**

In `src/trade/value.rs` tests:

```rust
    #[test]
    fn build_recovers_a_planted_driver() {
        // Category base is cheap; listings carrying "drv" are expensive.
        let mut corpus = Vec::new();
        for _ in 0..40 {
            corpus.push(ob("Staff", 1.0, &["filler"]));
        }
        for _ in 0..40 {
            corpus.push(ob("Staff", 10.0, &["drv", "filler"]));
        }
        let model = ValueModel::build(&corpus);
        let cat = model.category("Staff").unwrap();
        let drv = cat.stats.iter().find(|s| s.stat_id == "drv").unwrap();
        assert_eq!(drv.count, 40);
        assert!(drv.lift > 1.5, "driver lift should be well above 1: {}", drv.lift);
        assert!(drv.top_decile_freq > 0.9, "driver should dominate the expensive tail");
        // "drv" is a value-driver; "filler" (on everything) is not.
        assert!(cat.drivers().any(|s| s.stat_id == "drv"));
        assert!(!cat.drivers().any(|s| s.stat_id == "filler"));
    }
```

- [ ] **Step 2: Write the failing test — deconfounding demotes a co-traveler**

```rust
    #[test]
    fn deconfounding_collapses_a_co_traveler() {
        // A: genuine driver (expensive with or without B). B: rides A only.
        let mut corpus = Vec::new();
        for _ in 0..30 {
            corpus.push(ob("Staff", 1.0, &["base"])); // cheap baseline
        }
        for _ in 0..30 {
            corpus.push(ob("Staff", 10.0, &["A", "B", "base"])); // A and B together, expensive
        }
        for _ in 0..30 {
            corpus.push(ob("Staff", 10.0, &["A", "base"])); // A alone, still expensive
        }
        // B never appears without A, and contributes nothing on its own.
        let model = ValueModel::build(&corpus);
        let cat = model.category("Staff").unwrap();
        let a = cat.stats.iter().find(|s| s.stat_id == "A").unwrap();
        let b = cat.stats.iter().find(|s| s.stat_id == "B").unwrap();
        // Both look strong univariately…
        assert!(a.lift > 1.5 && b.lift > 1.5);
        // …but B's independent (conditional) lift collapses to ~1, A's stays high.
        assert!(a.conditional_lift.unwrap() > 1.5);
        assert!(b.conditional_lift.unwrap() < 1.3, "co-traveler should deconfound to ~1: {:?}", b.conditional_lift);
        // Deconfounded ranking puts A ahead of B.
        let pos = |id: &str| cat.stats.iter().position(|s| s.stat_id == id).unwrap();
        assert!(pos("A") < pos("B"));
    }
```

- [ ] **Step 3: Run both to confirm they fail**

Run: `cargo test build_recovers_a_planted_driver deconfounding_collapses 2>&1 | tail -8`
Expected: compile errors (`stats`, `conditional_lift`, `drivers`, etc. missing).

- [ ] **Step 4: Add `StatValue`, `ModPair`, extend `CategoryModel`, add constants**

In `src/trade/value.rs`, add the constants near the others:

```rust
/// A stat needs at least this many listings before its lift is trusted (drives
/// pricing; gates the conditional-lift computation).
pub const MIN_STAT_SAMPLE: usize = 15;
/// A trusted stat with lift at or above this is a value-driver.
pub const DRIVER_LIFT: f64 = 1.5;
/// How many co-occurrence pairs to retain per category.
const TOP_COOCCURRENCE: usize = 8;
```

Add the structs:

```rust
/// Per-stat value signal within a category.
#[derive(Debug, Default, Clone)]
pub struct StatValue {
    pub stat_id: String,
    pub label: Option<String>,
    pub count: usize,
    pub median_with: f64,
    /// Univariate lift = median_with / base_median. Used by pricing feedback.
    pub lift: f64,
    /// Lift conditioned on the higher-ranked drivers being absent — deconfounded.
    /// `None` when the driver-free subset was too thin to compute. Insights only.
    pub conditional_lift: Option<f64>,
    pub top_decile_freq: f64,
}

/// A pair of stats frequently co-occurring on the expensive tail.
#[derive(Debug, Default, Clone)]
pub struct ModPair {
    pub a: String,
    pub b: String,
    pub count: usize,
}
```

Extend `CategoryModel`:

```rust
#[derive(Debug, Default, Clone)]
pub struct CategoryModel {
    pub category: String,
    pub sample_size: usize,
    pub base_median: f64,
    /// Stats in deconfounded rank order (drivers first).
    pub stats: Vec<StatValue>,
    pub cooccurrences: Vec<ModPair>,
}

impl CategoryModel {
    /// Trusted value-drivers (high lift, enough samples), in deconfounded order.
    pub fn drivers(&self) -> impl Iterator<Item = &StatValue> {
        self.stats
            .iter()
            .filter(|s| s.count >= MIN_STAT_SAMPLE && s.lift >= DRIVER_LIFT)
    }
}
```

- [ ] **Step 5: Compute per-stat metrics + co-occurrence in `build`**

Replace the per-category body in `ValueModel::build` so it retains each observation (not just the price). Restructure the grouping to keep `&Observation`s:

```rust
    pub fn build(observations: &[Observation]) -> ValueModel {
        let mut by_cat: HashMap<String, Vec<&Observation>> = HashMap::new();
        for o in observations {
            let Some(raw) = o.category.as_deref() else { continue };
            by_cat.entry(canonical_category(raw)).or_default().push(o);
        }
        let mut categories = HashMap::new();
        for (category, obs) in by_cat {
            categories.insert(category.clone(), build_category(category, &obs));
        }
        ValueModel { categories }
    }
```

Add the per-category builder (free function in the same file):

```rust
fn build_category(category: String, obs: &[&Observation]) -> CategoryModel {
    let sample_size = obs.len();
    let prices: Vec<f64> = obs.iter().map(|o| o.price_divine).collect();
    let base_median = median(&prices);

    // Distinct stats and the prices of listings carrying each.
    let mut prices_with: HashMap<&str, Vec<f64>> = HashMap::new();
    for o in obs {
        let mut seen = std::collections::HashSet::new();
        for m in &o.mods {
            if seen.insert(m.stat_id.as_str()) {
                prices_with
                    .entry(m.stat_id.as_str())
                    .or_default()
                    .push(o.price_divine);
            }
        }
    }

    // Top decile (most expensive ~10%, at least 1) for frequency + co-occurrence.
    let mut by_price: Vec<&&Observation> = obs.iter().collect();
    by_price.sort_by(|a, b| {
        b.price_divine
            .partial_cmp(&a.price_divine)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let decile_n = (sample_size as f64 * 0.10).ceil() as usize;
    let decile_n = decile_n.max(1).min(sample_size);
    let top: Vec<&&Observation> = by_price.into_iter().take(decile_n).collect();

    let mut top_count: HashMap<&str, usize> = HashMap::new();
    for o in &top {
        let mut seen = std::collections::HashSet::new();
        for m in &o.mods {
            if seen.insert(m.stat_id.as_str()) {
                *top_count.entry(m.stat_id.as_str()).or_default() += 1;
            }
        }
    }

    let mut stats: Vec<StatValue> = prices_with
        .iter()
        .map(|(id, with)| {
            let median_with = median(with);
            let lift = if base_median > 0.0 { median_with / base_median } else { 1.0 };
            let top_decile_freq = *top_count.get(*id).unwrap_or(&0) as f64 / decile_n as f64;
            StatValue {
                stat_id: (*id).to_string(),
                label: None,
                count: with.len(),
                median_with,
                lift,
                conditional_lift: None,
                top_decile_freq,
            }
        })
        .collect();

    // Co-occurrence pairs among the top decile (unordered, stable key order).
    let mut pair_count: HashMap<(String, String), usize> = HashMap::new();
    for o in &top {
        let mut ids: Vec<&str> = o.mods.iter().map(|m| m.stat_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                *pair_count
                    .entry((ids[i].to_string(), ids[j].to_string()))
                    .or_default() += 1;
            }
        }
    }
    let mut cooccurrences: Vec<ModPair> = pair_count
        .into_iter()
        .map(|((a, b), count)| ModPair { a, b, count })
        .collect();
    cooccurrences.sort_by(|x, y| y.count.cmp(&x.count).then(x.a.cmp(&y.a)).then(x.b.cmp(&y.b)));
    cooccurrences.truncate(TOP_COOCCURRENCE);

    // Deconfounded ranking (Step 6 fills conditional_lift + final order).
    rank_deconfounded(&mut stats, obs, base_median);

    CategoryModel { category, sample_size, base_median, stats, cooccurrences }
}
```

- [ ] **Step 6: Implement the greedy deconfounding ranker**

Add to `src/trade/value.rs`:

```rust
/// Greedy deconfounding: rank drivers so a mod that only co-travels with a
/// stronger driver is demoted. Picks the highest-lift trusted stat, then
/// recomputes remaining stats' lift restricted to listings carrying none of the
/// already-picked drivers, and repeats. Fills `conditional_lift` and reorders
/// `stats` (drivers first, in deconfounded order; the rest by raw lift after).
/// Used for /insights ranking only — pricing reads raw `lift`.
fn rank_deconfounded(stats: &mut Vec<StatValue>, obs: &[&Observation], base_median: f64) {
    let trusted = |s: &StatValue| s.count >= MIN_STAT_SAMPLE && s.lift >= DRIVER_LIFT;
    let mut picked: Vec<String> = Vec::new();
    let mut ordered: Vec<StatValue> = Vec::new();
    let mut remaining: Vec<StatValue> = std::mem::take(stats);

    loop {
        // Listings carrying none of the already-picked drivers.
        let subset: Vec<&&Observation> = obs
            .iter()
            .filter(|o| !picked.iter().any(|d| o.mods.iter().any(|m| &m.stat_id == d)))
            .collect();
        let subset_median = median(
            &subset.iter().map(|o| o.price_divine).collect::<Vec<_>>(),
        );

        // Best remaining trusted stat by conditional lift over the subset.
        let mut best: Option<(usize, f64)> = None;
        for (i, s) in remaining.iter().enumerate() {
            if !trusted(s) {
                continue;
            }
            let with: Vec<f64> = subset
                .iter()
                .filter(|o| o.mods.iter().any(|m| m.stat_id == s.stat_id))
                .map(|o| o.price_divine)
                .collect();
            if with.len() < MIN_STAT_SAMPLE || subset_median <= 0.0 {
                continue;
            }
            let cl = median(&with) / subset_median;
            if best.map_or(true, |(_, bcl)| cl > bcl) {
                best = Some((i, cl));
            }
        }

        match best {
            Some((i, cl)) if cl >= DRIVER_LIFT => {
                let mut s = remaining.remove(i);
                s.conditional_lift = Some(cl);
                picked.push(s.stat_id.clone());
                ordered.push(s);
            }
            _ => break, // no remaining trusted stat clears the bar over the subset
        }
    }

    // Append the rest by descending raw lift (conditional_lift left as None).
    remaining.sort_by(|a, b| {
        b.lift
            .partial_cmp(&a.lift)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ordered.extend(remaining);
    let _ = base_median; // base_median is implicit in each StatValue.lift already
    *stats = ordered;
}
```

- [ ] **Step 7: Run the Task-2 model tests**

Run: `cargo test build_recovers_a_planted_driver deconfounding_collapses 2>&1 | tail -8`
Expected: both pass.

- [ ] **Step 8: Write the failing test for `StatCatalog::label_for`**

In `src/trade/stats.rs` tests (the `cat()` helper exists):

```rust
    #[test]
    fn label_for_reverses_id_to_text() {
        let c = cat();
        // stats_sample.json maps "+40 to maximum Life" -> explicit.stat_3299347043
        assert_eq!(
            c.label_for("explicit.stat_3299347043"),
            Some("+# to maximum Life")
        );
        assert_eq!(c.label_for("explicit.stat_nope"), None);
    }
```

(If the exact display text in the fixture differs, set the expected string to the fixture's `text` for that id — open `src/trade/fixtures/stats_sample.json` and read the `text` field paired with `explicit.stat_3299347043`.)

- [ ] **Step 9: Run it to confirm it fails**

Run: `cargo test label_for_reverses_id 2>&1 | tail -5`
Expected: compile error `no method named label_for`.

- [ ] **Step 10: Implement `label_for` (store a reverse map at build)**

In `src/trade/stats.rs`, add a field to `StatCatalog` and populate it in `from_json`:

```rust
pub struct StatCatalog {
    groups: HashMap<StatGroup, HashMap<String, String>>,
    /// stat_id -> original display text, for reverse lookup in /insights.
    labels: HashMap<String, String>,
}
```

In `from_json`, alongside inserting into `map`, also record the label (first id wins, matching the existing collision rule):

```rust
                for e in &g.entries {
                    map.entry(normalize(&e.text)).or_insert_with(|| e.id.clone());
                    labels.entry(e.id.clone()).or_insert_with(|| e.text.clone());
                }
```

Declare `let mut labels: HashMap<String, String> = HashMap::new();` before the loop, return `StatCatalog { groups, labels }`, and update the `Default`/struct construction sites (the `#[derive(Default)]` covers `default()`; the explicit `StatCatalog { groups }` in `from_json` becomes `StatCatalog { groups, labels }`). Add the method:

```rust
    /// Reverse lookup: the display text for a stat id, if known.
    pub fn label_for(&self, stat_id: &str) -> Option<&str> {
        self.labels.get(stat_id).map(String::as_str)
    }
```

- [ ] **Step 11: Run the label test + full build**

Run: `cargo test label_for_reverses_id 2>&1 | tail -5`
Expected: pass.
Run: `cargo build 2>&1 | tail -5`
Expected: compiles (`label_for` is called from `/insights` in Step 12 within this task).

- [ ] **Step 12: Enrich `/insights` to render drivers + co-occurrence (real reader of new fields)**

Replace the post-trust block in `src/discord/insights.rs` (the "Driver insights coming online" path) with a driver render. Drivers carry stat ids; resolve labels via the pricer's catalog through a new accessor — add to `src/trade/mod.rs`:

```rust
impl<C: Comparables> TradePricer<C> {
    /// Read access to the stat catalog (for /insights label resolution).
    pub fn catalog(&self) -> &StatCatalog {
        &self.catalog
    }
}
```

Then in `insights.rs`, after confirming `cat.sample_size >= MIN_CATEGORY_SAMPLE`:

```rust
    let catalog = ctx.data().pricer.catalog();
    let label = |id: &str| -> String {
        catalog.label_for(id).unwrap_or(id).to_string()
    };

    let mut body = format!(
        "**{canon}** — {} listings · median {:.1} div\n\n**Value drivers** (independent lift in parens):\n",
        cat.sample_size, cat.base_median
    );
    let mut any = false;
    for s in cat.drivers().take(8) {
        any = true;
        let cond = match s.conditional_lift {
            Some(c) => format!(" (independent {c:.1}×)"),
            None => String::new(),
        };
        body.push_str(&format!(
            "• **{}** — {:.1}×{} · in {:.0}% of priciest · n={}\n",
            label(&s.stat_id),
            s.lift,
            cond,
            s.top_decile_freq * 100.0,
            s.count
        ));
    }
    if !any {
        body.push_str("_(no mod clears the value-driver threshold yet)_\n");
    }
    if !cat.cooccurrences.is_empty() {
        body.push_str("\n**Top combos on expensive items:**\n");
        for p in cat.cooccurrences.iter().take(5) {
            body.push_str(&format!(
                "• {} + {} ({}×)\n",
                label(&p.a),
                label(&p.b),
                p.count
            ));
        }
    }
    ctx.say(body).await?;
    Ok(())
```

Remove the now-replaced placeholder `ctx.say(...)` for that branch. Drop the unused `model` read-lock borrow conflict by cloning what you need before resolving the catalog (the `cat` reference is borrowed from `model`; resolve labels into owned `String`s, then `drop(model)` is implicit at end of scope — if the borrow checker complains, clone `cat` via `cat.clone()` into an owned `CategoryModel` and `drop(model)` before reading the catalog).

- [ ] **Step 13: Run the suite, fmt, clippy**

Run: `cargo test 2>&1 | tail -6`
Expected: all pass.
Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | tail -8`
Expected: clean.

- [ ] **Step 14: Commit**

```bash
git add src/trade/value.rs src/trade/stats.rs src/trade/mod.rs src/discord/insights.rs
git commit -m "feat(value): driver metrics + deconfounded ranking + enriched /insights (Phase 4 Task 2)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: Pricing feedback — value-driver bands + learned relax order

Feed the model into `build_baseline`: value-drivers get a tighter band and survive relaxation longest (just before cornerstones); normal mods relax lowest-value-first. An empty/untrusted model must reproduce today's behavior exactly.

**Files:**
- Modify: `src/trade/query.rs` (`build_baseline` signature + ordering/band logic; add `driver_band`, `mod_strength`)
- Modify: `src/trade/mod.rs` (`TradePricer` holds `value`; `price()`/`breakdown()` pass it to `build_baseline`)
- Modify: `src/main.rs` (pass the shared `value` Arc into `TradePricer::new`)
- Test: inline in `src/trade/query.rs`

**Interfaces:**
- Consumes: `crate::trade::value::{ValueModel, canonical_category, MIN_CATEGORY_SAMPLE, MIN_STAT_SAMPLE, DRIVER_LIFT}`.
- Produces: `build_baseline(item, pseudo, catalog, value, league) -> TradeQuery` (new `value: &ValueModel` param, 4th position before `league`).

- [ ] **Step 1: Write the failing regression test — empty model ⇒ identical query**

In `src/trade/query.rs` tests (inspect the existing tests for a parsed-item fixture helper; reuse it — call it `staff_item()` here, substituting the real helper name):

```rust
    #[test]
    fn empty_model_reproduces_cold_start_query() {
        let item = staff_item(); // existing fixture helper
        let pseudo = PseudoMap::load();
        let catalog = StatCatalog::default();
        let empty = crate::trade::value::ValueModel::default();

        let q = build_baseline(&item, &pseudo, &catalog, &empty, "Standard");

        // Same stat ids, same order, same bands as the pre-feedback baseline.
        // Cornerstones first, then pseudo, then normals strongest→weakest by tier.
        let ids: Vec<&str> = q.stats.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, expected_cold_start_order()); // assert the exact vec
    }
```

Implement `expected_cold_start_order()` by capturing the *current* output before changing `build_baseline`: temporarily print `ids` from the existing behavior, paste the exact slice into the test as the expected value. (This pins today's behavior as the regression oracle.)

- [ ] **Step 2: Write the failing test — seeded model reorders + tightens drivers**

```rust
    #[test]
    fn trusted_model_makes_drivers_survive_and_tighten() {
        let item = staff_item();
        let pseudo = PseudoMap::load();
        let catalog = trade::stats::StatCatalog::from_json(include_str!(
            "fixtures/stats_sample.json"
        ))
        .unwrap();

        // Build a model where one of the item's matched stats is a strong driver
        // and the category is trusted. (Pick the stat id the catalog maps the
        // item's strongest explicit to; construct >= MIN_CATEGORY_SAMPLE obs.)
        let model = seed_staff_model_with_driver("explicit.stat_spell_dmg");

        let q = build_baseline(&item, &pseudo, &catalog, &model, "Standard");

        let driver_pos = q.stats.iter().position(|s| s.id == "explicit.stat_spell_dmg");
        // Driver present, and ordered ahead of (dropped later than) plain normals
        // but behind cornerstones. Its band is tighter than the loose default.
        let driver = q
            .stats
            .iter()
            .find(|s| s.id == "explicit.stat_spell_dmg")
            .expect("driver present");
        // Loose band for value v is [0.9v, 1.4v]; driver band [0.95v, 1.2v].
        // Assert the driver's max/min ratio is the tighter one.
        let ratio = driver.max.unwrap() / driver.min.unwrap();
        assert!(ratio < 1.35, "driver should use the tight band, got ratio {ratio}");
        assert!(driver_pos.is_some());
    }
```

(Provide `seed_staff_model_with_driver(stat_id)` as a test helper that builds a `ValueModel` via `ValueModel::build` over a synthetic `Staff` corpus where that stat is expensive — reuse the Task-2 `ob` pattern; ensure `sample_size >= MIN_CATEGORY_SAMPLE` and the driver `count >= MIN_STAT_SAMPLE` with `lift >= DRIVER_LIFT`.)

- [ ] **Step 3: Run both to confirm they fail**

Run: `cargo test empty_model_reproduces trusted_model_makes_drivers 2>&1 | tail -8`
Expected: compile error — `build_baseline` arity mismatch.

- [ ] **Step 4: Add `driver_band` and `mod_strength` helpers**

In `src/trade/query.rs`, near `band`:

```rust
/// Tighter band for learned value-drivers: keeps the price-defining combo
/// constrained. With BAND_K_DRIVER=0.25, BAND_PCTL=0.2 → [0.95·v, 1.2·v].
const BAND_K_DRIVER: f64 = 0.25;

fn driver_band(v: f64) -> (Option<f64>, Option<f64>) {
    let lo = (v * (1.0 - BAND_PCTL * BAND_K_DRIVER)).round();
    let hi = (v * (1.0 + (1.0 - BAND_PCTL) * BAND_K_DRIVER)).round();
    (Some(lo), Some(hi))
}

/// Relaxation strength for a normal (non-cornerstone) mod. Higher = kept longer.
/// Trusted lift when known; otherwise a small cold-start score from the tier
/// (stronger tier = higher), kept below trusted lifts so unknown mods relax
/// first. Drivers (lift >= DRIVER_LIFT) naturally sort to the front of normals.
fn mod_strength(trusted_lift: Option<f64>, tier: Option<u8>) -> f64 {
    match trusted_lift {
        Some(lift) => lift,
        None => -1.0 - (tier.unwrap_or(u8::MAX) as f64) / 1000.0,
    }
}
```

- [ ] **Step 5: Thread the model through `build_baseline` and apply feedback**

Change the signature:

```rust
pub fn build_baseline(
    item: &ParsedItem,
    pseudo: &PseudoMap,
    catalog: &StatCatalog,
    value: &crate::trade::value::ValueModel,
    league: &str,
) -> TradeQuery {
```

Resolve the trusted model for this item near the top (after `all_stats`):

```rust
    // Learned value for this item's canonical category, only if trusted.
    let cat_model = item
        .item_class
        .as_deref()
        .map(crate::trade::value::canonical_category)
        .and_then(|c| value.category(&c).cloned())
        .filter(|m| m.sample_size >= crate::trade::value::MIN_CATEGORY_SAMPLE);
```

In the per-mod loop that builds `mod_filters`, compute each normal mod's trusted lift and choose its band. Locate the branch that currently does `let (min, max) = if corner { (m.value, None) } else { band(m.value...) }`. Replace the non-cornerstone band selection with:

```rust
            let stat_lift = cat_model.as_ref().and_then(|cm| {
                cm.stats
                    .iter()
                    .find(|s| s.stat_id == id && s.count >= crate::trade::value::MIN_STAT_SAMPLE)
                    .map(|s| s.lift)
            });
            let is_driver = stat_lift.is_some_and(|l| l >= crate::trade::value::DRIVER_LIFT);
            let (min, max) = if corner {
                (Some(m.value), None)
            } else if is_driver {
                driver_band(m.value)
            } else {
                band(m.value)
            };
```

(Match the exact existing shape for the cornerstone arm — if the current code uses `(m.value, None)` with `min: Option<f64>`, keep that; adapt the `Some(...)` wrapping to whatever `StatFilter.min` expects.)

Carry the strength for ordering. Change the tagged tuple from `(bool, Option<u8>, StatFilter)` to also hold the strength, e.g. `(bool, f64, StatFilter)`, where the f64 is `mod_strength(stat_lift, m.tier)` for normals. Then replace the sort:

```rust
    // Order: cornerstones first (dropped last). Among normals, strongest first
    // (highest learned lift / tier score) so the weakest relaxes first; drivers,
    // having the highest lift, sit at the front of the normals and survive
    // longest before cornerstones. With an empty/untrusted model every normal
    // falls to the tier-based cold-start score, reproducing today's order.
    mod_filters.sort_by(|(ca, sa, _), (cb, sb, _)| {
        cb.cmp(ca) // cornerstones (true) before normals (false)
            .then(sb.partial_cmp(sa).unwrap_or(std::cmp::Ordering::Equal))
    });
```

**Important — preserve the empty-model order exactly.** Today's order sorts normals by `tier ascending` (strongest tier first). The cold-start `mod_strength(None, tier) = -1.0 - tier/1000` is *descending in strength as tier rises*, i.e. tier 1 → -1.001 (strongest), tier 5 → -1.005, unknown → -1 - 65.535. Sorting by strength **descending** puts tier 1 first, then 2, … then unknown last — identical to the prior `tier.unwrap_or(MAX)` ascending sort. Verify this against `expected_cold_start_order()`; if the tie-breaking differs, adjust `mod_strength` so the empty-model test passes (it is the oracle).

Keep the existing cornerstone/pseudo partition-and-append logic the same (cornerstones ahead of pseudo, pseudo in the middle, normals after).

- [ ] **Step 6: Update `price()` and `breakdown()` to pass the model**

In `src/trade/mod.rs`, add the field and constructor param:

```rust
pub struct TradePricer<C: Comparables> {
    comparables: C,
    pseudo: PseudoMap,
    catalog: StatCatalog,
    log: ObservationLog,
    value: std::sync::Arc<std::sync::RwLock<crate::trade::value::ValueModel>>,
}
```

```rust
    pub fn new(
        comparables: C,
        pseudo: PseudoMap,
        catalog: StatCatalog,
        log: ObservationLog,
        value: std::sync::Arc<std::sync::RwLock<crate::trade::value::ValueModel>>,
    ) -> Self {
        TradePricer { comparables, pseudo, catalog, log, value }
    }
```

In `price()` and `breakdown()`, read the model and pass it:

```rust
        let model = self.value.read().unwrap_or_else(|e| e.into_inner());
        let query = build_baseline(item, &self.pseudo, &self.catalog, &model, league);
        drop(model);
```

- [ ] **Step 7: Update `main.rs` and all `TradePricer::new` call sites**

In `src/main.rs`, move the `value` Arc creation ABOVE the `pricer` construction and pass `value.clone()` into `TradePricer::new(...)`. The startup `rebuild_into`, `spawn_value_refresher`, and `Data { value: value.clone() }` continue to share the same Arc.

Update every other `TradePricer::new(...)` call (the harvest tests in `src/trade/mod.rs` construct it directly) to pass a fresh `std::sync::Arc::new(std::sync::RwLock::new(crate::trade::value::ValueModel::default()))` as the final argument.

- [ ] **Step 8: Run the targeted tests**

Run: `cargo test empty_model_reproduces trusted_model_makes_drivers 2>&1 | tail -8`
Expected: both pass. If `empty_model_reproduces` fails, the strength ordering diverged from today — fix `mod_strength`/sort until the captured oracle matches.

- [ ] **Step 9: Full suite, fmt, clippy**

Run: `cargo test 2>&1 | tail -6`
Expected: all pass.
Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | tail -8`
Expected: clean.

- [ ] **Step 10: Commit**

```bash
git add src/trade/query.rs src/trade/mod.rs src/main.rs
git commit -m "feat(value): learned value-driver bands + relax order in price-check (Phase 4 Task 3)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review (completed)

- **Spec coverage:** canonical category (Task 1) ✓; corpus read path (Task 1) ✓; ValueModel lift/top-decile/co-occurrence (Task 2) ✓; deconfounded conditional-lift ranking, insights-only (Task 2) ✓; trust gates (Tasks 1–3 constants) ✓; refresh startup/periodic/post-harvest (Task 1) ✓; `/insights` everyone, raw+conditional lift visible (Tasks 1–2) ✓; pricing feedback relax-order + driver bands with empty-model regression (Task 3) ✓; best-effort error handling (Tasks 1, 3) ✓; the deconfounding confound test, planted-driver test, empty-model regression test (Tasks 2–3) ✓.
- **Placeholder scan:** no TBD/TODO; every code step carries real code. Two steps require the implementer to *capture an oracle from current behavior* (`expected_cold_start_order`) and *read a fixture value* (`label_for` expected text) — these are explicit, bounded instructions, not vague placeholders.
- **Type consistency:** `ValueModel`/`CategoryModel`/`StatValue`/`ModPair`, `canonical_category`, `rebuild_into`, `MIN_CATEGORY_SAMPLE`/`MIN_STAT_SAMPLE`/`DRIVER_LIFT`/`VALUE_REFRESH_MINS`, `build_baseline` 5-arg signature, and `TradePricer::new` 5-arg signature are used consistently across tasks.
- **Dead-code safety:** each task ends integration-complete — Task 1's new symbols are called from startup/refresher/harvest/`/insights`; Task 2's new fields are read by enriched `/insights`; Task 3's param is consumed by `price()`/`breakdown()`.

## Deviations from spec (intentional, low-risk)

- Thresholds are module `const`s, not env vars (spec said "env-overridable"). Simpler, fewer Config changes; promote to env later if tuning demands it.
- The "±9% / ±18%" band tiers are realized through the existing band formula as two `BAND_K` values (driver 0.25 → [0.95v,1.2v]; normal 0.5 → [0.9v,1.4v]) — the faithful mechanism, not literal percentages.
