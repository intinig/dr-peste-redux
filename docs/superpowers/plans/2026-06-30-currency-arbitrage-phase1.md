# Currency Arbitrage Detector — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship an on-demand `/arb` Discord command that finds and ranks profitable PoE2 Currency Exchange flips and triangulation cycles over a configured currency watchlist, using live trade2 order-book data.

**Architecture:** A new isolated `src/arb/` module. A `CandidateSource` emits directed rate `Edge`s; a pure graph engine (`graph.rs`) enumerates profitable cycles and a pure spread scanner (`spread.rs`) finds flips; `ArbEngine` (`mod.rs`) orchestrates source → detectors → rank. Phase 1 backs the source with `WatchlistSource`, which live-queries the trade2 exchange endpoint, so every quote is already `Live` and the confirm stage is a no-op. Data flows source → engine → discord, never sideways.

**Tech Stack:** Rust, tokio, `async-trait`, `reqwest`, `serde_json`, `poise`/`serenity`, `anyhow`, `tracing`. Offline tests against committed JSON fixtures.

## Global Constraints

- **Read-only, ToS-safe:** the feature only *finds and ranks* opportunities. No code path places Currency Exchange orders. Never add one.
- **Never panic in background/refresher or command paths:** `anyhow::Result` + `tracing`; degrade gracefully.
- **Tests offline by default:** anything hitting the network is `#[ignore]`d. Unit tests use committed fixtures / in-memory fakes.
- **Politeness to GGG:** all live trade2 traffic goes through `RateLimiter`; reuse the existing 60s response cache for the exchange call.
- **Secrets:** no new secrets in Phase 1. Never commit `.env`; only update `.env.example`.
- **CI is strict:** `cargo clippy --all-targets -- -D warnings` must pass (CI uses a newer toolchain than local — fix all warnings).
- **Stage files by name** (never `git add -A`). Conventional commits, scope `arb`.
- **Editing Rust here:** prefer the native Edit tool for `.rs` changes (serena `replace_symbol_body` has corrupted Rust files in this repo before). Trust `cargo build`/`cargo test` over rust-analyzer diagnostics.

---

## File Structure

```
src/arb/
  mod.rs       ArbEngine + ArbConfig; the only surface the command uses
  model.rs     Currency, Freshness, RatioQuote, Edge, Leg, Opportunity, score()
  graph.rs     RateGraph + profitable_cycles() (triangulation, length >= 3)
  spread.rs    scan() (flips: 2-cycle maker spread)
  source.rs    CandidateSource trait + WatchlistSource
src/discord/arb.rs            /arb command + autocomplete-free
src/trade/client.rs           + TradeClient::exchange(), ExchangeOffer, parse_exchange()
src/trade/limiter.rs          + Endpoint::Exchange + bucket
src/trade/fixtures/exchange_pair.json   captured live fixture (committed)
src/config.rs                 + ARB_* fields
src/discord/embeds.rs         + arb_embed()
src/discord/mod.rs            register arb module
src/main.rs                   register arb::arb() command + build ArbEngine into Data
.env.example, CLAUDE.md       docs
```

The pure logic (`model`, `graph`, `spread`) has no I/O and is fully unit-tested. The trade2 exchange shape is verified against a captured fixture in Task 6 before `WatchlistSource` depends on it.

---

### Task 1: `arb` module scaffold + core types

**Files:**
- Create: `src/arb/mod.rs`, `src/arb/model.rs`
- Modify: `src/main.rs` (add `mod arb;`)

**Interfaces:**
- Produces:
  - `pub type Currency = String` (a trade2 exchange currency id, e.g. `"divine"`)
  - `pub enum Freshness { Live, Aggregated }` (Clone, Copy, PartialEq, Eq, Debug)
  - `pub struct RatioQuote { pub pay: u32, pub get: u32, pub stock: u64, pub freshness: Freshness }` with `pub fn ratio(&self) -> f64` returning `get/pay` (units of `to` per 1 `from`)
  - `pub struct Edge { pub from: Currency, pub to: Currency, pub quote: RatioQuote }`
  - `pub struct Leg { pub from: Currency, pub to: Currency, pub quote: RatioQuote }`

- [ ] **Step 1: Write the failing test**

Create `src/arb/model.rs`:

```rust
//! Core value types for the currency-arbitrage engine. Pure, no I/O.

/// A trade2 currency-exchange id (e.g. "divine", "exalted").
pub type Currency = String;

/// Where a quote came from: a live trade2 order book, or a cxapi hourly digest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Freshness {
    Live,
    Aggregated,
}

/// One executable conversion: give `pay` units of `from` to receive `get`
/// units of `to`. Kept as integers to match the exchange's discrete ratios.
#[derive(Clone, Debug)]
pub struct RatioQuote {
    pub pay: u32,
    pub get: u32,
    /// Units of `to` available at this quote (taker depth).
    pub stock: u64,
    pub freshness: Freshness,
}

impl RatioQuote {
    /// Units of `to` received per 1 unit of `from`.
    pub fn ratio(&self) -> f64 {
        self.get as f64 / self.pay as f64
    }
}

/// A directed conversion edge `from -> to` carrying its best quote.
#[derive(Clone, Debug)]
pub struct Edge {
    pub from: Currency,
    pub to: Currency,
    pub quote: RatioQuote,
}

/// One hop of a realised cycle.
#[derive(Clone, Debug)]
pub struct Leg {
    pub from: Currency,
    pub to: Currency,
    pub quote: RatioQuote,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_is_get_over_pay() {
        let q = RatioQuote { pay: 2, get: 5, stock: 100, freshness: Freshness::Live };
        assert!((q.ratio() - 2.5).abs() < 1e-9);
    }
}
```

Create `src/arb/mod.rs`:

```rust
pub mod model;
```

Add `mod arb;` to `src/main.rs` alongside the other top-level `mod` declarations.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test arb::model 2>&1 | tail -20`
Expected: compile error or FAIL until the file is in place; once it compiles, PASS. (If it already passes, that's fine — the type is trivial.)

- [ ] **Step 3: (covered by Step 1)** No additional implementation; the type and test are written together because the behavior is a one-line accessor.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test arb::model 2>&1 | tail -20`
Expected: `test arb::model::tests::ratio_is_get_over_pay ... ok`

- [ ] **Step 5: Commit**

