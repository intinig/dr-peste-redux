# Marginal-Contribution Rare Pricing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix "No comparable listings found" on heavily-modded rares by deriving the item's value from partial-overlap comparables (a marginal-contribution / hedonic model) when an exact-match search is too thin.

**Architecture:** Two paths in `TradePricer::price`. Fast path (exact query ≥ MIN comparables) is unchanged. Value path samples the market (base + base-plus-each-mod, throttle-paced), reads each comparable's mods by stat id from the fetch, fits `ln(price) ~ Σ mods-present` by hand-rolled OLS, and predicts the full item. A `PriceProgress` hook surfaces a latency estimate before the wait.

**Tech Stack:** Rust; existing `Comparables` seam + PR #13 throttle; no new dependencies (OLS is hand-rolled).

**Design spec:** `docs/superpowers/specs/2026-06-19-rare-pricing-marginal-contribution-design.md`.

## Global Constraints

- **Value model:** an item's worth is the combination of its mods weighted by each mod's inherent (market-learned) value; derive the full-item price from overlapping partials. Output is an **estimate with confidence**, never a hard promise.
- **Fast path stays one search** for common items; the value path's extra searches only fire when the exact query is thin, and are **throttle-paced** (never 429).
- **Affix explicits only** drive stat filters — runes, implicits, enchants, and granted-skill lines are excluded.
- **Never error to the user:** when the model can't fit, fall back to a base-tier percentile estimate with `Low` confidence.
- **Breakdown is out of scope** (unchanged in this plan).
- Binary crate, no lib target — verify with `cargo test` (never `--lib`); final `cargo build` **zero warnings**; `cargo clippy` clean.
- Commit trailer (after a blank line): `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Stage files by name; never `git add -A`.

## File structure

| File | Responsibility |
|---|---|
| `src/trade/model.rs` | `Listing` gains `id` + `explicit_stat_ids`; `EstimateBasis::Marginal`. |
| `src/trade/client.rs` | `parse_fetch` extracts listing id + explicit stat-id set. |
| `src/trade/query.rs` | `build_baseline` = affix explicits only; `base_query` helper. |
| `src/trade/hedonic.rs` (**new**) | Pure model: OLS, feature extraction, `model_price` → prediction interval. No I/O. |
| `src/trade/limiter.rs` | `RateLimiter::estimate(ep, n) -> Duration`. |
| `src/trade/ablation.rs` | `PriceProgress` trait + `NoProgress`; `marginal_estimate` orchestration. |
| `src/trade/mod.rs` | `price` routes fast vs value path; threads `PriceProgress`. |
| `src/discord/paste.rs` | `PriceProgress` impl editing the deferred reply; defer/edit flow. |
| `src/discord/embeds.rs` | label the `Marginal` basis. |

---

## Task 1: `Listing` carries listing id + explicit stat ids

**Files:**
- Modify: `src/trade/model.rs` (`Listing` struct)
- Modify: `src/trade/client.rs` (`parse_fetch` + a helper + tests)
- Modify: `src/trade/ablation.rs` (test helpers `listing`/`listing_ec`), `src/trade/mod.rs` (`Flat` test), `src/trade/model.rs` (its `parse_fetch`-adjacent test literal) — add the two new fields to every `Listing { … }` literal.

**Interfaces:**
- Produces: `Listing { price: Money, price_divine: f64, explicit_count: usize, id: String, explicit_stat_ids: Vec<String> }`. `explicit_stat_ids` are normalised ids like `"explicit.stat_2768835289"` (matching `StatFilter.id`).

- [ ] **Step 1: Write the failing extraction test**

In `src/trade/client.rs` `tests`, add:

```rust
    #[test]
    fn parse_fetch_extracts_id_and_stat_ids() {
        let client = test_client();
        let v = serde_json::json!({
            "result": [{
                "id": "abc123",
                "listing": { "price": { "amount": 1.0, "currency": "divine" } },
                "item": {
                    "explicitMods": [
                        { "hash": "stat.explicit.stat_2768835289", "mods": [] }
                    ],
                    "extended": {
                        "hashes": { "explicit": [["explicit.stat_2768835289", [0]],
                                                 ["explicit.stat_1050105434", [1]]] }
                    }
                }
            }]
        });
        let ls = client.parse_fetch(&v);
        assert_eq!(ls.len(), 1);
        assert_eq!(ls[0].id, "abc123");
        // Prefer extended.hashes.explicit (both ids), already normalised.
        assert_eq!(
            ls[0].explicit_stat_ids,
            vec!["explicit.stat_2768835289", "explicit.stat_1050105434"]
        );
    }

    #[test]
    fn parse_fetch_stat_ids_fall_back_to_explicit_mods_hash() {
        let client = test_client();
        let v = serde_json::json!({
            "result": [{
                "id": "x",
                "listing": { "price": { "amount": 1.0, "currency": "divine" } },
                "item": { "explicitMods": [ { "hash": "stat.explicit.stat_999" } ] }
            }]
        });
        let ls = client.parse_fetch(&v);
        // "stat." prefix stripped to match StatFilter ids.
        assert_eq!(ls[0].explicit_stat_ids, vec!["explicit.stat_999"]);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test parse_fetch_extracts_id_and_stat_ids parse_fetch_stat_ids_fall_back`
Expected: compile error — `Listing` has no `id`/`explicit_stat_ids`.

- [ ] **Step 3: Add the fields to `Listing`**

In `src/trade/model.rs` replace the `Listing` struct:

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct Listing {
    pub price: Money,
    /// Price normalized to Divine Orbs for comparison/ranking.
    pub price_divine: f64,
    /// Count of explicit (prefix/suffix) mods on the listed item; the
    /// craftability-tier key. `0` when the fetch response omits mods.
    pub explicit_count: usize,
    /// Trade listing id (dedup key when pooling several searches).
    pub id: String,
    /// Normalised explicit stat ids on the item (e.g. `explicit.stat_123`),
    /// for matching against our query's `StatFilter.id`.
    pub explicit_stat_ids: Vec<String>,
}
```

- [ ] **Step 4: Extract in `parse_fetch`**

In `src/trade/client.rs`, add a module-level helper near `affix_count`:

```rust
/// Normalised explicit stat ids for a fetched item: prefer
/// `extended.hashes.explicit` (already `explicit.stat_*`), else strip the
/// leading `stat.` from each `explicitMods[].hash`.
fn explicit_stat_ids(item: &Value) -> Vec<String> {
    if let Some(arr) = item
        .pointer("/extended/hashes/explicit")
        .and_then(|v| v.as_array())
    {
        let ids: Vec<String> = arr
            .iter()
            .filter_map(|pair| pair.get(0).and_then(|s| s.as_str()).map(String::from))
            .collect();
        if !ids.is_empty() {
            return ids;
        }
    }
    item.get("explicitMods")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("hash").and_then(|h| h.as_str()))
                .map(|h| h.strip_prefix("stat.").unwrap_or(h).to_string())
                .collect()
        })
        .unwrap_or_default()
}
```

In `parse_fetch`, inside the `filter_map` closure, after `explicit_count` is computed, build the new fields and add them to the `Listing`:

```rust
                        let item = entry.get("item");
                        let explicit_count = item.map(affix_count).unwrap_or(0);
                        // NB: `stat_ids` not `explicit_stat_ids` — avoid shadowing
                        // the free `explicit_stat_ids` fn used on the same line.
                        let stat_ids = item.map(explicit_stat_ids).unwrap_or_default();
                        let id = entry
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let money = Money {
                            amount,
                            currency: Self::parse_currency(code),
                        };
                        Some(Listing {
                            price: money,
                            price_divine,
                            explicit_count,
                            id,
                            explicit_stat_ids: stat_ids,
                        })
```

(The existing `let explicit_count = entry.get("item").map(affix_count)...` line is replaced by the `let item = …` block above.)

- [ ] **Step 5: Fix the other `Listing` literals**

Add `id: String::new(), explicit_stat_ids: vec![]` to each test literal:
- `src/trade/ablation.rs` `listing` (line ~310) and `listing_ec` (line ~321) helpers.
- `src/trade/mod.rs` `Flat::comparables` literal (line ~129).
- `src/trade/model.rs` test literal (line ~182).

Example for `listing`:

```rust
    fn listing(divine: f64) -> Listing {
        Listing {
            price: Money { amount: divine, currency: Currency::Divine },
            price_divine: divine,
            explicit_count: 0,
            id: String::new(),
            explicit_stat_ids: vec![],
        }
    }
```

- [ ] **Step 6: Run to green**

Run: `cargo test client:: && cargo test` then `cargo build`
Expected: new tests pass; existing `parse_fetch_*` tests still pass; whole suite green; zero warnings.

- [ ] **Step 7: Format, lint, commit**

```bash
cargo fmt && cargo clippy
git add src/trade/model.rs src/trade/client.rs src/trade/ablation.rs src/trade/mod.rs
git commit -m "feat(trade): carry listing id + explicit stat ids from fetch"
# + trailer
```

---

## Task 2: `build_baseline` — affix explicits only + `base_query` helper

**Files:**
- Modify: `src/trade/query.rs` (`build_baseline` + new `base_query` + tests)

**Interfaces:**
- Produces: `pub fn base_query(q: &TradeQuery) -> TradeQuery` — clones `q` with `stats` emptied (type + misc + equipment retained).
- Behaviour change: `build_baseline` resolves pseudo aggregates and per-mod filters from `item.explicits` **only** (runes/implicits/enchants no longer contribute filters).

- [ ] **Step 1: Write the failing tests**

In `src/trade/query.rs` `tests`, add (the staff has 6 explicit affixes + 2 rune lines + a granted-skill line):

```rust
    #[test]
    fn build_baseline_ignores_runes_and_implicits() {
        let item = ParsedItem {
            rarity: crate::itemtext::Rarity::Rare,
            name: "Onslaught Spell".into(),
            base_type: Some("Chiming Staff".into()),
            item_class: Some("Staves".into()),
            item_level: Some(80),
            quality: None,
            corrupted: false,
            energy_shield: None,
            armour: None,
            evasion: None,
            implicits: vec![ItemStat { raw: "10% increased Cast Speed".into(), value: Some(10.0), affix: None }],
            enchants: vec![],
            runes: vec![ItemStat { raw: "+1 to Level of all Spell Skills".into(), value: Some(1.0), affix: None }],
            explicits: vec![ItemStat {
                raw: "201% increased Spell Physical Damage".into(),
                value: Some(201.0),
                affix: Some(crate::itemtext::Affix::Prefix),
            }],
        };
        let catalog = StatCatalog::from_json(include_str!("fixtures/stats_sample.json")).unwrap();
        let q = build_baseline(&item, &PseudoMap::load(), &catalog, "Standard");
        // Only the explicit affix yields a filter (if the sample catalog matches it);
        // the rune and implicit never do.
        assert!(q.stats.iter().all(|s| !s.label.contains("all Spell Skills")));
        assert!(q.stats.iter().all(|s| !s.label.contains("increased Cast Speed")
            || s.label.contains("Spell"))); // no implicit cast-speed filter
    }

    #[test]
    fn base_query_clears_stats_keeps_type_and_misc() {
        let q = TradeQuery {
            league: "L".into(),
            category: None,
            type_line: Some("Chiming Staff".into()),
            stats: vec![StatFilter { id: "explicit.stat_1".into(), label: "x".into(), min: Some(1.0), max: Some(2.0) }],
            misc: MiscFilters { item_level_min: Some(80), quality_min: None, corrupted: Some(false) },
            equipment: vec![],
        };
        let b = base_query(&q);
        assert!(b.stats.is_empty());
        assert_eq!(b.type_line, q.type_line);
        assert_eq!(b.misc.item_level_min, Some(80));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test build_baseline_ignores_runes_and_implicits base_query_clears_stats`
Expected: FAIL — `base_query` undefined; build_baseline currently emits rune/implicit filters.

- [ ] **Step 3: Restrict `build_baseline` to explicits**

In `src/trade/query.rs` `build_baseline`, change the pseudo input and the buckets to explicits only. Replace:

```rust
    let all_stats: Vec<_> = item
        .implicits
        .iter()
        .chain(&item.enchants)
        .chain(&item.runes)
        .chain(&item.explicits)
        .cloned()
        .collect();
```

with:

```rust
    // Only the item's explicit affixes drive value/comparable filters. Runes are
    // buyer-added sockets, implicits are base-inherent, enchants are added — none
    // should constrain the comparable search (they over-collapse it; see the
    // marginal-pricing design).
    let all_stats: Vec<_> = item.explicits.to_vec();
```

and replace the `buckets` array with explicits only:

```rust
    let buckets = [(&item.explicits, StatGroup::Explicit)];
```

- [ ] **Step 4: Add `base_query`**

Add near `build_baseline` in `src/trade/query.rs`:

```rust
/// The same query with all stat filters removed (type + misc + equipment kept).
/// Used by the marginal-contribution sampler to fetch the base population.
pub fn base_query(q: &TradeQuery) -> TradeQuery {
    TradeQuery {
        stats: Vec::new(),
        ..q.clone()
    }
}
```

- [ ] **Step 5: Run to green**

Run: `cargo test query::` then `cargo test`
Expected: new tests pass; existing query tests still pass (their items use explicit mods, unaffected). Zero warnings.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt && cargo clippy
git add src/trade/query.rs
git commit -m "feat(trade): build_baseline filters on affix explicits only; add base_query"
# + trailer
```

---

## Task 3: Pure hedonic model (OLS fit + predict)

**Files:**
- Create: `src/trade/hedonic.rs`
- Modify: `src/trade/mod.rs` (add `pub mod hedonic;`)

**Interfaces:**
- Consumes: `crate::trade::model::Listing` (Task 1).
- Produces:
  - `pub struct Prediction { pub p20: f64, pub p50: f64, pub p80: f64, pub sample: usize, pub kept_features: usize }`
  - `pub fn model_price(listings: &[Listing], our_ids: &[String]) -> Option<Prediction>` — `None` when guards trip (too few comparables or singular system); caller falls back.

- [ ] **Step 1: Declare the module + scaffold with failing tests**

In `src/trade/mod.rs` add alongside the others:

```rust
pub mod hedonic;
```

Create `src/trade/hedonic.rs`:

```rust
//! Pure marginal-contribution (hedonic) price model: fit `ln(price)` on which of
//! our mods each comparable has, then predict the full item. No I/O; the caller
//! (`ablation::marginal_estimate`) does the sampling.

use crate::trade::model::Listing;

/// Minimum pooled comparables required to fit; below this, return `None`.
const MIN_FIT: usize = 20;
/// Fraction trimmed from each end of the price distribution before fitting.
const TRIM_FRAC: f64 = 0.10;

#[derive(Clone, Debug, PartialEq)]
pub struct Prediction {
    pub p20: f64,
    pub p50: f64,
    pub p80: f64,
    pub sample: usize,
    pub kept_features: usize,
}

/// 1.0 if `listing` carries each of `our_ids`, else 0.0 (parallel to `our_ids`).
fn features(listing: &Listing, our_ids: &[String]) -> Vec<f64> {
    our_ids
        .iter()
        .map(|id| {
            if listing.explicit_stat_ids.iter().any(|s| s == id) {
                1.0
            } else {
                0.0
            }
        })
        .collect()
}

fn quantile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Solve `(XᵀX) β = Xᵀy` by Gaussian elimination with partial pivoting.
/// `x` rows already include a leading 1.0 (intercept). `None` if singular.
fn ols(x: &[Vec<f64>], y: &[f64]) -> Option<Vec<f64>> {
    let k = x.first()?.len();
    // Normal equations.
    let mut a = vec![vec![0.0_f64; k + 1]; k]; // augmented [XᵀX | Xᵀy]
    for (row, &yi) in x.iter().zip(y) {
        for i in 0..k {
            for j in 0..k {
                a[i][j] += row[i] * row[j];
            }
            a[i][k] += row[i] * yi;
        }
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..k {
        let mut piv = col;
        for r in (col + 1)..k {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if a[piv][col].abs() < 1e-9 {
            return None; // singular / collinear
        }
        a.swap(col, piv);
        let d = a[col][col];
        for j in col..=k {
            a[col][j] /= d;
        }
        for r in 0..k {
            if r != col {
                let f = a[r][col];
                for j in col..=k {
                    a[r][j] -= f * a[col][j];
                }
            }
        }
    }
    Some((0..k).map(|i| a[i][k]).collect())
}

/// Fit `ln(price) ~ intercept + Σ present_i` over `listings`, then predict the
/// item that has ALL our mods. Drops zero-variance feature columns (folded into
/// the intercept), clamps coefficients ≥ 0, and builds the interval from residual
/// quantiles. `None` when too few comparables or the system is singular.
pub fn model_price(listings: &[Listing], our_ids: &[String]) -> Option<Prediction> {
    // Trim both price tails.
    let mut by_price: Vec<&Listing> = listings.iter().filter(|l| l.price_divine > 0.0).collect();
    by_price.sort_by(|a, b| a.price_divine.partial_cmp(&b.price_divine).unwrap());
    let drop = ((by_price.len() as f64) * TRIM_FRAC).floor() as usize;
    let kept: Vec<&Listing> = if by_price.len() > 2 * drop {
        by_price[drop..by_price.len() - drop].to_vec()
    } else {
        by_price
    };
    if kept.len() < MIN_FIT {
        return None;
    }

    // Keep only feature columns that vary in the sample (others are unidentifiable
    // and would make the system singular; their effect lives in the intercept).
    let raw: Vec<Vec<f64>> = kept.iter().map(|l| features(l, our_ids)).collect();
    let keep_cols: Vec<usize> = (0..our_ids.len())
        .filter(|&c| {
            let first = raw[0][c];
            raw.iter().any(|r| r[c] != first)
        })
        .collect();

    let x: Vec<Vec<f64>> = raw
        .iter()
        .map(|r| {
            let mut row = vec![1.0];
            row.extend(keep_cols.iter().map(|&c| r[c]));
            row
        })
        .collect();
    let y: Vec<f64> = kept.iter().map(|l| l.price_divine.ln()).collect();

    let mut coef = ols(&x, &y)?;
    // Clamp marginal coefficients (not the intercept) to be non-negative.
    for c in coef.iter_mut().skip(1) {
        if *c < 0.0 {
            *c = 0.0;
        }
    }

    // Predict the full item: every kept feature present (= 1).
    let pred: f64 = coef[0] + coef[1..].iter().sum::<f64>();

    // Residual quantiles (log space) → interval, recomputed against clamped coef.
    let mut resid: Vec<f64> = x
        .iter()
        .zip(&y)
        .map(|(row, &yi)| {
            let fit: f64 = row.iter().zip(&coef).map(|(xi, ci)| xi * ci).sum();
            yi - fit
        })
        .collect();
    resid.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let p50 = pred.exp();
    let p20 = (pred + quantile(&resid, 0.20)).exp();
    let p80 = (pred + quantile(&resid, 0.80)).exp();
    // Floor at the trimmed base median (full item is never worth less than base).
    let base_median = {
        let mut p: Vec<f64> = kept.iter().map(|l| l.price_divine).collect();
        p.sort_by(|a, b| a.partial_cmp(b).unwrap());
        quantile(&p, 0.50)
    };
    Some(Prediction {
        p20: p20.max(0.0),
        p50: p50.max(base_median),
        p80: p80.max(p50),
        sample: kept.len(),
        kept_features: keep_cols.len(),
    })
}
```

- [ ] **Step 2: Write the model tests**

Add to `src/trade/hedonic.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::{Currency, Money};

    fn lst(price: f64, ids: &[&str]) -> Listing {
        Listing {
            price: Money { amount: price, currency: Currency::Divine },
            price_divine: price,
            explicit_count: ids.len(),
            id: format!("{price}-{}", ids.join("+")),
            explicit_stat_ids: ids.iter().map(|s| s.to_string()).collect(),
        }
    }

    // base≈2, mod A multiplies ×2, mod B ×3 (log-additive). Build a varied sample
    // including 5-of-6-style partials but NEVER the full A+B together.
    fn sample() -> Vec<Listing> {
        let mut v = Vec::new();
        for i in 0..15 {
            let j = (i % 5) as f64 * 0.1;
            v.push(lst(2.0 + j, &[]));            // base
            v.push(lst(4.0 + j, &["A"]));         // +A  (×2)
            v.push(lst(6.0 + j, &["B"]));         // +B  (×3)
        }
        v
    }

    #[test]
    fn predicts_full_from_partials() {
        let ids = vec!["A".to_string(), "B".to_string()];
        let p = model_price(&sample(), &ids).expect("fits");
        // ln2 + ln2 + ln3 = ln12 → ~12, though no comparable had both A and B.
        assert!(p.p50 > 9.0 && p.p50 < 16.0, "p50={}", p.p50);
        assert!(p.p20 <= p.p50 && p.p50 <= p.p80);
        assert_eq!(p.kept_features, 2);
    }

    #[test]
    fn too_few_comparables_returns_none() {
        let ids = vec!["A".to_string()];
        let few = vec![lst(2.0, &[]), lst(4.0, &["A"])];
        assert!(model_price(&few, &ids).is_none());
    }

    #[test]
    fn zero_variance_feature_dropped_not_singular() {
        // Feature C present on EVERY comparable → unidentifiable; must be dropped,
        // not crash the solve.
        let ids = vec!["A".to_string(), "C".to_string()];
        let mut v = Vec::new();
        for i in 0..12 {
            let j = i as f64 * 0.01;
            v.push(lst(2.0 + j, &["C"]));
            v.push(lst(4.0 + j, &["A", "C"]));
        }
        let p = model_price(&v, &ids).expect("fits with C dropped");
        assert_eq!(p.kept_features, 1); // only A varies
    }
}
```

- [ ] **Step 3: Run to green**

Run: `cargo test trade::hedonic`
Expected: PASS (3 tests). The scaffold + impl were written together; if `predicts_full_from_partials` is off, check the trim isn't eating the sample (MIN_FIT=20, sample has 45 rows).

- [ ] **Step 4: Build + commit**

```bash
cargo build && cargo fmt && cargo clippy
git add src/trade/hedonic.rs src/trade/mod.rs
git commit -m "feat(trade): pure hedonic price model (OLS marginal contributions)"
# + trailer
```

---

## Task 4: `RateLimiter::estimate(ep, n)`

**Files:**
- Modify: `src/trade/limiter.rs` (method + pure helper + tests)

**Interfaces:**
- Produces: `pub fn estimate(&self, ep: Endpoint, n: usize) -> std::time::Duration` — expected wall-clock to send `n` more requests on `ep` given the current window + learned rules.

- [ ] **Step 1: Write the failing test**

In `src/trade/limiter.rs` `tests`:

```rust
    #[test]
    fn estimate_scales_with_n_and_rules() {
        // 5 per 10s: the first 5 are ~free, then ~one per 2s.
        assert_eq!(estimate_secs(&[rule(5, 10)], &[], 5).round() as i64, 0);
        // 10 requests against 5/10s ⇒ ~10s of waiting for the extra 5.
        let t = estimate_secs(&[rule(5, 10)], &[], 10);
        assert!((9.0..=11.0).contains(&t), "got {t}");
        // No rules ⇒ instant.
        assert_eq!(estimate_secs(&[], &[], 50), 0.0);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test estimate_scales_with_n_and_rules`
Expected: FAIL — `estimate_secs` undefined.

- [ ] **Step 3: Implement the pure helper + method**

In `src/trade/limiter.rs`, add a pure free function near `wait_secs`:

```rust
/// Expected seconds to send `n` more requests, simulating the sliding window
/// forward: each request waits `wait_secs` then occupies a slot. `ages` are the
/// current window's send ages (ascending), as in `wait_secs`.
fn estimate_secs(rules: &[RateRule], ages: &[f64], n: usize) -> f64 {
    if rules.is_empty() || n == 0 {
        return 0.0;
    }
    // Work in "time since now"; a send at simulated time t has age (clock - t).
    let mut clock = 0.0_f64;
    let mut sends: Vec<f64> = ages.iter().map(|a| -a).collect(); // send times (≤0)
    let longest = rules.iter().map(|r| r.period).max().unwrap_or(0) as f64;
    for _ in 0..n {
        let cur: Vec<f64> = sends.iter().map(|t| clock - t).collect();
        let mut sorted = cur.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let w = wait_secs(rules, &sorted);
        clock += w;
        sends.push(clock);
        sends.retain(|t| clock - t <= longest);
    }
    clock
}
```

Add the method on `RateLimiter` (uses `try_lock` to avoid blocking; falls back to the rules with an empty window if the bucket is busy):

```rust
    /// Ballpark wall-clock to send `n` more requests on `ep`, from the current
    /// window + learned rules. Best-effort: if the bucket is momentarily locked,
    /// estimates against an empty window.
    pub fn estimate(&self, ep: Endpoint, n: usize) -> Duration {
        let secs = match self.bucket(ep).try_lock() {
            Ok(b) => {
                let now = Instant::now();
                let mut ages: Vec<f64> = b
                    .sends
                    .iter()
                    .map(|t| now.duration_since(*t).as_secs_f64())
                    .collect();
                ages.sort_by(|a, c| a.partial_cmp(c).unwrap_or(std::cmp::Ordering::Equal));
                estimate_secs(&b.rules, &ages, n)
            }
            Err(_) => estimate_secs(&Bucket::with_defaults().rules, &[], n),
        };
        Duration::from_secs_f64(secs)
    }
```

- [ ] **Step 4: Run to green**

Run: `cargo test trade::limiter`
Expected: PASS — the new test plus all existing limiter tests.

- [ ] **Step 5: Build + commit**

```bash
cargo build && cargo fmt && cargo clippy
git add src/trade/limiter.rs
git commit -m "feat(trade): RateLimiter::estimate for value-path latency ETA"
# + trailer
```

---

## Task 5: Orchestration — `PriceProgress`, `marginal_estimate`, routing

**Files:**
- Modify: `src/trade/model.rs` (`EstimateBasis::Marginal`)
- Modify: `src/discord/embeds.rs` (`tier_note` arm for `Marginal` — required: the match is exhaustive, so the variant won't compile without it)
- Modify: `src/trade/ablation.rs` (`PriceProgress` + `NoProgress` + `marginal_estimate` + tests)
- Modify: `src/trade/mod.rs` (`price` routing; pass progress; tests use `NoProgress`)

**Interfaces:**
- Consumes: `hedonic::model_price` (Task 3), `query::base_query` (Task 2), `RateLimiter::estimate` (Task 4), existing `craftability_filter`/`estimate_from`/`modal_currency`/`MIN_COMPARABLES`.
- Produces:
  - `#[async_trait] pub trait PriceProgress: Send + Sync { async fn value_path(&self, sub_queries: usize, eta: std::time::Duration); }`
  - `pub struct NoProgress;` impl that does nothing.
  - `pub async fn marginal_estimate<C: Comparables + ?Sized>(c: &C, query: &TradeQuery, limit: usize, session: &TradeSession, max_explicit: usize, progress: &dyn PriceProgress) -> Result<PriceEstimate>`
  - `TradePricer::price` gains a `progress: &dyn PriceProgress` parameter.

- [ ] **Step 1: Add the `Marginal` basis (+ its embed arm)**

In `src/trade/model.rs` `EstimateBasis`, add a variant:

```rust
    /// Exact comparables too thin → value derived from a marginal-contribution
    /// (hedonic) model over partial-overlap comparables.
    Marginal,
```

This makes `tier_note`'s match in `src/discord/embeds.rs` non-exhaustive — add the
arm in the same step (before `}` closing the `match est.basis`):

```rust
        Marginal => format!(
            "estimated from marginal mod values · modelled from {} partial-match listings",
            est.listing_count
        ),
```

- [ ] **Step 2: Write the orchestration + routing tests**

In `src/trade/ablation.rs` `tests`, add a fake `Comparables` that returns different listings per query so the model has variety, and assert routing + a recording progress:

```rust
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingFake {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl Comparables for CountingFake {
        async fn comparables(
            &self,
            q: &TradeQuery,
            _l: usize,
            _s: &TradeSession,
        ) -> anyhow::Result<Vec<Listing>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // base (no stats) → 30 cheap 1-affix items; base+stat → pricier items
            // carrying that stat id, so the model can fit.
            let mut v = Vec::new();
            if q.stats.is_empty() {
                for i in 0..30 {
                    v.push(listing_full(2.0 + i as f64 * 0.01, 2, "i", &[]));
                }
            } else {
                let id = q.stats[0].id.clone();
                for i in 0..30 {
                    v.push(listing_full(
                        6.0 + i as f64 * 0.01,
                        2,
                        &format!("{id}-{i}"),
                        &[id.clone()],
                    ));
                }
            }
            Ok(v)
        }
    }

    struct RecProgress {
        hits: AtomicUsize,
        last: std::sync::Mutex<usize>,
    }
    #[async_trait]
    impl PriceProgress for RecProgress {
        async fn value_path(&self, sub_queries: usize, _eta: std::time::Duration) {
            self.hits.fetch_add(1, Ordering::SeqCst);
            *self.last.lock().unwrap() = sub_queries;
        }
    }

    #[tokio::test]
    async fn marginal_estimate_fits_and_reports_progress() {
        let fake = CountingFake { calls: AtomicUsize::new(0) };
        let prog = RecProgress { hits: AtomicUsize::new(0), last: std::sync::Mutex::new(0) };
        let q = two_stat_query(); // 2 stat filters (existing helper)
        let est = marginal_estimate(&fake, &q, 100, &TradeSession::for_test(), 6, &prog)
            .await
            .unwrap();
        assert_eq!(est.basis, EstimateBasis::Marginal);
        assert!(est.typical > 0.0);
        // base + each of 2 stats = 3 sub-queries.
        assert_eq!(*prog.last.lock().unwrap(), 3);
        assert_eq!(prog.hits.load(Ordering::SeqCst), 1);
    }
```

Add a shared test helper in the `ablation.rs` `tests` module (used above):

```rust
    pub(super) fn listing_full(divine: f64, ec: usize, id: &str, ids: &[String]) -> Listing {
        Listing {
            price: Money { amount: divine, currency: Currency::Divine },
            price_divine: divine,
            explicit_count: ec,
            id: id.to_string(),
            explicit_stat_ids: ids.to_vec(),
        }
    }
```

(If `two_stat_query`, `Money`, `Currency` aren't already imported in the test module, add them.)

- [ ] **Step 3: Run to verify failure**

Run: `cargo test marginal_estimate_fits_and_reports_progress`
Expected: FAIL — `PriceProgress`, `NoProgress`, `marginal_estimate` undefined.

- [ ] **Step 4: Implement `PriceProgress` + `marginal_estimate`**

In `src/trade/ablation.rs`, add the imports and code (near the top / after `Comparables`):

```rust
use std::time::Duration;

use crate::trade::limiter::Endpoint;
use crate::trade::query::base_query;

/// Reports the value path's latency estimate to the caller before the wait.
#[async_trait]
pub trait PriceProgress: Send + Sync {
    async fn value_path(&self, sub_queries: usize, eta: Duration);
}

/// No-op progress for tests and non-interactive callers.
pub struct NoProgress;
#[async_trait]
impl PriceProgress for NoProgress {
    async fn value_path(&self, _sub_queries: usize, _eta: Duration) {}
}

/// Derive an estimate when the exact query is too thin: sample base + each mod,
/// fit a marginal-contribution model, predict the full item. Falls back to a
/// broad base-tier percentile when the model can't fit. Never errors on thin data.
pub async fn marginal_estimate<C: Comparables + ?Sized>(
    c: &C,
    query: &TradeQuery,
    limit: usize,
    session: &TradeSession,
    max_explicit: usize,
    progress: &dyn PriceProgress,
) -> Result<PriceEstimate> {
    let n = query.stats.len();
    let eta = session.limiter.estimate(Endpoint::Search, n + 1)
        + session.limiter.estimate(Endpoint::Fetch, n + 1);
    progress.value_path(n + 1, eta).await;

    let base = base_query(query);
    let mut pool = c.comparables(&base, limit, session).await?;
    for s in &query.stats {
        let mut q = base.clone();
        q.stats = vec![s.clone()];
        pool.extend(c.comparables(&q, limit, session).await?);
    }
    // Dedup by listing id (a rare returned by several sub-queries counts once).
    pool.sort_by(|a, b| a.id.cmp(&b.id));
    pool.dedup_by(|a, b| a.id == b.id && !a.id.is_empty());

    let pool = craftability_filter(&pool, max_explicit);
    let our_ids: Vec<String> = query.stats.iter().map(|s| s.id.clone()).collect();

    match crate::trade::hedonic::model_price(&pool, &our_ids) {
        Some(p) => {
            let confidence = if p.sample >= 30 && p.kept_features * 2 >= n.max(1) {
                Confidence::Medium
            } else {
                Confidence::Low
            };
            Ok(PriceEstimate {
                low: p.p20,
                typical: p.p50,
                high: p.p80,
                listing_count: pool.len(),
                confidence,
                modal_currency: modal_currency(&pool),
                basis: EstimateBasis::Marginal,
            })
        }
        // Too thin/collinear to model → broad base-tier percentile, Low confidence.
        None => Ok(estimate_from(&pool, EstimateBasis::BroadMarket)),
    }
}
```

`TradeSession` already exposes `limiter`. Ensure `EstimateBasis`, `Confidence`, `Duration` are in scope (extend the existing `use crate::trade::model::{…}`; add `Confidence`).

- [ ] **Step 5: Route in `TradePricer::price`**

In `src/trade/mod.rs`, change `price`'s signature and body:

```rust
    pub async fn price(
        &self,
        item: &ParsedItem,
        league: &str,
        session: &TradeSession,
        progress: &dyn crate::trade::ablation::PriceProgress,
    ) -> Result<PriceEstimate> {
        let query = build_baseline(item, &self.pseudo, &self.catalog, league);
        let max_explicit = item.craftability().map(|c| c.explicit_count as usize);
        let exact = estimate(
            &self.comparables,
            &query,
            COMPARABLE_SAMPLE,
            session,
            max_explicit,
        )
        .await?;
        // Fast path: enough exact comparables, or craftability unknown (can't model).
        let est = match max_explicit {
            Some(max) if exact.listing_count < MIN_COMPARABLES => {
                crate::trade::ablation::marginal_estimate(
                    &self.comparables,
                    &query,
                    COMPARABLE_SAMPLE,
                    session,
                    max,
                    progress,
                )
                .await?
            }
            _ => exact,
        };
        self.record(&query, &est);
        Ok(est)
    }
```

Add `use crate::trade::ablation::{estimate, Comparables, MIN_COMPARABLES};` — i.e. import `MIN_COMPARABLES` (make it `pub(crate)` in `ablation.rs` if not already). Update the `mod.rs` test `price_logs_a_probe_and_returns_estimate` to pass `&crate::trade::ablation::NoProgress` as the new arg. (The `Flat` fake returns 8 listings with `explicit_count: 0`; `craftability_filter` drops them so `exact.listing_count` is 0, but `max_explicit` is `None` for the `ring()` item — it has an untagged explicit — so it stays on the fast path and the test is unchanged in behaviour. Confirm `ring()`'s `craftability()` is `None`; it is, since its single explicit has `affix: None`.)

- [ ] **Step 6: Run to green**

Run: `cargo test trade::` then `cargo test`
Expected: PASS — orchestration test, routing test, existing suite. Zero warnings.

- [ ] **Step 7: Format, lint, commit**

```bash
cargo fmt && cargo clippy
git add src/trade/model.rs src/discord/embeds.rs src/trade/ablation.rs src/trade/mod.rs
git commit -m "feat(trade): marginal-contribution value path + PriceProgress + routing"
# + trailer
```

---

## Task 6: Discord wiring — latency notice during the value path

**Files:**
- Modify: `src/discord/paste.rs` (`run_pricing`: placeholder reply + `PriceProgress` impl + price-with-progress)

(The `Marginal` embed label was added in Task 5, since the variant must compile.)

**Interfaces:**
- Consumes: `TradePricer::price(.., progress)` and `crate::trade::ablation::PriceProgress` (Task 5).

- [ ] **Step 1: Implement `PriceProgress` for the discord reply**

In `src/discord/paste.rs`, add (near `run_pricing`):

```rust
// `Context` is `Copy` (the existing code already uses `*ctx`), so hold it by value
// to avoid a nested `&'a Context<'a>` lifetime.
struct ReplyProgress<'a> {
    ctx: Context<'a>,
    reply: &'a poise::ReplyHandle<'a>,
}
#[async_trait::async_trait]
impl crate::trade::ablation::PriceProgress for ReplyProgress<'_> {
    async fn value_path(&self, sub_queries: usize, eta: std::time::Duration) {
        let secs = eta.as_secs().max(1);
        let _ = self
            .reply
            .edit(
                self.ctx,
                poise::CreateReply::default().content(format!(
                    "⏳ Heavily-modded item — modelling its value from {sub_queries} market samples (~{secs}s)…"
                )),
            )
            .await;
    }
}
```

- [ ] **Step 2: Restructure `run_pricing` to send a placeholder first, then price with progress**

In `src/discord/paste.rs` `run_pricing`, replace the opening `let est = match pricer.price(...)` block with: send a "Pricing…" reply first, capture the handle, price with a `ReplyProgress`, then continue building the embed by editing that handle. Concretely, change the start of `run_pricing`:

```rust
    let pricer = ctx.data().pricer.clone();
    let reply = ctx.send(poise::CreateReply::default().content("⏳ Pricing…")).await?;
    let progress = ReplyProgress { ctx: *ctx, reply: &reply };
    let est = match pricer.price(parsed, &league.name, session, &progress).await {
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

Then, where the code currently does `let reply = ctx.send( … embed … components … )`, **reuse the existing `reply`** instead — replace that `ctx.send(...)` with `reply.edit(*ctx, poise::CreateReply::default().embed(...).components(vec![row]))` and keep `let msg = reply.message().await?;` afterward. The two later `reply.edit(...)` calls (button-clicked / timeout branches) stay as-is.

(The collector and breakdown branches are unchanged. `ReplyProgress` borrows `ctx` and `reply`; both outlive the `price` call.)

- [ ] **Step 3: Build + verify the discord crate compiles**

Run: `cargo build` then `cargo test`
Expected: zero warnings; suite green (the discord layer has no unit tests for this flow — covered by manual acceptance).

- [ ] **Step 4: Format, lint, commit**

```bash
cargo fmt && cargo clippy
git add src/discord/paste.rs
git commit -m "feat(discord): show value-path latency notice during modelled pricing"
# + trailer
```

---

## Final verification (after all tasks)

- [ ] `cargo fmt --check` clean; `cargo clippy` clean; `cargo test` green; `cargo build` zero warnings.
- [ ] **Manual live acceptance** (after deploy): `/paste` the Chiming Staff → shows "⏳ modelling … (~Ts)" then a **non-zero** estimate labelled "marginal mod values" with a sensible p20/p50/p80 and Low/Medium confidence; no "No comparable listings found". A common rare (boot) prices via the fast path with **no** notice and unchanged output. Confirm no `trade2 rate-limited` / 429s in `docker logs` during the modelled pricing.
- [ ] Confirm the value path issued ~N+1 searches (paced) and completed within roughly the shown ETA.
