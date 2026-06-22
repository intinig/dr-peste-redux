# Phase 2 — Durable Observation Corpus Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist every comparable the price-check fetches as a per-listing market observation (`{base, category, mods:[{stat_id,tier,roll}], price, source}`) to an append-only JSONL on a host-mounted Docker volume, so the Phase 3 harvester and Phase 4 learning layer have a durable corpus to mine.

**Architecture:** Extend `parse_fetch` so each `Listing` carries its mods with tier+roll; replace the per-aggregate `Probe`/`ProbeLog` with a per-listing `Observation`/`ObservationLog`; `price()` logs every comparable it read (source `Paste`); the log path is configurable (`OBSERVATION_LOG_PATH`) and the container mounts a volume so it survives deploys.

**Tech Stack:** Rust; existing append-only-JSONL pattern (`pricelog.rs`), `serde`/`serde_json`; terraform (kreuzwerker/docker) for the volume.

**Design spec:** `docs/superpowers/specs/2026-06-22-pricing-heuristic-and-market-learning-design.md` (Phase 2 — "Durable observation corpus").

> **Sequencing:** branch this off `main` AFTER Phase 1 (PR #16) merges — it builds on Phase 1's `price_check`/`Listing`/`parse_fetch`.

## Global Constraints

- **Atomic observation = one real listing:** `Observation { timestamp_unix, league, base_type, category, mods: Vec<ObsMod{stat_id, tier: Option<u8>, roll: Option<f64>}>, price_divine, source }`, `source ∈ {Paste, Harvest}` (Harvest is wired in Phase 3; Phase 2 only emits `Paste`).
- **`category` = the clipboard `Item Class:`** (the parser's `item_class`); `base_type` = the parsed base. No new base→category table.
- **Never persist secrets:** member POESESSIDs are never written; only non-secret market data.
- **Best-effort logging:** a log write failure is a `tracing::warn!`, never a panic and never blocks pricing (preserve the current `record` behavior).
- **Durability:** the log path comes from `OBSERVATION_LOG_PATH` (default `observations.jsonl`); production sets it to a path on a mounted volume so it survives container replacement.
- Binary crate, no lib target — verify with `cargo test` (never `--lib`). Final `cargo build` zero warnings; **CI runs `cargo clippy --all-targets -- -D warnings`** — run that exact command, must be clean.
- Commit trailer (after a blank line): `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Stage files by name; never `git add -A`.

## File structure

| File | Change |
|---|---|
| `src/trade/model.rs` | `Listing.explicit_stat_ids: Vec<String>` → `mods: Vec<ListingMod>` (`{stat_id, tier, roll}`) |
| `src/trade/client.rs` | `parse_fetch` builds `mods` (stat_id + tier from `mods[].tier`, roll from description) |
| `src/observe.rs` (**new**, replaces `src/pricelog.rs`) | `Observation`, `ObsMod`, `Source`, `ObservationLog` (append-only JSONL) |
| delete `src/pricelog.rs` | replaced by `observe.rs` |
| `src/trade/model.rs` | remove `Probe` (replaced by `Observation`) |
| `src/trade/mod.rs` | `price_check` returns the listings; `price()` logs an `Observation` per comparable; `TradePricer` holds an `ObservationLog` |
| `src/config.rs` | `OBSERVATION_LOG_PATH` (default `observations.jsonl`) |
| `src/main.rs` | build `ObservationLog` from config; wire into `TradePricer` |
| `src/lib`/`main` module decl | `mod observe;` replaces `mod pricelog;` |
| infra (terraform, deploy step) | mounted volume + `OBSERVATION_LOG_PATH` env |

---

## Task 1: `Listing` carries per-mod `{stat_id, tier, roll}`

**Files:**
- Modify: `src/trade/model.rs` (`Listing`; new `ListingMod`)
- Modify: `src/trade/client.rs` (`parse_fetch` + a tier helper + tests)
- Modify: `Listing { … }` literals in `src/trade/mod.rs` tests (compiler-guided)

**Interfaces:**
- Produces: `pub struct ListingMod { pub stat_id: String, pub tier: Option<u8>, pub roll: Option<f64> }` (derive `Clone, Debug, PartialEq, Serialize, Deserialize`); `Listing.mods: Vec<ListingMod>` replaces `explicit_stat_ids: Vec<String>`. `stat_id` is normalised `explicit.stat_*` (as `explicit_stat_ids` was).

- [ ] **Step 1: Write the failing extraction test**

In `src/trade/client.rs` `tests`, replace/extend the existing stat-id extraction test with one that also asserts tier + roll:

```rust
    #[test]
    fn parse_fetch_extracts_mods_with_tier_and_roll() {
        let client = test_client();
        let v = serde_json::json!({
            "result": [{
                "id": "abc123",
                "listing": { "price": { "amount": 1.0, "currency": "divine" } },
                "item": {
                    "explicitMods": [
                        { "name": "Sadistic", "hash": "stat.explicit.stat_2768835289",
                          "description": "123% increased Spell Physical Damage",
                          "mods": [ { "tier": "P5", "magnitudes": [ { "min": "109", "max": "128" } ] } ] }
                    ],
                    "extended": { "hashes": { "explicit": [["explicit.stat_2768835289", [0]]] } }
                }
            }]
        });
        let ls = client.parse_fetch(&v);
        assert_eq!(ls.len(), 1);
        assert_eq!(ls[0].mods.len(), 1);
        assert_eq!(ls[0].mods[0].stat_id, "explicit.stat_2768835289");
        assert_eq!(ls[0].mods[0].tier, Some(5));      // "P5" → 5
        assert_eq!(ls[0].mods[0].roll, Some(123.0));  // first number in the description
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test parse_fetch_extracts_mods_with_tier_and_roll`
Expected: compile error — `Listing` has no `mods`; `ListingMod` undefined.

- [ ] **Step 3: Add `ListingMod` + swap the `Listing` field**

In `src/trade/model.rs`:

```rust
/// One explicit mod on a fetched listing, for the observation corpus.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListingMod {
    /// Normalised stat id, e.g. `explicit.stat_2768835289`.
    pub stat_id: String,
    /// Affix tier number (1 = best); parsed from the fetch `tier` string (`"P5"`→5).
    pub tier: Option<u8>,
    /// The displayed rolled value (first number of the mod description).
    pub roll: Option<f64>,
}
```

Change `Listing`: replace `pub explicit_stat_ids: Vec<String>,` with `pub mods: Vec<ListingMod>,`.

- [ ] **Step 4: Build `mods` in `parse_fetch` + tier helper**

In `src/trade/client.rs`, replace the `explicit_stat_ids` free helper with one that returns `Vec<ListingMod>`:

```rust
/// Parses a fetch `tier` string like `"P5"`/`"S3"` → `5`/`3`.
fn parse_fetch_tier(t: &str) -> Option<u8> {
    let digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// First number in a mod description (the displayed roll), e.g.
/// "123% increased …" → 123.0; "Adds 5 to 10 …" → 5.0.
fn first_number(s: &str) -> Option<f64> {
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() || (c == '.' && !num.is_empty()) {
            num.push(c);
        } else if !num.is_empty() {
            break;
        }
    }
    num.parse().ok()
}

/// Per-listing explicit mods with stat id, tier, and rolled value. Stat id from
/// `explicitMods[].hash` (strip the `stat.` prefix); tier from `mods[0].tier`;
/// roll from the first number of the description.
fn listing_mods(item: &Value) -> Vec<ListingMod> {
    item.get("explicitMods")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let hash = m.get("hash").and_then(|h| h.as_str())?;
                    let stat_id = hash.strip_prefix("stat.").unwrap_or(hash).to_string();
                    let tier = m
                        .get("mods")
                        .and_then(|x| x.as_array())
                        .and_then(|a| a.first())
                        .and_then(|m0| m0.get("tier"))
                        .and_then(|t| t.as_str())
                        .and_then(parse_fetch_tier);
                    let roll = m
                        .get("description")
                        .and_then(|d| d.as_str())
                        .and_then(first_number);
                    Some(ListingMod { stat_id, tier, roll })
                })
                .collect()
        })
        .unwrap_or_default()
}
```

In `parse_fetch`, replace the `explicit_stat_ids` field population with:

```rust
                        let mods = item.map(listing_mods).unwrap_or_default();