```bash
git add src/arb/mod.rs src/arb/model.rs src/main.rs
git commit -m "feat(arb): scaffold arb module and core rate types

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Triangulation engine (`graph.rs`)

Enumerate profitable simple cycles (length ≥ 3) with stock-aware feasible volume.

**Files:**
- Create: `src/arb/graph.rs`
- Modify: `src/arb/mod.rs` (add `pub mod graph;`)

**Interfaces:**
- Consumes: `Edge`, `Leg`, `RatioQuote` from `model`.
- Produces:
  - `pub struct CycleResult { pub legs: Vec<Leg>, pub multiplier: f64, pub feasible_volume: f64 }`
  - `pub struct RateGraph { /* private */ }`
  - `RateGraph::from_edges(edges: &[Edge]) -> RateGraph`
  - `RateGraph::profitable_cycles(&self, max_len: usize, min_profit: f64) -> Vec<CycleResult>` — returns cycles of 3..=max_len currencies whose `multiplier > 1.0 + min_profit`, deduped by canonical rotation, sorted by `multiplier` descending.

**Math (document in code):** For a cycle `c0 -> c1 -> ... -> c0` with leg ratios `r_i = get_i/pay_i`, the gross multiplier is `M = Π r_i`. Per unit of `c0` input, the amount entering leg `i` is `P_i = r_0·…·r_{i-1}` (so `P_0 = 1`). Leg `i` produces `P_i · X · r_i` units of `c_{i+1}`, capped by `stock_i` → `X ≤ stock_i / (P_i · r_i) = stock_i / P_{i+1}`. Therefore `feasible_volume = min_i ( stock_i / P_{i+1} )`, in units of `c0`.

- [ ] **Step 1: Write the failing test**

Create `src/arb/graph.rs`:

```rust
//! Directed rate graph + bounded profitable-cycle enumeration (triangulation).
//! Pure, no I/O. A profitable cycle's leg ratios compound to > 1.

use crate::arb::model::{Currency, Edge, Leg, RatioQuote};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct CycleResult {
    pub legs: Vec<Leg>,
    pub multiplier: f64,
    pub feasible_volume: f64,
}

pub struct RateGraph {
    /// from -> [(to, quote)]
    adj: HashMap<Currency, Vec<(Currency, RatioQuote)>>,
    nodes: Vec<Currency>,
}

impl RateGraph {
    pub fn from_edges(edges: &[Edge]) -> RateGraph {
        let mut adj: HashMap<Currency, Vec<(Currency, RatioQuote)>> = HashMap::new();
        let mut nodes: Vec<Currency> = Vec::new();
        for e in edges {
            for c in [&e.from, &e.to] {
                if !nodes.iter().any(|n| n == c) {
                    nodes.push(c.clone());
                }
            }
            adj.entry(e.from.clone())
                .or_default()
                .push((e.to.clone(), e.quote.clone()));
        }
        RateGraph { adj, nodes }
    }

    pub fn profitable_cycles(&self, max_len: usize, min_profit: f64) -> Vec<CycleResult> {
        let mut found: Vec<CycleResult> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Start a DFS from each node; only keep cycles that return to the start.
        for start in &self.nodes {
            let mut path: Vec<Leg> = Vec::new();
            self.dfs(start, start, max_len, min_profit, &mut path, &mut found, &mut seen);
        }
        found.sort_by(|a, b| b.multiplier.partial_cmp(&a.multiplier).unwrap_or(std::cmp::Ordering::Equal));
        found
    }

    #[allow(clippy::too_many_arguments)]
    fn dfs(
        &self,
        start: &str,
        current: &str,
        max_len: usize,
        min_profit: f64,
        path: &mut Vec<Leg>,
        out: &mut Vec<CycleResult>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        if path.len() >= max_len {
            return;
        }
        let Some(neighbors) = self.adj.get(current) else { return };
        for (to, quote) in neighbors {
            // Closing the cycle back to start.
            if to == start {
                if path.len() + 1 < 3 {
                    continue; // triangulation is length >= 3
                }
                let mut legs = path.clone();
                legs.push(Leg { from: current.to_string(), to: to.clone(), quote: quote.clone() });
                if let Some(res) = evaluate_cycle(&legs) {
                    if res.multiplier > 1.0 + min_profit {
                        let key = canonical_key(&legs);
                        if seen.insert(key) {
                            out.push(res);
                        }
                    }
                }
                continue;
            }
            // Avoid revisiting a node already on the path (simple cycles only).
            if to == start || path.iter().any(|l| &l.from == to) {
                continue;
            }
            path.push(Leg { from: current.to_string(), to: to.clone(), quote: quote.clone() });
            self.dfs(start, to, max_len, min_profit, path, out, seen);
            path.pop();
        }
    }
}

/// Multiplier and stock-bottleneck feasible volume for a closed cycle.
fn evaluate_cycle(legs: &[Leg]) -> Option<CycleResult> {
    if legs.len() < 3 {
        return None;
    }
    let mut multiplier = 1.0f64;
    let mut feasible = f64::INFINITY;
    // P_{i+1} = product of ratios up to and including leg i.
    for leg in legs {
        let r = leg.quote.ratio();
        multiplier *= r;
        let p_next = multiplier; // P_{i+1}
        let cap = leg.quote.stock as f64 / p_next;
        if cap < feasible {
            feasible = cap;
        }
    }
    Some(CycleResult { legs: legs.to_vec(), multiplier, feasible_volume: feasible })
}

/// Rotation-invariant key so the same cycle discovered from different start
/// nodes is deduped. Uses the currency sequence rotated to its lexicographic min.
fn canonical_key(legs: &[Leg]) -> String {
    let seq: Vec<&str> = legs.iter().map(|l| l.from.as_str()).collect();
    let n = seq.len();
    let min_idx = (0..n).min_by_key(|&i| seq[i]).unwrap_or(0);
    let rotated: Vec<&str> = (0..n).map(|k| seq[(min_idx + k) % n]).collect();
    rotated.join(">")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arb::model::Freshness;

    fn edge(from: &str, to: &str, pay: u32, get: u32, stock: u64) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            quote: RatioQuote { pay, get, stock, freshness: Freshness::Live },
        }
    }

    #[test]
    fn finds_profitable_triangle() {
        // A->B 1:2, B->C 1:2, C->A 1:0.3 => M = 2*2*0.3 = 1.2 (+20%)
        let edges = vec![
            edge("A", "B", 1, 2, 1000),
            edge("B", "C", 1, 2, 1000),
            edge("C", "A", 10, 3, 1000),
        ];
        let g = RateGraph::from_edges(&edges);
        let cycles = g.profitable_cycles(4, 0.0);
        assert_eq!(cycles.len(), 1);
        assert!((cycles[0].multiplier - 1.2).abs() < 1e-9);
    }

    #[test]
    fn ignores_unprofitable_and_two_cycles() {
        // A<->B round trip loses (no triangle); must yield nothing.
        let edges = vec![edge("A", "B", 1, 2, 100), edge("B", "A", 2, 1, 100)];
        let g = RateGraph::from_edges(&edges);
        assert!(g.profitable_cycles(4, 0.0).is_empty());
    }

    #[test]
    fn feasible_volume_is_bottleneck() {
        // Same +20% triangle but C->A stock limits throughput.
        // P after legs: P1=2, P2=4, P3=1.2. caps: 1000/2=500, 1000/4=250, stock/1.2.
        // Set C->A stock=120 => cap=100 => bottleneck 100.
        let edges = vec![
            edge("A", "B", 1, 2, 1000),
            edge("B", "C", 1, 2, 1000),
            edge("C", "A", 10, 3, 120),
        ];
        let g = RateGraph::from_edges(&edges);
        let c = &g.profitable_cycles(4, 0.0)[0];
        assert!((c.feasible_volume - 100.0).abs() < 1e-6, "got {}", c.feasible_volume);
    }

    #[test]
    fn dedups_rotations() {
        let edges = vec![
            edge("A", "B", 1, 2, 1000),
            edge("B", "C", 1, 2, 1000),
            edge("C", "A", 10, 3, 1000),
        ];
        let g = RateGraph::from_edges(&edges);
        // Even though DFS starts from A, B, and C, the cycle appears once.
        assert_eq!(g.profitable_cycles(4, 0.0).len(), 1);
    }
}
```

Add `pub mod graph;` to `src/arb/mod.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test arb::graph 2>&1 | tail -30`
Expected: tests compile and run; if any logic is off they FAIL here. (Write-then-run; the implementation is included so this may pass directly — confirm all four tests pass.)

- [ ] **Step 3: Fix any failing assertion** by correcting `evaluate_cycle` / `dfs` until the math matches the hand-computed numbers in the tests. Do not change the tests' expected values (they are hand-derived).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test arb::graph 2>&1 | tail -30`
Expected: all four tests `... ok`.