```

and set `mods` in the `Listing { … }` literal (replacing `explicit_stat_ids`).

(`affix_count` still reads `extended.prefixes/suffixes`/`explicitMods.len()` for `explicit_count` — leave it; `explicit_count` stays.)

- [ ] **Step 5: Update `Listing` literals in tests**

Build fails at each test `Listing { … explicit_stat_ids: … }`. Replace with `mods: vec![]` (or appropriate `ListingMod`s) — these are in `src/trade/mod.rs` tests (`make_listing`, `Flat`). The `make_listing` helper signature can stay; just set `mods: vec![]`.

- [ ] **Step 6: Run to green**

Run: `cargo test client:: && cargo test` then `cargo build`
Expected: the new test passes; the existing `parse_fetch` currency-drop test still passes; whole suite green; zero warnings.

- [ ] **Step 7: Format, strict clippy, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/trade/model.rs src/trade/client.rs src/trade/mod.rs
git commit -m "feat(trade): listings carry per-mod stat_id+tier+roll from fetch"
# + trailer
```

---

## Task 2: `Observation` + `ObservationLog` (replace `Probe`/`ProbeLog`)

**Files:**
- Create: `src/observe.rs`
- Delete: `src/pricelog.rs`
- Modify: `src/trade/model.rs` (remove `Probe`)
- Modify: the crate module declaration (`mod observe;` replacing `mod pricelog;` in `src/main.rs` or wherever `mod pricelog;` is declared)