- [ ] **Step 5: Commit**

```bash
git add src/arb/graph.rs src/arb/mod.rs
git commit -m "feat(arb): triangulation cycle search with stock-bottleneck volume

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Flip scanner (`spread.rs`)

**Files:**
- Create: `src/arb/spread.rs`
- Modify: `src/arb/mod.rs` (add `pub mod spread;`)

**Interfaces:**
- Consumes: `Edge`, `RatioQuote` from `model`.
- Produces:
  - `pub struct FlipResult { pub market: (Currency, Currency), pub spread_pct: f64, pub volume: f64 }`
  - `pub fn scan(edges: &[Edge], min_spread: f64, min_volume: f64) -> Vec<FlipResult>` — for each unordered pair with both directions present, `spread_pct = 1 - ratio(A->B)*ratio(B->A)` (clamped ≥ 0), `volume = min(stock(A->B), stock(B->A)) as f64`; keep where `spread_pct >= min_spread && volume >= min_volume`; sort by `spread_pct * volume` descending; market tuple ordered lexicographically for stable identity.

- [ ] **Step 1: Write the failing test**

Create `src/arb/spread.rs`:

```rust
//! Flip detection: a maker captures the round-trip deficit on a single market.
//! For market {A,B}, taking both directions returns ratio(A->B)*ratio(B->A) < 1;
//! the deficit (1 - product) is the spread a maker can earn. Pure, no I/O.

use crate::arb::model::{Currency, Edge};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct FlipResult {
    pub market: (Currency, Currency),
    pub spread_pct: f64,
    pub volume: f64,
}

pub fn scan(edges: &[Edge], min_spread: f64, min_volume: f64) -> Vec<FlipResult> {
    // Index directed edges by (from,to).
    let mut by_pair: HashMap<(Currency, Currency), &Edge> = HashMap::new();
    for e in edges {
        by_pair.insert((e.from.clone(), e.to.clone()), e);
    }
    let mut out: Vec<FlipResult> = Vec::new();
    let mut done: std::collections::HashSet<(Currency, Currency)> = std::collections::HashSet::new();
    for e in edges {
        let (a, b) = if e.from <= e.to {
            (e.from.clone(), e.to.clone())
        } else {
            (e.to.clone(), e.from.clone())
        };
        if !done.insert((a.clone(), b.clone())) {
            continue;
        }
        let (Some(ab), Some(ba)) = (by_pair.get(&(a.clone(), b.clone())), by_pair.get(&(b.clone(), a.clone()))) else {
            continue;
        };
        let product = ab.quote.ratio() * ba.quote.ratio();
        let spread_pct = (1.0 - product).max(0.0);
        let volume = ab.quote.stock.min(ba.quote.stock) as f64;
        if spread_pct >= min_spread && volume >= min_volume {
            out.push(FlipResult { market: (a, b), spread_pct, volume });
        }
    }
    out.sort_by(|x, y| {
        (y.spread_pct * y.volume)
            .partial_cmp(&(x.spread_pct * x.volume))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arb::model::{Freshness, RatioQuote};

    fn edge(from: &str, to: &str, pay: u32, get: u32, stock: u64) -> Edge {
        Edge { from: from.into(), to: to.into(), quote: RatioQuote { pay, get, stock, freshness: Freshness::Live } }
    }

    #[test]
    fn detects_spread() {
        // A->B ratio 0.9, B->A ratio 1.0 => product 0.9 => 10% spread.
        let edges = vec![edge("A", "B", 10, 9, 500), edge("B", "A", 1, 1, 300)];
        let flips = scan(&edges, 0.01, 0.0);
        assert_eq!(flips.len(), 1);
        assert!((flips[0].spread_pct - 0.1).abs() < 1e-9);
        assert!((flips[0].volume - 300.0).abs() < 1e-9);
    }

    #[test]
    fn filters_below_thresholds() {
        let edges = vec![edge("A", "B", 10, 9, 5), edge("B", "A", 1, 1, 5)];
        // volume 5 below min_volume 100 => filtered.
        assert!(scan(&edges, 0.01, 100.0).is_empty());
    }

    #[test]
    fn needs_both_directions() {
        let edges = vec![edge("A", "B", 10, 9, 500)];
        assert!(scan(&edges, 0.0, 0.0).is_empty());
    }
}
```

Add `pub mod spread;` to `src/arb/mod.rs`.

- [ ] **Step 2: Run to verify**

Run: `cargo test arb::spread 2>&1 | tail -20`
Expected: three tests `... ok` (fix `scan` if any fail).

- [ ] **Step 3–4: Adjust until green, re-run.**

- [ ] **Step 5: Commit**

```bash
git add src/arb/spread.rs src/arb/mod.rs
git commit -m "feat(arb): flip (maker-spread) scanner

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: `Opportunity` type + ranking

**Files:**
- Modify: `src/arb/model.rs` (add `Opportunity` + `score()`)

**Interfaces:**
- Consumes: `CycleResult` (graph), `FlipResult` (spread), `Leg`, `Currency`, `Freshness`.
- Produces:
  - `pub enum Opportunity { Triangulation { legs: Vec<Leg>, multiplier: f64, feasible_volume: f64, confidence: Freshness }, Flip { market: (Currency, Currency), spread_pct: f64, volume: f64, confidence: Freshness } }`
  - `Opportunity::score(&self) -> f64` — ranking key: triangulation `(multiplier - 1.0) * feasible_volume`; flip `spread_pct * volume`.
  - `Opportunity::confidence(&self) -> Freshness`

- [ ] **Step 1: Write the failing test** (append to `src/arb/model.rs`, above the existing `tests` module's closing brace or as new items + a new test)

```rust
use crate::arb::graph::CycleResult;
use crate::arb::spread::FlipResult;

#[derive(Clone, Debug)]
pub enum Opportunity {
    Triangulation { legs: Vec<Leg>, multiplier: f64, feasible_volume: f64, confidence: Freshness },
    Flip { market: (Currency, Currency), spread_pct: f64, volume: f64, confidence: Freshness },
}

impl Opportunity {
    pub fn from_cycle(c: CycleResult, confidence: Freshness) -> Opportunity {
        Opportunity::Triangulation {
            legs: c.legs,
            multiplier: c.multiplier,
            feasible_volume: c.feasible_volume,
            confidence,
        }
    }
    pub fn from_flip(f: FlipResult, confidence: Freshness) -> Opportunity {
        Opportunity::Flip {
            market: f.market,
            spread_pct: f.spread_pct,
            volume: f.volume,
            confidence,
        }
    }
    pub fn score(&self) -> f64 {
        match self {
            Opportunity::Triangulation { multiplier, feasible_volume, .. } => {
                (multiplier - 1.0) * feasible_volume
            }
            Opportunity::Flip { spread_pct, volume, .. } => spread_pct * volume,
        }
    }
    pub fn confidence(&self) -> Freshness {
        match self {
            Opportunity::Triangulation { confidence, .. } => *confidence,
            Opportunity::Flip { confidence, .. } => *confidence,
        }
    }
}
```

Add this test to the `tests` module in `model.rs`:

```rust
#[test]
fn triangulation_score_is_profit_times_volume() {
    let c = crate::arb::graph::CycleResult { legs: vec![], multiplier: 1.2, feasible_volume: 50.0 };
    let opp = Opportunity::from_cycle(c, Freshness::Live);
    assert!((opp.score() - 10.0).abs() < 1e-9); // 0.2 * 50
}
```

- [ ] **Step 2: Run to verify**

Run: `cargo test arb::model 2>&1 | tail -20`
Expected: existing + new test pass.

- [ ] **Step 3–4: Fix and re-run as needed.**

- [ ] **Step 5: Commit**

```bash
git add src/arb/model.rs
git commit -m "feat(arb): Opportunity enum with ranking score

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Add `Endpoint::Exchange` to the rate limiter

**Files:**
- Modify: `src/trade/limiter.rs` (enum `Endpoint`, struct `RateLimiter`, `new`, `permissive`, `bucket`)

**Interfaces:**
- Produces: `Endpoint::Exchange` variant + its own bucket, paced and calibrated exactly like `Search`/`Fetch`.

- [ ] **Step 1: Write the failing test** — add to the `tests` module in `limiter.rs`:

```rust
#[tokio::test]
async fn exchange_bucket_is_independent_and_permissive_never_waits() {
    let rl = RateLimiter::permissive();
    // Should return immediately (no rules) for the new endpoint.
    rl.acquire(Endpoint::Exchange).await;
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p '*' limiter 2>&1 | tail -20` (or `cargo test trade::limiter`)
Expected: compile error — `no variant named Exchange`.

- [ ] **Step 3: Implement** with native Edit:

In the `Endpoint` enum, add `Exchange`:

```rust
pub enum Endpoint {
    Search,
    Fetch,
    Exchange,
}
```

In `struct RateLimiter`, add a field `exchange: Mutex<Bucket>,` next to `search`/`fetch`.

In `RateLimiter::new()`, add `exchange: Mutex::new(Bucket::with_defaults()),`.

In `RateLimiter::permissive()`, add `exchange: Mutex::new(Bucket::empty()),`.

In `fn bucket(&self, ep: Endpoint)`, add the arm:

```rust
Endpoint::Exchange => &self.exchange,
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test trade::limiter 2>&1 | tail -20`
Expected: new test + existing limiter tests `... ok`.

- [ ] **Step 5: Commit**

```bash
git add src/trade/limiter.rs
git commit -m "feat(arb): add Exchange endpoint bucket to rate limiter

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: trade2 exchange call — capture fixture, parse, `TradeClient::exchange()`

This task verifies the live JSON shape against a captured fixture, then parses it offline. The parsing field paths below follow the documented PoE trade exchange contract; **the capture step is the source of truth — if the live JSON nests fields differently, adjust the paths in `parse_exchange` until the offline test passes against the real fixture.**

**Files:**
- Create: `src/trade/fixtures/exchange_pair.json` (captured, committed)
- Modify: `src/trade/client.rs` (add `ExchangeOffer`, `parse_exchange`, `TradeClient::exchange`, and a method on the `TradeApi` trait is NOT needed — `exchange` is an inherent method using `self.http` + `self.default_limiter`)

**Interfaces:**
- Consumes: `Endpoint::Exchange`, `send_with_retry`, `self.http`, `self.default_limiter`, `TRADE_BASE`.
- Produces:
  - `pub struct ExchangeOffer { pub pay_currency: String, pub pay_amount: u32, pub get_currency: String, pub get_amount: u32, pub stock: u64 }`
  - `pub async fn exchange(&self, have: &str, want: &str, league: &str) -> Result<Vec<ExchangeOffer>>` on `impl TradeClient` — returns offers sorted best-ratio-first (most `want` per `have`).

- [ ] **Step 1: Write the capture helper (ignored, network)** — add to the `tests` module in `client.rs`:

```rust
#[tokio::test]
#[ignore = "network: captures a live trade2 exchange fixture"]
async fn capture_exchange_fixture() {
    // Operator runs this once: `cargo test capture_exchange_fixture -- --ignored --nocapture`
    // It prints the raw fetch JSON for divine<-exalted so we can save a fixture.
    let rates = std::sync::Arc::new(std::sync::RwLock::new(RateTable::default()));
    let client = TradeClient::new(std::env::var("POESESSID").ok(), rates).unwrap();
    let league = std::env::var("ARB_TEST_LEAGUE").unwrap_or_else(|_| "Standard".into());
    let offers = client.exchange("exalted", "divine", &league).await.unwrap();
    println!("offers: {offers:#?}");
    assert!(!offers.is_empty(), "expected at least one offer");
}
```

- [ ] **Step 2: Implement `ExchangeOffer`, `parse_exchange`, and `exchange`** with native Edit.

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct ExchangeOffer {
    pub pay_currency: String,
    pub pay_amount: u32,
    pub get_currency: String,
    pub get_amount: u32,
    pub stock: u64,
}

impl TradeClient {
    /// Query the trade2 currency exchange: how much `want` you receive per
    /// `have`. Returns offers (top-of-book first). Uses the operator/anonymous
    /// session and the Exchange rate bucket. Politeness: reuses the 60s cache.
    pub async fn exchange(&self, have: &str, want: &str, league: &str) -> Result<Vec<ExchangeOffer>> {
        let cache_key = format!("exchange|{league}|{have}|{want}");
        if let Some(hit) = self.exchange_cache_get(&cache_key) {
            return Ok(hit);
        }
        let url = format!("{TRADE_BASE}/exchange/{league}");
        let payload = serde_json::json!({
            "query": { "status": { "option": "online" }, "have": [have], "want": [want] },
            "sort": { "have": "asc" },
            "engine": "new"
        });
        let resp = self
            .send_with_retry(&self.default_limiter, Endpoint::Exchange, || {
                self.http.post(&url).json(&payload)
            })
            .await
            .context("trade2 exchange search failed")?;
        let v: Value = resp.json().await?;
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string();
        let hashes: Vec<String> = v
            .get("result")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|h| h.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if id.is_empty() || hashes.is_empty() {
            return Ok(Vec::new());
        }
        // Exchange fetch: same /fetch endpoint, with &exchange, capped at 10 ids.
        let mut offers = Vec::new();
        for csv in fetch_batches(&hashes) {
            let furl = format!("{TRADE_BASE}/fetch/{csv}?query={id}&exchange");
            let fv: Value = self
                .send_with_retry(&self.default_limiter, Endpoint::Exchange, || self.http.get(&furl))
                .await
                .context("trade2 exchange fetch failed")?
                .json()
                .await?;
            offers.extend(parse_exchange(&fv, have, want));
        }
        // Best ratio first (most `want` per `have`).
        offers.sort_by(|a, b| {
            let ra = a.get_amount as f64 / a.pay_amount.max(1) as f64;
            let rb = b.get_amount as f64 / b.pay_amount.max(1) as f64;
            rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
        });
        self.exchange_cache_put(&cache_key, &offers);
        Ok(offers)
    }

    fn exchange_cache_get(&self, key: &str) -> Option<Vec<ExchangeOffer>> {
        // Reuse the existing `cache` field's TTL semantics if convenient; otherwise
        // a dedicated field. Simplest: a separate Mutex<HashMap<String,(Instant,Vec<ExchangeOffer>)>>.
        let _ = key;
        None // see Step 4 note; wire to a real 60s cache field
    }
    fn exchange_cache_put(&self, _key: &str, _offers: &[ExchangeOffer]) {}
}

/// Parse a trade2 exchange `/fetch&exchange` response into offers.
/// Field paths follow the documented contract; verify against the fixture.
fn parse_exchange(v: &Value, have: &str, want: &str) -> Vec<ExchangeOffer> {
    let mut out = Vec::new();
    let Some(results) = v.get("result").and_then(|x| x.as_array()) else { return out };
    for r in results {
        let Some(offers) = r.pointer("/listing/offers").and_then(|x| x.as_array()) else { continue };
        for o in offers {
            // `exchange` = what the seller wants from us (our `have`/pay).
            // `item`     = what the seller gives (our `want`/get), with stock.
            let pay_amount = o.pointer("/exchange/amount").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
            let get_amount = o.pointer("/item/amount").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
            let stock = o.pointer("/item/stock").and_then(|x| x.as_u64()).unwrap_or(0);
            if pay_amount == 0 || get_amount == 0 {
                continue;
            }
            out.push(ExchangeOffer {
                pay_currency: have.to_string(),
                pay_amount,
                get_currency: want.to_string(),
                get_amount,
                stock,
            });
        }
    }
    out
}
```

> **Cache note (Step 2 cont.):** Add a field `exchange_cache: std::sync::Mutex<HashMap<String,(std::time::Instant, Vec<ExchangeOffer>)>>` to `TradeClient`, initialize it in `new()` (`Mutex::new(HashMap::new())`), and implement `exchange_cache_get`/`exchange_cache_put` with the same 60s TTL pattern as the existing `cache` field (expire entries older than `Duration::from_secs(60)`). This keeps repeated `/arb` calls polite.

- [ ] **Step 3: Capture the fixture** (operator/network step):

```bash
POESESSID=... ARB_TEST_LEAGUE="<active league>" cargo test capture_exchange_fixture -- --ignored --nocapture 2>&1 | tail -40
```

Save the raw fetch JSON (from the println or by adding a temporary `std::fs::write`) to `src/trade/fixtures/exchange_pair.json`. Trim to ~3 listings. If field paths differ from `parse_exchange`, fix the pointers.

- [ ] **Step 4: Write the offline parse test** — add to the `tests` module:

```rust
#[test]
fn parses_exchange_fixture() {
    let v: Value = serde_json::from_str(include_str!("fixtures/exchange_pair.json")).unwrap();
    let offers = parse_exchange(&v, "exalted", "divine");
    assert!(!offers.is_empty());
    let best = &offers[0];
    assert!(best.get_amount > 0 && best.pay_amount > 0 && best.stock > 0);
}
```

- [ ] **Step 5: Run offline test to verify it passes**

Run: `cargo test parses_exchange_fixture 2>&1 | tail -20`
Expected: PASS against the committed fixture.

- [ ] **Step 6: Commit**

```bash
git add src/trade/client.rs src/trade/fixtures/exchange_pair.json
git commit -m "feat(arb): trade2 currency-exchange query + offline fixture parse

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: `CandidateSource` trait + `WatchlistSource`

**Files:**
- Create: `src/arb/source.rs`
- Modify: `src/arb/mod.rs` (add `pub mod source;`)

**Interfaces:**
- Consumes: `Edge`, `RatioQuote`, `Freshness` from `model`; `TradeClient::exchange` + `ExchangeOffer` from `trade::client`.
- Produces:
  - `#[async_trait] pub trait CandidateSource { async fn edges(&self, league: &str) -> Result<Vec<Edge>>; }`
  - `pub struct WatchlistSource { client: Arc<TradeClient>, watchlist: Vec<Currency> }` + `WatchlistSource::new(client, watchlist)`
  - `impl CandidateSource for WatchlistSource` — for every ordered pair `(have, want)` of distinct watchlist currencies, calls `client.exchange(have, want, league)`, takes the best (first) offer as the `Live` `Edge { from: have, to: want, quote }`. Pairs that return no offers are skipped (logged at debug). Errors on a single pair are logged and skipped (never abort the whole sweep).

**Cost note (document in code):** N currencies → N·(N-1) exchange queries, all paced by the limiter. Keep `ARB_WATCHLIST` small (≤ ~6) in Phase 1; whole-market coverage is exactly what the cxapi source (Phase 2) replaces this with.

- [ ] **Step 1: Write the failing test** — `WatchlistSource` needs the network, so test the *trait seam* with an in-memory fake, and test `WatchlistSource` construction only.

Create `src/arb/source.rs`:

```rust
//! Candidate edge sources. Phase 1: WatchlistSource (live trade2). The engine
//! depends only on the `CandidateSource` trait, so Phase 2's cxapi source slots
//! in behind the same interface.

use crate::arb::model::{Currency, Edge, Freshness, RatioQuote};
use crate::trade::client::TradeClient;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait CandidateSource: Send + Sync {
    async fn edges(&self, league: &str) -> Result<Vec<Edge>>;
}

pub struct WatchlistSource {
    client: Arc<TradeClient>,
    watchlist: Vec<Currency>,
}

impl WatchlistSource {
    pub fn new(client: Arc<TradeClient>, watchlist: Vec<Currency>) -> Self {
        WatchlistSource { client, watchlist }
    }
}

#[async_trait]
impl CandidateSource for WatchlistSource {
    async fn edges(&self, league: &str) -> Result<Vec<Edge>> {
        let mut edges = Vec::new();
        for have in &self.watchlist {
            for want in &self.watchlist {
                if have == want {
                    continue;
                }
                match self.client.exchange(have, want, league).await {
                    Ok(offers) => {
                        if let Some(best) = offers.into_iter().next() {
                            edges.push(Edge {
                                from: have.clone(),
                                to: want.clone(),
                                quote: RatioQuote {
                                    pay: best.pay_amount,
                                    get: best.get_amount,
                                    stock: best.stock,
                                    freshness: Freshness::Live,
                                },
                            });
                        }
                    }
                    Err(e) => tracing::warn!(%have, %want, error = %e, "exchange pair failed; skipping"),
                }
            }
        }
        Ok(edges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSource(Vec<Edge>);
    #[async_trait]
    impl CandidateSource for FakeSource {
        async fn edges(&self, _league: &str) -> Result<Vec<Edge>> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn trait_seam_returns_edges() {
        let src = FakeSource(vec![Edge {
            from: "A".into(),
            to: "B".into(),
            quote: RatioQuote { pay: 1, get: 2, stock: 10, freshness: Freshness::Live },
        }]);
        let edges = src.edges("X").await.unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to, "B");
    }
}
```

Add `pub mod source;` to `src/arb/mod.rs`. Confirm `async-trait` is a dependency (it is — `TradeApi` uses `#[async_trait]`).

- [ ] **Step 2: Run to verify**

Run: `cargo test arb::source 2>&1 | tail -20`
Expected: `trait_seam_returns_edges ... ok`.

- [ ] **Step 3–4: Fix and re-run as needed.**

- [ ] **Step 5: Commit**

```bash
git add src/arb/source.rs src/arb/mod.rs
git commit -m "feat(arb): CandidateSource trait + live trade2 WatchlistSource

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: `ArbEngine` orchestration (`mod.rs`)

**Files:**
- Modify: `src/arb/mod.rs` (add `ArbConfig`, `ArbEngine`)

**Interfaces:**
- Consumes: `CandidateSource`, `RateGraph`, `spread::scan`, `Opportunity`.
- Produces:
  - `pub struct ArbConfig { pub max_cycle_len: usize, pub min_profit_pct: f64, pub min_spread_pct: f64, pub min_volume: f64, pub top_n: usize }`
  - `pub struct ArbEngine { source: Arc<dyn CandidateSource>, cfg: ArbConfig }` + `ArbEngine::new(source, cfg)`
  - `pub async fn opportunities(&self, league: &str) -> Result<Vec<Opportunity>>` — fetch edges, run graph + spread, wrap into `Opportunity` (all `Freshness::Live` in Phase 1), filter triangulations by `feasible_volume >= min_volume`, sort all by `score()` desc, truncate to `top_n`.

- [ ] **Step 1: Write the failing test** — append to `src/arb/mod.rs`:

```rust
pub mod graph;
pub mod model;
pub mod source;
pub mod spread;

use crate::arb::model::{Freshness, Opportunity};
use crate::arb::source::CandidateSource;
use anyhow::Result;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct ArbConfig {
    pub max_cycle_len: usize,
    pub min_profit_pct: f64,
    pub min_spread_pct: f64,
    pub min_volume: f64,
    pub top_n: usize,
}

pub struct ArbEngine {
    source: Arc<dyn CandidateSource>,
    cfg: ArbConfig,
}

impl ArbEngine {
    pub fn new(source: Arc<dyn CandidateSource>, cfg: ArbConfig) -> Self {
        ArbEngine { source, cfg }
    }

    pub async fn opportunities(&self, league: &str) -> Result<Vec<Opportunity>> {
        let edges = self.source.edges(league).await?;
        let graph = graph::RateGraph::from_edges(&edges);
        let cycles = graph.profitable_cycles(self.cfg.max_cycle_len, self.cfg.min_profit_pct);
        let flips = spread::scan(&edges, self.cfg.min_spread_pct, self.cfg.min_volume);

        let mut opps: Vec<Opportunity> = Vec::new();
        for c in cycles {
            if c.feasible_volume >= self.cfg.min_volume {
                opps.push(Opportunity::from_cycle(c, Freshness::Live));
            }
        }
        for f in flips {
            opps.push(Opportunity::from_flip(f, Freshness::Live));
        }
        opps.sort_by(|a, b| b.score().partial_cmp(&a.score()).unwrap_or(std::cmp::Ordering::Equal));
        opps.truncate(self.cfg.top_n);
        Ok(opps)
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;
    use crate::arb::model::{Edge, RatioQuote};
    use crate::arb::source::CandidateSource;
    use async_trait::async_trait;

    struct Fixed(Vec<Edge>);
    #[async_trait]
    impl CandidateSource for Fixed {
        async fn edges(&self, _l: &str) -> Result<Vec<Edge>> { Ok(self.0.clone()) }
    }

    fn e(from: &str, to: &str, pay: u32, get: u32, stock: u64) -> Edge {
        Edge { from: from.into(), to: to.into(), quote: RatioQuote { pay, get, stock, freshness: Freshness::Live } }
    }

    #[tokio::test]
    async fn surfaces_and_ranks() {
        // One +20% triangle.
        let edges = vec![e("A","B",1,2,1000), e("B","C",1,2,1000), e("C","A",10,3,1000)];
        let eng = ArbEngine::new(Arc::new(Fixed(edges)), ArbConfig {
            max_cycle_len: 4, min_profit_pct: 0.0, min_spread_pct: 0.5, min_volume: 0.0, top_n: 10,
        });
        let opps = eng.opportunities("X").await.unwrap();
        assert!(matches!(opps[0], Opportunity::Triangulation { .. }));
    }

    #[tokio::test]
    async fn abstains_when_nothing_clears() {
        let edges = vec![e("A","B",1,2,100), e("B","A",2,1,100)];
        let eng = ArbEngine::new(Arc::new(Fixed(edges)), ArbConfig {
            max_cycle_len: 4, min_profit_pct: 0.5, min_spread_pct: 0.5, min_volume: 0.0, top_n: 10,
        });
        assert!(eng.opportunities("X").await.unwrap().is_empty());
    }
}
```

(Remove the now-duplicated `pub mod` lines if they already exist at the top of `mod.rs` from earlier tasks — keep one set.)

- [ ] **Step 2: Run to verify**

Run: `cargo test arb:: 2>&1 | tail -30`
Expected: all `arb` tests pass, including `surfaces_and_ranks` and `abstains_when_nothing_clears`.

- [ ] **Step 3–4: Fix and re-run.**

- [ ] **Step 5: Commit**

```bash
git add src/arb/mod.rs
git commit -m "feat(arb): ArbEngine orchestration (screen + rank + abstain)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: Config — `ARB_*` fields

**Files:**
- Modify: `src/config.rs` (struct `Config`, `from_lookup`, Debug impl if it lists fields, tests)

**Interfaces:**
- Produces on `Config`: `pub arb_watchlist: Vec<String>`, `pub arb_min_profit_pct: f64`, `pub arb_min_spread_pct: f64`, `pub arb_min_volume: f64`, `pub arb_max_cycle_len: usize`, `pub arb_top_n: usize`.

Defaults: watchlist `["divine","exalted","chaos","annul","regal","vaal"]` (these are trade2 exchange currency ids — **verify against the live exchange and the Task 6 fixture; adjust if a code differs**), `arb_min_profit_pct = 0.03`, `arb_min_spread_pct = 0.03`, `arb_min_volume = 0.0`, `arb_max_cycle_len = 4`, `arb_top_n = 8`.

- [ ] **Step 1: Write the failing test** — add to `config.rs` `tests`:

```rust
#[test]
fn arb_defaults_apply_when_unset() {
    let cfg = Config::from_lookup(|k| match k {
        "DISCORD_TOKEN" => Some("t".into()),
        "GUILD_ID" => Some("1".into()),
        _ => None,
    }).unwrap();
    assert_eq!(cfg.arb_max_cycle_len, 4);
    assert_eq!(cfg.arb_top_n, 8);
    assert_eq!(cfg.arb_watchlist, vec!["divine","exalted","chaos","annul","regal","vaal"]);
    assert!((cfg.arb_min_profit_pct - 0.03).abs() < 1e-9);
}

#[test]
fn arb_watchlist_parses_csv() {
    let cfg = Config::from_lookup(|k| match k {
        "DISCORD_TOKEN" => Some("t".into()),
        "GUILD_ID" => Some("1".into()),
        "ARB_WATCHLIST" => Some("divine, exalted ,chaos".into()),
        _ => None,
    }).unwrap();
    assert_eq!(cfg.arb_watchlist, vec!["divine","exalted","chaos"]);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test config:: 2>&1 | tail -20`
Expected: compile error — missing fields.

- [ ] **Step 3: Implement** with native Edit. Add the six fields to `Config`. In `from_lookup`, before the final `Ok(Self { ... })`, parse them:

```rust
let arb_watchlist = match get("ARB_WATCHLIST").filter(|s| !s.is_empty()) {
    Some(v) => v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
    None => ["divine","exalted","chaos","annul","regal","vaal"].iter().map(|s| s.to_string()).collect(),
};
let arb_min_profit_pct = match get("ARB_MIN_PROFIT_PCT") {
    Some(v) => v.parse::<f64>().context("ARB_MIN_PROFIT_PCT must be a number")?,
    None => 0.03,
};
let arb_min_spread_pct = match get("ARB_MIN_SPREAD_PCT") {
    Some(v) => v.parse::<f64>().context("ARB_MIN_SPREAD_PCT must be a number")?,
    None => 0.03,
};
let arb_min_volume = match get("ARB_MIN_VOLUME") {
    Some(v) => v.parse::<f64>().context("ARB_MIN_VOLUME must be a number")?,
    None => 0.0,
};
let arb_max_cycle_len = match get("ARB_MAX_CYCLE_LEN") {
    Some(v) => v.parse::<usize>().context("ARB_MAX_CYCLE_LEN must be a usize")?,
    None => 4,
};
let arb_top_n = match get("ARB_CONFIRM_TOP_N") {
    Some(v) => v.parse::<usize>().context("ARB_CONFIRM_TOP_N must be a usize")?,
    None => 8,
};
```

Add the six fields to the returned `Self { ... }`. If `impl std::fmt::Debug for Config` enumerates fields, add the new ones (they are non-secret, safe to print).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test config:: 2>&1 | tail -20`
Expected: both new tests + existing config tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(arb): ARB_* configuration fields with defaults

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 10: `/arb` command + embed + wiring

**Files:**
- Create: `src/discord/arb.rs`
- Modify: `src/discord/embeds.rs` (add `arb_embed`), `src/discord/mod.rs` (add `pub mod arb;`), `src/main.rs` (build `ArbEngine`, store it in `Data`, register `discord::arb::arb()`)

**Interfaces:**
- Consumes: `ArbEngine`, `Opportunity`, `Data`, `Context`, `Error`, the store's current league.
- Produces: `pub fn arb() -> poise::Command<Data, Error>` (via `#[poise::command]`), `embeds::arb_embed(opps: &[Opportunity], league: &str) -> serenity::CreateEmbed`.
- Adds field to `Data`: `pub arb: Arc<crate::arb::ArbEngine>`.

- [ ] **Step 1: Add `ArbEngine` to `Data`** (native Edit, `src/discord/mod.rs`): add `pub arb: std::sync::Arc<crate::arb::ArbEngine>,` to `struct Data`, and `pub mod arb;`.

- [ ] **Step 2: Build the engine in `main.rs`** (native Edit). After `sessions` is built and before the framework, construct a dedicated trade client for arb (operator/anonymous) and the engine:

```rust
let arb_client = std::sync::Arc::new(TradeClient::new(config.poesessid.clone(), rates.clone())?);
let arb_source = std::sync::Arc::new(crate::arb::source::WatchlistSource::new(
    arb_client,
    config.arb_watchlist.clone(),
));
let arb_engine = std::sync::Arc::new(crate::arb::ArbEngine::new(
    arb_source,
    crate::arb::ArbConfig {
        max_cycle_len: config.arb_max_cycle_len,
        min_profit_pct: config.arb_min_profit_pct,
        min_spread_pct: config.arb_min_spread_pct,
        min_volume: config.arb_min_volume,
        top_n: config.arb_top_n,
    },
));
```

Add `discord::arb::arb(),` to the `commands: vec![...]` list, and `arb: arb_engine,` to the `Ok(Data { ... })` constructor.

- [ ] **Step 3: Write the command + embed.** Create `src/discord/arb.rs`:

```rust
use crate::arb::model::Opportunity;
use crate::discord::{embeds, Context, Error};

/// Find currency flip and triangulation opportunities right now.
#[poise::command(slash_command)]
pub async fn arb(ctx: Context<'_>) -> Result<(), Error> {
    let Some(snap) = ctx.data().store.snapshot().await else {
        ctx.say("Still warming up — try again in a few seconds.").await?;
        return Ok(());
    };
    let league = snap.league.clone();
    // Live trade2 queries take seconds; defer so Discord doesn't time out.
    ctx.defer().await?;

    match ctx.data().arb.opportunities(&league).await {
        Ok(opps) if opps.is_empty() => {
            ctx.say(format!(
                "No currency arbitrage above the configured thresholds right now ({league})."
            )).await?;
        }
        Ok(opps) => {
            ctx.send(poise::CreateReply::default().embed(embeds::arb_embed(&opps, &league)))
                .await?;
        }
        Err(e) => {
            tracing::warn!(error = %e, "arb scan failed");
            ctx.say("Couldn't scan the exchange just now — try again shortly.").await?;
        }
    }
    Ok(())
}
```

Add `embeds::arb_embed` to `src/discord/embeds.rs` (match the existing embed style; `farm_embed` is the template):

```rust
pub fn arb_embed(opps: &[Opportunity], league: &str) -> serenity::CreateEmbed {
    let mut lines = String::new();
    for (i, o) in opps.iter().enumerate() {
        match o {
            Opportunity::Triangulation { legs, multiplier, feasible_volume, .. } => {
                let path = std::iter::once(legs[0].from.as_str())
                    .chain(legs.iter().map(|l| l.to.as_str()))
                    .collect::<Vec<_>>()
                    .join(" → ");
                lines.push_str(&format!(
                    "**{}. Cycle** `{}`  +{:.1}%  (~{:.0} vol)\n",
                    i + 1, path, (multiplier - 1.0) * 100.0, feasible_volume
                ));
            }
            Opportunity::Flip { market, spread_pct, volume, .. } => {
                lines.push_str(&format!(
                    "**{}. Flip** `{} / {}`  {:.1}% spread  (~{:.0} vol)\n",
                    i + 1, market.0, market.1, spread_pct * 100.0, volume
                ));
            }
        }
    }
    if lines.is_empty() {
        lines.push_str("Nothing above thresholds.");
    }
    serenity::CreateEmbed::new()
        .title("⚖️ Currency arbitrage")
        .description(lines)
        .footer(serenity::CreateEmbedFooter::new(format!(
            "{league} • execute manually in-game; ratios move fast"
        )))
}
```

Add `use crate::arb::model::Opportunity;` to the top of `embeds.rs` (and confirm `serenity` items used — `CreateEmbed`, `CreateEmbedFooter` — match how the file already imports serenity).

- [ ] **Step 4: Build and verify it compiles + all tests still pass**

Run: `cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -20`
Expected: clean build; all tests pass. (No automated test for the Discord handler — it needs a live gateway. Verified manually in Step 5.)

- [ ] **Step 5: Manual smoke (operator, optional)** — run the bot against the guild with a valid `DISCORD_TOKEN`/`POESESSID`, invoke `/arb`, confirm an embed (or the honest "nothing above thresholds" message) returns within a few seconds.

> Note: this is a Discord-runtime change that cannot be verified by `cargo test`. State that explicitly in the PR; the engine logic itself is covered by the offline tests in Tasks 2–8.

- [ ] **Step 6: Commit**

```bash
git add src/discord/arb.rs src/discord/embeds.rs src/discord/mod.rs src/main.rs
git commit -m "feat(arb): /arb command, embed, and engine wiring

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 11: Docs — `.env.example` + CLAUDE.md

**Files:**
- Modify: `.env.example`, `CLAUDE.md`

- [ ] **Step 1: Update `.env.example`** — add a documented block:

```
# --- Currency arbitrage (/arb) ---
# Comma-separated trade2 exchange currency ids to scan. Keep small in Phase 1
# (cost is quadratic: N*(N-1) live queries per /arb). Default: divine,exalted,chaos,annul,regal,vaal
ARB_WATCHLIST=
# Minimum triangulation profit (fraction) to surface. Default 0.03 (3%).
ARB_MIN_PROFIT_PCT=
# Minimum flip maker-spread (fraction) to surface. Default 0.03.
ARB_MIN_SPREAD_PCT=
# Minimum feasible volume to surface. Default 0.
ARB_MIN_VOLUME=
# Max triangulation cycle length. Default 4.
ARB_MAX_CYCLE_LEN=
# Max opportunities returned/confirmed. Default 8.
ARB_CONFIRM_TOP_N=
```

- [ ] **Step 2: Update `CLAUDE.md`** — under "Command surfaces", add:

```
- `/arb` — finds and ranks currency flip (maker-spread) and triangulation
  (cross-rate cycle) opportunities from live trade2 exchange data. Read-only:
  it surfaces opportunities for manual in-game execution; it never trades.
```

And under "Module layout", add the `arb/` block:

```
  arb/               mod.rs (ArbEngine) model.rs graph.rs spread.rs source.rs
```

- [ ] **Step 3: Verify nothing references secrets** and the build is clean:

Run: `cargo build 2>&1 | tail -5 && git diff --stat`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add .env.example CLAUDE.md
git commit -m "docs(arb): document /arb command and ARB_* config

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Final verification (before opening a PR)

- [ ] `cargo fmt`
- [ ] `cargo clippy --all-targets -- -D warnings` (matches CI; fix every warning)
- [ ] `cargo test` (offline suite green)
- [ ] Manually confirm `.env` is NOT staged and no secret is present in the diff
- [ ] PR body states the `/arb` Discord handler is runtime-verified only (engine logic is unit-tested), and that Phase 2 (cxapi whole-market screening) is gated on GGG OAuth approval

## Out of scope (later plans)

- **Phase 2:** `CxapiSource` behind `CandidateSource` + hourly background snapshot in the store + activating the confirm stage for `Aggregated` legs. Blocked on a GGG-approved `service:cxapi` OAuth app.
- **Phase 3:** background alerter posting threshold-beating opportunities to a channel, with de-dup/cooldown.