**Interfaces:**
- Consumes: `crate::trade::model::ListingMod` (Task 1).
- Produces:
  - `pub enum Source { Paste, Harvest }` (Serialize/Deserialize, e.g. `#[serde(rename_all = "lowercase")]`).
  - `pub struct Observation { pub timestamp_unix: u64, pub league: String, pub base_type: Option<String>, pub category: Option<String>, pub mods: Vec<ListingMod>, pub price_divine: f64, pub source: Source }` (Serialize/Deserialize).
  - `pub struct ObservationLog { … }` with `pub fn new(path: impl AsRef<Path>) -> Self` and `pub fn append(&self, obs: &Observation) -> Result<()>` (append-only JSONL, mutex-guarded — same shape as `ProbeLog`).

- [ ] **Step 1: Write the failing round-trip test**

Create `src/observe.rs` with the test first (and stubs that won't compile yet, so RED is real):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::ListingMod;

    fn obs(price: f64) -> Observation {
        Observation {
            timestamp_unix: 0,
            league: "Standard".into(),
            base_type: Some("Chiming Staff".into()),
            category: Some("Staves".into()),
            mods: vec![ListingMod { stat_id: "explicit.stat_1".into(), tier: Some(2), roll: Some(123.0) }],
            price_divine: price,
            source: Source::Paste,
        }
    }

    #[test]
    fn appends_one_json_line_per_observation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obs.jsonl");
        let log = ObservationLog::new(&path);
        log.append(&obs(10.0)).unwrap();
        log.append(&obs(20.0)).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        // Round-trips back to the same struct.
        let back: Observation = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(back, obs(10.0));
        assert!(lines[0].contains("\"source\":\"paste\""));
        assert!(lines[1].contains("Staves"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test observe::`
Expected: compile error — `Observation`/`Source`/`ObservationLog` not defined.

- [ ] **Step 3: Implement `observe.rs`**

Above the tests in `src/observe.rs`:

```rust
//! Append-only JSONL corpus of per-listing market observations — the data the
//! learning layer mines. Market data only; never any Discord/member secret.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::trade::model::ListingMod;

/// Where an observation came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Paste,
    Harvest,
}

/// One real market listing, captured for the learning corpus.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub timestamp_unix: u64,
    pub league: String,
    pub base_type: Option<String>,
    pub category: Option<String>,
    pub mods: Vec<ListingMod>,
    pub price_divine: f64,
    pub source: Source,
}

/// Append-only JSONL log of observations. Mutex-guarded; failures are returned,
/// never panicked, so the caller can downgrade to a warning.
pub struct ObservationLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl ObservationLog {
    pub fn new(path: impl AsRef<Path>) -> Self {
        ObservationLog {
            path: path.as_ref().to_path_buf(),
            lock: Mutex::new(()),
        }
    }

    pub fn append(&self, obs: &Observation) -> Result<()> {
        let line = serde_json::to_string(obs)?;
        let _guard = self.lock.lock().unwrap();
        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(f, "{line}")?;
        Ok(())
    }
}
```

- [ ] **Step 4: Swap the module declaration and delete `pricelog.rs`**

Find `mod pricelog;` (or `pub mod pricelog;`) in `src/main.rs` and replace with `mod observe;` (match the existing visibility). `Probe` is removed in Step 5; `ProbeLog` usages are rewired in Task 3 — until then the crate won't fully compile, which is expected mid-task. Do NOT delete `pricelog.rs` until its last referencer (`TradePricer`) is rewired; to keep this task self-contained, delete `pricelog.rs` and `Probe` here and let Task 3's `TradePricer` change land in the SAME task if the build can't be green otherwise.

**Reconcile:** because `TradePricer` currently holds a `ProbeLog` and calls `record`/`Probe`, removing `pricelog.rs`/`Probe` now breaks `mod.rs` and `main.rs`. To keep each task green, **fold Task 3 into this task** (they are interdependent: the log type swap and its sole consumer must change together). Proceed to Task 3's steps before building/committing; commit both together at the end of Task 3.

- [ ] **Step 5: Remove `Probe`**

Delete the `Probe` struct from `src/trade/model.rs` and any `Probe` import. (`ObservationLog`/`Observation` replace it.)

(Continue into Task 3 — do not build/commit yet.)

---

## Task 3: `price()` logs observations; config + wiring

**(Folded with Task 2 — same commit, to keep the build green across the `ProbeLog`→`ObservationLog` swap.)**

**Files:**
- Modify: `src/trade/mod.rs` (`TradePricer` holds `ObservationLog`; `price_check` returns listings; `price()` logs observations; remove `record`/`Probe`)
- Modify: `src/config.rs` (`OBSERVATION_LOG_PATH`)
- Modify: `src/main.rs` (build `ObservationLog` from config; wire into `TradePricer::new`)

**Interfaces:**
- Consumes: `ObservationLog`/`Observation`/`Source`/`ListingMod` (Tasks 1–2).
- Produces: `price_check(...) -> Result<(PriceEstimate, Vec<Listing>)>`; `TradePricer::new(comparables, pseudo, catalog, log: ObservationLog)`.

- [ ] **Step 1: Write the failing test (price logs one observation per comparable)**

In `src/trade/mod.rs` `tests`, update `make_pricer` to pass an `ObservationLog` (it already creates a temp dir) and add:

```rust
    #[tokio::test]
    async fn price_logs_one_observation_per_comparable() {
        struct Comps;
        #[async_trait]
        impl Comparables for Comps {
            async fn comparables(&self, _q: &TradeQuery, _l: usize, _mr: usize, _s: &TradeSession)
                -> anyhow::Result<Vec<Listing>> {
                Ok((1..=5).map(|i| make_listing(i as f64, 1, &format!("c{i}"))).collect())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obs.jsonl");
        let pricer = TradePricer::new(
            Comps,
            crate::trade::pseudo::PseudoMap::load(),
            crate::trade::stats::StatCatalog::default(),
            crate::observe::ObservationLog::new(&path),
        );
        let est = pricer.price(&ring(), "Standard", &TradeSession::for_test()).await.unwrap();
        assert!(est.typical > 0.0);
        let lines = std::fs::read_to_string(&path).unwrap();
        assert_eq!(lines.lines().count(), 5); // one observation per comparable
        assert!(lines.contains("\"source\":\"paste\""));
        assert!(lines.contains("Sapphire Ring")); // base_type from the parsed item
    }
```

(Update the existing `make_pricer` helper to construct `ObservationLog` instead of `ProbeLog`, and update `price_reads_percentiles_over_comparables_no_progress_arg` / `price_logs_a_probe_and_returns_estimate` — rename the latter or adjust its assertion to read the observation log; keep one test that asserts the estimate value.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test price_logs_one_observation_per_comparable`
Expected: compile error — `TradePricer::new` still takes `ProbeLog`; `price_check` returns only `PriceEstimate`.

- [ ] **Step 3: `price_check` returns the listings it priced over**

`price_check` is now exact-first then relax (merged in PR #16). Change it to return
the listing set it actually used (the exact set, or the relaxed set) alongside the
estimate, so `price()` can log exactly those comparables:

```rust
pub async fn price_check<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    max_relax: usize,
    session: &TradeSession,
) -> Result<(PriceEstimate, Vec<Listing>)> {
    // Exact (no relaxation): a full-constraint match with enough comparables is
    // precise and deserves count-based confidence.
    let exact = c.comparables(query, limit, 0, session).await?;
    if exact.len() >= MIN_COMPARABLES {
        let est = estimate_from(&exact, EstimateBasis::CraftTier);
        return Ok((est, exact));
    }
    // Too thin at full constraint → relax and price the broader set (low confidence).
    let relaxed = c.comparables(query, limit, max_relax, session).await?;
    let est = estimate_from(&relaxed, EstimateBasis::BroadMarket);
    Ok((est, relaxed))
}
```

Update the two existing `price_check` tests in `ablation.rs`
(`price_check_relaxed_result_is_broad_market_low_confidence`,
`price_check_exact_match_is_craft_tier`) to destructure the `(est, _listings)` tuple.

- [ ] **Step 4: `TradePricer` holds `ObservationLog`; `price()` logs**

In `src/trade/mod.rs`:
- Imports: replace `use crate::pricelog::ProbeLog;` with `use crate::observe::{Observation, ObservationLog, Source};`; drop `Probe` from the model import.
- `TradePricer.log: ObservationLog` (was `ProbeLog`); `new(...)` takes `log: ObservationLog`.
- Replace `price()` body's tail + the `record` helper with per-listing logging:

```rust
    pub async fn price(
        &self,
        item: &ParsedItem,
        league: &str,
        session: &TradeSession,
    ) -> Result<PriceEstimate> {
        let query = build_baseline(item, &self.pseudo, &self.catalog, league);
        let max_relax = query.stats.len();
        let (est, listings) =
            price_check(&self.comparables, &query, PRICE_SAMPLE, max_relax, session).await?;
        self.log_observations(item, league, &listings);
        Ok(est)
    }

    /// Append one `Observation { source: Paste }` per fetched comparable. Best-
    /// effort: a write failure is logged, never fatal.
    fn log_observations(&self, item: &ParsedItem, league: &str, listings: &[Listing]) {
        let timestamp_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        for l in listings {
            let obs = Observation {
                timestamp_unix,
                league: league.to_string(),
                base_type: item.base_type.clone(),
                category: item.item_class.clone(),
                mods: l.mods.clone(),
                price_divine: l.price_divine,
                source: Source::Paste,
            };
            if let Err(e) = self.log.append(&obs) {
                tracing::warn!(error = %e, "failed to append observation");
            }
        }
    }
```

Remove the old `record` method and the `Probe` usage. (`Listing` must be imported in `mod.rs`'s non-test scope now — add `use crate::trade::model::Listing;` if not already present.)

Update `breakdown()`: it currently calls `self.record(&query, &bd.baseline)`. Drop that line (breakdown no longer logs a probe; observations come from `price`). Confirm `breakdown` still compiles.

- [ ] **Step 5: Config + main wiring**

In `src/config.rs`, add to `Config`: `pub observation_log_path: String,` and in `from_lookup`:

```rust
        let observation_log_path = get("OBSERVATION_LOG_PATH")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "observations.jsonl".to_string());
```

(add it to the returned `Self { … }`). Add a test asserting the default and an override.

In `src/main.rs`, replace `ProbeLog::new("probes.jsonl")` with `crate::observe::ObservationLog::new(&config.observation_log_path)` and fix the import.

- [ ] **Step 6: Build, full suite, strict clippy**

Run: `cargo build` then `cargo test` then `cargo clippy --all-targets -- -D warnings`
Expected: zero warnings; suite green; clippy clean. `pricelog.rs` is gone, `Probe`/`ProbeLog` have no residual references.

- [ ] **Step 7: Format, commit (Tasks 2+3 together)**

```bash
cargo fmt
git add src/observe.rs src/trade/model.rs src/trade/mod.rs src/trade/ablation.rs src/config.rs src/main.rs
git rm src/pricelog.rs
git commit -m "feat(observe): durable per-listing observation corpus (replaces probe log)"
# + trailer
```

---

## Deploy step (controller, at merge — terraform volume)

Not an SDD code task; done in the **`terraformed-infrastructure`** repo at deploy:

- [ ] In `projects/dr-peste-redux/hetzner/main.tf`, add a host bind-mount to `docker_container.bot` and the env var:

```hcl
  volumes {
    host_path      = "/opt/dr-peste-redux/data"   # on the Hetzner box
    container_path = "/data"
  }

  env = [
    # … existing …
    "OBSERVATION_LOG_PATH=/data/observations.jsonl",
  ]
```

- [ ] Ensure `/opt/dr-peste-redux/data` exists on the box (created on first apply via the bind-mount, or `mkdir -p` over SSH).
- [ ] `terraform plan -out=deploy.tfplan` → review (expect container replacement + the new mount/env) → `terraform apply`.
- [ ] After deploy: `/paste` an item, then over SSH confirm `/opt/dr-peste-redux/data/observations.jsonl` has one JSON line per comparable, and that it **survives a subsequent deploy** (the whole point).

---

## Final verification (after all tasks)

- [ ] `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` green; `cargo build` zero warnings.
- [ ] No residual `Probe`/`ProbeLog`/`pricelog`/`explicit_stat_ids` references (`grep`).
- [ ] **Manual live acceptance** (after deploy): `/paste` writes N observations (N = comparables read) with `source:"paste"`, real `category`/`base_type`, and per-mod `stat_id`/`tier`/`roll`; the file is on the mounted volume and persists across a redeploy.
- [ ] Note for Phase 3: the harvester appends `Observation { source: Harvest }` to the same log via a price-banded category sweep.
