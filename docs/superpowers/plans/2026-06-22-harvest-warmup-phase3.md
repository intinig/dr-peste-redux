# Phase 3 — `/harvest` Market Warm-up Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an operator-runnable `/harvest <category>` that price-bands a whole item category across the trade2 market (via the invoking member's own session) and appends every listing to the observation corpus, so the Phase 4 learning layer has broad data to mine — instead of waiting for sparse organic `/paste` traffic.

**Architecture:** `/harvest` runs against the **member's own session** (POESESSID + sticky proxy, prompt-to-connect if absent), exactly like `/paste` — so any member can run it safely and it's paced by the existing per-member throttle. It sweeps a category at several min-price bands (to capture the expensive end trade2's cheapest-first search otherwise hides), fetches each band (the existing ≤10-id batched fetch), and logs each real listing as `Observation { source: Harvest }` to the same corpus Phase 2 built.

**Tech Stack:** Rust; existing `TradeApi` (search/fetch), per-member `TradeSession`/throttle, `ObservationLog`, poise slash commands + autocomplete.

**Design spec:** `docs/superpowers/specs/2026-06-22-pricing-heuristic-and-market-learning-design.md` (Phase 4 in the spec text — "Warm-up: on-demand market harvester"). **Decision revised from the spec:** the harvester uses the **invoking member's session**, not a shared operator session — so any guild member may run it (their own account/IP, per-member throttle), and no operator gating is added.

## Global Constraints

- **Per-member, like `/paste`:** harvest egresses the member's session (`sessions.session_for(uid)`; if absent, prompt-to-connect). No operator/global account.
- **One category per invocation, price-banded:** `/harvest <category>` sweeps that one trade2 category at ascending min-price bands.
- **Corpus reuse:** append `Observation { source: Harvest, category: <trade2 category text>, base_type: <per-listing>, mods, price_divine }` to the existing `ObservationLog` (the durable `/data/observations.jsonl`). Non-secret market data only; never POESESSIDs.
- **Politeness:** all searches/fetches go through the existing throttle (`send_with_retry` → `limiter`) and the ≤10-id batched `fetch`; bound the work per run (a fixed small set of bands × the search cap).
- Binary crate, no lib target — verify with `cargo test` (never `--lib`). Final `cargo build` zero warnings; **CI runs `cargo clippy --all-targets -- -D warnings`** — run that exact command, must be clean.
- Commit trailer (after a blank line): `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Stage files by name; never `git add -A`.

## File structure

| File | Change |
|---|---|
| `src/trade/model.rs` | `Listing.base_type: Option<String>`; `TradeQuery.min_price_divine: Option<f64>` |
| `src/trade/client.rs` | `parse_fetch` sets `base_type` from `item.baseType` |
| `src/trade/query.rs` | `to_payload` emits the price-band filter |
| `src/trade/mod.rs` | `log_observations` uses each listing's `base_type`; `TradePricer::harvest` |
| `src/trade/categories.rs` (**new**) | `CategoryCatalog` — fetch/parse trade2 `/data/filters` category options |
| `src/main.rs` | fetch `CategoryCatalog` at startup; put it in `Data` |
| `src/discord/mod.rs` | add `CategoryCatalog` to `Data`; register `harvest` |
| `src/discord/harvest.rs` (**new**) | `/harvest` command: autocomplete, member session/connect, progress, call `harvest` |

---

## Task 1: `Listing.base_type` from the fetch; log it per-listing

**Files:**
- Modify: `src/trade/model.rs` (`Listing`)
- Modify: `src/trade/client.rs` (`parse_fetch` + test)
- Modify: `src/trade/mod.rs` (`log_observations` + `Listing` literals in tests)

**Interfaces:**
- Produces: `Listing.base_type: Option<String>` (the listed item's base, e.g. "Chiming Staff", from `item.baseType`). `log_observations` records each observation's `base_type` from the listing itself (falling back to the pasted item's base when the listing's is absent).

- [ ] **Step 1: Write the failing extraction test**

In `src/trade/client.rs` `tests`, extend the fetch test:

```rust
    #[test]
    fn parse_fetch_extracts_base_type() {
        let client = test_client();
        let v = serde_json::json!({
            "result": [{
                "id": "abc",
                "listing": { "price": { "amount": 1.0, "currency": "divine" } },
                "item": { "baseType": "Chiming Staff", "explicitMods": [] }
            }]
        });
        let ls = client.parse_fetch(&v);
        assert_eq!(ls.len(), 1);
        assert_eq!(ls[0].base_type.as_deref(), Some("Chiming Staff"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test parse_fetch_extracts_base_type`
Expected: compile error — `Listing` has no `base_type`.

- [ ] **Step 3: Add the field + extract it**

In `src/trade/model.rs`, add to `Listing` (after `id`):

```rust
    /// The listed item's base type (e.g. "Chiming Staff"), from the fetch
    /// `item.baseType`. The corpus join key across paste and harvest.
    pub base_type: Option<String>,
```

In `src/trade/client.rs` `parse_fetch`, inside the per-entry closure (where `item` is already bound), add:

```rust
                        let base_type = item
                            .and_then(|it| it.get("baseType"))
                            .and_then(|b| b.as_str())
                            .map(String::from);
```

and set `base_type` in the `Listing { … }` literal.

- [ ] **Step 4: Log per-listing base_type**

In `src/trade/mod.rs` `log_observations`, set the observation's base from each listing, falling back to the pasted item:

```rust
                base_type: l.base_type.clone().or_else(|| item.base_type.clone()),
```

(replacing the current `base_type: item.base_type.clone()`).

- [ ] **Step 5: Update `Listing` literals**

Add `base_type: None` to each test `Listing { … }` literal (compiler-guided: `src/trade/mod.rs` `Flat`/`make_listing`, `src/trade/ablation.rs` `listing`/`lst`/inline fakes).

- [ ] **Step 6: Run to green**

Run: `cargo test client:: && cargo test` then `cargo build`
Expected: new test passes; suite green; zero warnings.

- [ ] **Step 7: Format, strict clippy, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/trade/model.rs src/trade/client.rs src/trade/mod.rs src/trade/ablation.rs
git commit -m "feat(trade): listings carry base_type; observations record per-listing base"
# + trailer
```

---

## Task 2: Price-band filter on the search query

**Files:**
- Modify: `src/trade/model.rs` (`TradeQuery`)
- Modify: `src/trade/query.rs` (`to_payload` + test; `TradeQuery` literals)

**Interfaces:**
- Produces: `TradeQuery.min_price_divine: Option<f64>` → `to_payload` emits `query.filters.trade_filters.filters.price = { "min": <v>, "option": "divine" }`. `build_baseline`/`base_query` leave it `None`; harvest sets it.

- [ ] **Step 1: Verify the live price-filter JSON shape (load-bearing)**

Before coding, confirm the exact shape trade2 expects, since a wrong shape silently returns cheapest-overall (defeating the warm-up). Add a temporary ignored probe in `src/trade/client.rs` (proxied, anonymous — search works anon via the proxy) that POSTs a `Chiming Staff` search with a `trade_filters.filters.price.min` and prints `total`, comparing min=0 vs min=20:

```rust
    #[tokio::test]
    #[ignore = "diagnostic: confirm trade2 price-filter JSON shape"]
    async fn diag_price_filter() {
        use crate::trade::session::{sticky_proxy_url, ProxyConfig, TradeSession};
        dotenvy::dotenv().ok();
        let cfg = ProxyConfig { gateway: "geo.iproyal.com:32325".into(),
            user: std::env::var("IPROYAL_USERNAME").unwrap(), pass: std::env::var("IPROYAL_PASSWORD").unwrap(),
            country: "de".into(), lifetime_mins: 30 };
        let client = reqwest::Client::builder().user_agent(USER_AGENT)
            .proxy(reqwest::Proxy::all(sticky_proxy_url(&cfg, 999)).unwrap()).build().unwrap();
        let nc = crate::poeninja::NinjaClient::new().unwrap();
        let league = nc.current_league().await.unwrap().name;
        for min in [0.0_f64, 20.0] {
            let body = serde_json::json!({ "query": { "status": {"option":"online"}, "type": "Chiming Staff",
                "stats": [{"type":"and","filters":[]}],
                "filters": { "trade_filters": { "filters": { "price": { "min": min, "option": "divine" } } } } },
                "sort": { "price": "asc" } });
            let r = client.post(format!("{TRADE_BASE}/search/{league}")).json(&body).send().await.unwrap();
            let st = r.status(); let v: serde_json::Value = r.json().await.unwrap_or(serde_json::json!({}));
            eprintln!("DIAG min={min} HTTP {st} total={}", v.get("total").and_then(|t| t.as_u64()).unwrap_or(0));
        }
    }
```

Run: `cargo test --quiet diag_price_filter -- --ignored --nocapture 2>&1 | grep DIAG`
Expected: both 200; `total` for min=20 is **smaller** than min=0 (the filter works). If HTTP 400 or totals equal, adjust the JSON (e.g. `price` may need to be under `filters` directly, or `option` omitted) until min=20 < min=0, and use the confirmed shape below. **Delete this probe after confirming** (do not commit it).

- [ ] **Step 2: Write the failing `to_payload` test**

In `src/trade/query.rs` `tests`:

```rust
    #[test]
    fn to_payload_emits_min_price_band() {
        let q = TradeQuery {
            league: "L".into(), category: Some("weapon.staff".into()), type_line: None,
            stats: vec![], misc: MiscFilters::default(), equipment: vec![],
            min_price_divine: Some(20.0),
        };
        let p = to_payload(&q);
        assert_eq!(p["query"]["filters"]["trade_filters"]["filters"]["price"]["min"], 20.0);
        assert_eq!(p["query"]["filters"]["trade_filters"]["filters"]["price"]["option"], "divine");
    }

    #[test]
    fn to_payload_omits_price_when_none() {
        let q = TradeQuery {
            league: "L".into(), category: None, type_line: Some("Chiming Staff".into()),
            stats: vec![], misc: MiscFilters::default(), equipment: vec![],
            min_price_divine: None,
        };
        let p = to_payload(&q);
        assert!(p["query"]["filters"].get("trade_filters").is_none());
    }
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test to_payload_emits_min_price_band to_payload_omits_price_when_none`
Expected: compile error — `TradeQuery` has no `min_price_divine`.

- [ ] **Step 4: Add the field + emit it**

In `src/trade/model.rs` `TradeQuery`, add:

```rust
    /// Minimum listing price in Divine for a price-banded search (harvest only;
    /// `None` for normal pricing searches).
    pub min_price_divine: Option<f64>,
```

In `src/trade/query.rs` `to_payload`, after the `equipment_filters` are added to `query["filters"]`, add (use the shape confirmed in Step 1):

```rust
    if let Some(min) = q.min_price_divine {
        query["filters"]["trade_filters"] =
            json!({ "filters": { "price": { "min": min, "option": "divine" } } });
    }
```

- [ ] **Step 5: Update `TradeQuery` literals**

Add `min_price_divine: None` to every `TradeQuery { … }` literal the compiler flags (`build_baseline`, `base_query`, and test literals in `query.rs`, `observe.rs`, `mod.rs`, `client.rs`, `ablation.rs`).

- [ ] **Step 6: Run to green**

Run: `cargo test query:: && cargo test` then `cargo build`
Expected: new tests pass; suite green; zero warnings.

- [ ] **Step 7: Format, strict clippy, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add <the files the compiler flagged for the TradeQuery literal + model.rs + query.rs>
git commit -m "feat(trade): min-price band filter on search queries (harvest)"
# + trailer
```

(Stage exactly the files you changed — `model.rs`, `query.rs`, plus each file the compiler flagged for the new `TradeQuery` literal field. Don't stage files you didn't touch.)

---

## Task 3: `CategoryCatalog` — trade2 category taxonomy for autocomplete

**Files:**
- Create: `src/trade/categories.rs`
- Modify: `src/trade/mod.rs` (`pub mod categories;`)
- Modify: `src/main.rs` (fetch at startup), `src/discord/mod.rs` (`Data`)

**Interfaces:**
- Produces:
  - `pub struct Category { pub id: String, pub text: String }`
  - `pub struct CategoryCatalog { categories: Vec<Category> }` with `pub fn from_filters_json(body: &str) -> Self` (parse the `category` filter's options), `pub async fn fetch<A: TradeApi-or-client>(...)`, `pub fn all(&self) -> &[Category]`, and `pub fn matches(&self, prefix: &str) -> Vec<&Category>` (case-insensitive prefix match on `text`, for autocomplete).
  - A committed fixture `src/trade/fixtures/filters_sample.json` for offline tests.

- [ ] **Step 1: Capture a fixture + write the failing parse test**

Save a trimmed real `/data/filters` response (the category filter block) to `src/trade/fixtures/filters_sample.json`. Minimum viable content (the parser only needs the `category` filter's `option.options`):

```json
{ "result": [ { "id": "type_filters", "title": "Type Filters", "filters": [
  { "id": "category", "text": "Item Category", "option": { "options": [
    { "id": null, "text": "Any" },
    { "id": "weapon.staff", "text": "Staff" },
    { "id": "weapon.warstaff", "text": "Quarterstaff" },
    { "id": "armour.helmet", "text": "Helmet" },
    { "id": "accessory.amulet", "text": "Amulet" }
  ] } }
] } ] }
```

Then in `src/trade/categories.rs` `tests`:

```rust
    #[test]
    fn parses_category_options_skipping_any() {
        let cat = CategoryCatalog::from_filters_json(include_str!("fixtures/filters_sample.json"));
        let ids: Vec<&str> = cat.all().iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"weapon.staff"));
        assert!(ids.contains(&"accessory.amulet"));
        // The null-id "Any" option is skipped (not a harvestable category).
        assert!(cat.all().iter().all(|c| !c.id.is_empty()));
    }

    #[test]
    fn matches_is_case_insensitive_prefix() {
        let cat = CategoryCatalog::from_filters_json(include_str!("fixtures/filters_sample.json"));
        let m: Vec<&str> = cat.matches("sta").iter().map(|c| c.text.as_str()).collect();
        assert!(m.contains(&"Staff"));
        assert!(!m.iter().any(|t| *t == "Helmet"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test categories::`
Expected: compile error — module/types absent.

- [ ] **Step 3: Implement `categories.rs`**

```rust
//! trade2 item-category taxonomy (from `/data/filters`), used to offer `/harvest`
//! autocomplete and to issue category-filtered searches.

use anyhow::Result;
use serde_json::Value;

use crate::trade::client::TradeClient;

#[derive(Clone, Debug, PartialEq)]
pub struct Category {
    /// trade2 category option id, e.g. "weapon.staff".
    pub id: String,
    /// Human label, e.g. "Staff".
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct CategoryCatalog {
    categories: Vec<Category>,
}

impl CategoryCatalog {
    /// Parses the `category` filter's options out of a `/data/filters` body.
    /// Walks to the object whose `"id" == "category"` and reads `option.options`,
    /// skipping the null-id "Any" entry.
    pub fn from_filters_json(body: &str) -> Self {
        let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let mut categories = Vec::new();
        Self::collect(&v, &mut categories);
        CategoryCatalog { categories }
    }

    fn collect(v: &Value, out: &mut Vec<Category>) {
        match v {
            Value::Object(obj) => {
                if obj.get("id").and_then(|x| x.as_str()) == Some("category") {
                    if let Some(opts) = obj
                        .get("option")
                        .and_then(|o| o.get("options"))
                        .and_then(|o| o.as_array())
                    {
                        for o in opts {
                            let id = o.get("id").and_then(|x| x.as_str());
                            let text = o.get("text").and_then(|x| x.as_str());
                            if let (Some(id), Some(text)) = (id, text) {
                                out.push(Category { id: id.to_string(), text: text.to_string() });
                            }
                        }
                    }
                }
                for (_, val) in obj {
                    Self::collect(val, out);
                }
            }
            Value::Array(arr) => {
                for val in arr {
                    Self::collect(val, out);
                }
            }
            _ => {}
        }
    }

    /// Fetches `/data/filters` and parses it. Empty catalog on error/empty body.
    pub async fn fetch(client: &TradeClient) -> Result<Self> {
        let body = client.fetch_filters_raw().await?;
        Ok(Self::from_filters_json(&body))
    }

    pub fn all(&self) -> &[Category] {
        &self.categories
    }

    /// Case-insensitive prefix match on the human text, for autocomplete.
    pub fn matches(&self, prefix: &str) -> Vec<&Category> {
        let p = prefix.to_lowercase();
        self.categories
            .iter()
            .filter(|c| c.text.to_lowercase().starts_with(&p))
            .collect()
    }

    /// The trade2 category id for an exact human text (autocomplete returns text).
    pub fn id_for_text(&self, text: &str) -> Option<&str> {
        self.categories
            .iter()
            .find(|c| c.text == text)
            .map(|c| c.id.as_str())
    }
}
```

Add `pub mod categories;` to `src/trade/mod.rs`. Add a raw fetch to `TradeClient` (mirrors `fetch_stats_raw`) in `src/trade/client.rs`:

```rust
    /// Fetches the raw `data/filters` taxonomy JSON.
    pub async fn fetch_filters_raw(&self) -> Result<String> {
        let url = format!("{TRADE_BASE}/data/filters");
        Ok(self
            .send_with_retry(&self.default_limiter, Endpoint::Fetch, || self.http.get(&url))
            .await
            .context("trade2 data/filters failed")?
            .text()
            .await?)
    }
```

- [ ] **Step 4: Run to green**

Run: `cargo test categories::`
Expected: PASS (2 tests).

- [ ] **Step 5: Wire into startup + `Data`**

In `src/main.rs`, after the stat catalog is fetched, fetch the category catalog (best-effort; empty on failure) and include it in `Data`:

```rust
    let category_catalog = match trade::categories::CategoryCatalog::fetch(&trade_client).await {
        Ok(c) => {
            tracing::info!(categories = c.all().len(), "loaded trade2 category catalog");
            c
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to fetch category catalog; /harvest autocomplete empty");
            trade::categories::CategoryCatalog::default()
        }
    };
```

Add `pub categories: crate::trade::categories::CategoryCatalog` to the `Data` struct in `src/discord/mod.rs` and set it where `Data` is constructed in `main.rs`.

- [ ] **Step 6: Build + commit**

```bash
cargo build && cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/trade/categories.rs src/trade/mod.rs src/trade/client.rs src/trade/fixtures/filters_sample.json src/main.rs src/discord/mod.rs
git commit -m "feat(trade): CategoryCatalog from /data/filters; loaded at startup"
# + trailer
```

---

## Task 4: `TradePricer::harvest` + the `/harvest` command

**Files:**
- Modify: `src/trade/mod.rs` (`TradePricer::harvest`)
- Create: `src/discord/harvest.rs`
- Modify: `src/discord/mod.rs` (`mod harvest;`), `src/main.rs` (register `harvest`)

**Interfaces:**
- Consumes: `TradeApi::{search, fetch}` (via `self.comparables`), `ObservationLog` (`self.log`), `TradeQuery.min_price_divine` (Task 2), `Listing.base_type` (Task 1), `Source::Harvest`, `CategoryCatalog` (Task 3), the member `TradeSession` + connect flow (mirror `paste.rs`).
- Produces: `TradePricer::harvest(&self, category_id: &str, category_text: &str, league: &str, session: &TradeSession) -> Result<usize>` (returns observations logged); the `/harvest` slash command.

- [ ] **Step 1: Write the failing harvest test**

In `src/trade/mod.rs` `tests`, add a fake that is BOTH `Comparables` and `TradeApi` (harvest uses `TradeApi`):

```rust
    use crate::trade::client::TradeApi;
    use crate::trade::model::{SearchResponse};

    struct HarvestFake;
    #[async_trait]
    impl Comparables for HarvestFake {
        async fn comparables(&self, _q: &TradeQuery, _l: usize, _mr: usize, _s: &TradeSession)
            -> anyhow::Result<Vec<Listing>> { Ok(vec![]) }
    }
    #[async_trait]
    impl TradeApi for HarvestFake {
        async fn search(&self, q: &TradeQuery, _s: &TradeSession) -> anyhow::Result<SearchResponse> {
            // One hash per band; band identified by min_price_divine.
            let band = q.min_price_divine.unwrap_or(0.0);
            Ok(SearchResponse { id: format!("q{band}"), total: 1, hashes: vec![format!("h{band}")] })
        }
        async fn fetch(&self, _id: &str, hashes: &[String], _s: &TradeSession)
            -> anyhow::Result<Vec<Listing>> {
            Ok(hashes.iter().map(|h| Listing {
                price: Money { amount: 1.0, currency: Currency::Divine },
                price_divine: 1.0, explicit_count: 1, id: h.clone(),
                base_type: Some("Chiming Staff".into()),
                mods: vec![crate::trade::model::ListingMod {
                    stat_id: "explicit.stat_1".into(), tier: Some(1), roll: Some(50.0) }],
            }).collect())
        }
    }

    #[tokio::test]
    async fn harvest_logs_one_observation_per_band_listing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obs.jsonl");
        let pricer = TradePricer::new(
            HarvestFake,
            crate::trade::pseudo::PseudoMap::load(),
            crate::trade::stats::StatCatalog::default(),
            crate::observe::ObservationLog::new(&path),
        );
        let n = pricer.harvest("weapon.staff", "Staff", "Standard", &TradeSession::for_test())
            .await.unwrap();
        assert_eq!(n, PRICE_BANDS.len()); // one listing per band
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), PRICE_BANDS.len());
        assert!(body.contains("\"source\":\"harvest\""));
        assert!(body.contains("\"category\":\"Staff\""));
        assert!(body.contains("Chiming Staff"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test harvest_logs_one_observation_per_band_listing`
Expected: compile error — `harvest` / `PRICE_BANDS` undefined.

- [ ] **Step 3: Implement `harvest` on `TradePricer`**

In `src/trade/mod.rs`, add the bands const near `PRICE_SAMPLE`:

```rust
/// Min-price bands (Divine) for a harvest sweep. Each band fetches the cheapest
/// HARVEST_SAMPLE listings at or above it, so together they span the price
/// spectrum (cheapest-first search otherwise hides the expensive end).
const PRICE_BANDS: [f64; 4] = [0.0, 5.0, 20.0, 50.0];
/// Cheapest listings fetched per band.
const HARVEST_SAMPLE: usize = 100;
```

Add a `harvest` method in a `TradeApi`-bounded impl block (so it can issue raw searches/fetches), after the existing `impl<C: Comparables> TradePricer<C>` block:

```rust
impl<C: Comparables + crate::trade::client::TradeApi> TradePricer<C> {
    /// Price-band sweep of a whole category, logging every listing to the corpus
    /// as a Harvest observation. Returns the number of observations logged. Each
    /// search/fetch is throttle-paced by the member session; a per-band failure is
    /// logged and skipped so one bad band doesn't abort the whole harvest.
    pub async fn harvest(
        &self,
        category_id: &str,
        category_text: &str,
        league: &str,
        session: &TradeSession,
    ) -> Result<usize> {
        let mut logged = 0usize;
        for band in PRICE_BANDS {
            let q = crate::trade::model::TradeQuery {
                league: league.to_string(),
                category: Some(category_id.to_string()),
                type_line: None,
                stats: vec![],
                misc: crate::trade::model::MiscFilters::default(),
                equipment: vec![],
                min_price_divine: if band > 0.0 { Some(band) } else { None },
            };
            let resp = match self.comparables.search(&q, session).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, band, "harvest band search failed; skipping");
                    continue;
                }
            };
            let take = resp.hashes.len().min(HARVEST_SAMPLE);
            let listings = match self
                .comparables
                .fetch(&resp.id, &resp.hashes[..take], session)
                .await
            {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(error = %e, band, "harvest band fetch failed; skipping");
                    continue;
                }
            };
            let timestamp_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            for l in &listings {
                let obs = Observation {
                    timestamp_unix,
                    league: league.to_string(),
                    base_type: l.base_type.clone(),
                    category: Some(category_text.to_string()),
                    mods: l.mods.clone(),
                    price_divine: l.price_divine,
                    source: Source::Harvest,
                };
                if self.log.append(&obs).is_ok() {
                    logged += 1;
                } else {
                    tracing::warn!("failed to append harvest observation");
                }
            }
        }
        Ok(logged)
    }
}
```

- [ ] **Step 4: Run the harvest test to green**

Run: `cargo test harvest_logs_one_observation_per_band_listing` then `cargo test`
Expected: PASS — 4 observations (one per band), `source:"harvest"`, category "Staff".

- [ ] **Step 5: The `/harvest` command**

Create `src/discord/harvest.rs` (mirror `paste.rs`'s member-session/connect pattern; reuse `prompt_connect` by making it `pub(crate)` if not already, or duplicate a minimal connect prompt). **Note:** the autocomplete callback signature is poise-version-specific — match poise 0.6's form used elsewhere in this crate (check whether any existing command uses `#[autocomplete]`, e.g. `/price`; follow its exact signature/return type rather than the sketch below if they differ):

```rust
use super::{Context, Error};

/// Autocomplete category names from the loaded trade2 taxonomy.
async fn autocomplete_category<'a>(
    ctx: Context<'a>,
    partial: &'a str,
) -> impl Iterator<Item = String> + 'a {
    ctx.data()
        .categories
        .matches(partial)
        .into_iter()
        .map(|c| c.text.clone())
        .take(25)
        .collect::<Vec<_>>()
        .into_iter()
}

/// Harvest a whole item category into the observation corpus (warms up pricing).
#[poise::command(slash_command)]
pub async fn harvest(
    ctx: Context<'_>,
    #[description = "Item category to harvest"]
    #[autocomplete = "autocomplete_category"]
    category: String,
) -> Result<(), Error> {
    let Some(category_id) = ctx.data().categories.id_for_text(&category).map(str::to_string) else {
        ctx.say(format!("Unknown category `{category}` — pick one from the autocomplete."))
            .await?;
        return Ok(());
    };
    let Some(snap) = ctx.data().store.snapshot().await else {
        ctx.say("Still warming up — try again in a few seconds.").await?;
        return Ok(());
    };
    let uid = ctx.author().id.get();
    let Some(session) = ctx.data().sessions.session_for(uid) else {
        ctx.say("Connect your PoE account first (run /paste once to set your POESESSID), then retry /harvest.")
            .await?;
        return Ok(());
    };
    let reply = ctx
        .send(poise::CreateReply::default().content(format!("⏳ Harvesting **{category}** — this runs several searches against your account…")))
        .await?;
    match ctx.data().pricer.harvest(&category_id, &category, &snap.league.name, &session).await {
        Ok(n) => {
            reply.edit(ctx, poise::CreateReply::default()
                .content(format!("Harvested **{category}**: logged {n} market observations to the corpus."))).await?;
        }
        Err(e) => {
            tracing::warn!(error = %e, "harvest failed");
            reply.edit(ctx, poise::CreateReply::default()
                .content("Harvest hit an error — try again shortly.")).await?;
        }
    }
    Ok(())
}
```

Add `pub mod harvest;` to `src/discord/mod.rs`. Register it in `src/main.rs` `commands: vec![ … discord::harvest::harvest(), … ]`.

(If `Data`'s `pricer` field type is `Arc<TradePricer<TradeClient>>`, the `harvest` method is available because `TradeClient: Comparables + TradeApi`. Confirm the connect-prompt copy matches `paste.rs`'s tone; reuse `prompt_connect` if convenient.)

- [ ] **Step 6: Build, full suite, strict clippy**

Run: `cargo build` then `cargo test` then `cargo clippy --all-targets -- -D warnings`
Expected: zero warnings; suite green; clippy clean.

- [ ] **Step 7: Format, commit**

```bash
cargo fmt
git add src/trade/mod.rs src/discord/harvest.rs src/discord/mod.rs src/main.rs
git commit -m "feat(discord): /harvest <category> price-banded market warm-up"
# + trailer
```

---

## Deploy notes

- No infra change needed — the observation volume from Phase 2 already persists the corpus; harvest writes to the same `OBSERVATION_LOG_PATH`.
- After deploy: run `/harvest Staff` (connect first via `/paste` if prompted), confirm it replies with a logged count, and over SSH confirm `/opt/dr-peste-redux/data/observations.jsonl` gained `source:"harvest"` lines spanning price bands.

## Final verification (after all tasks)

- [ ] `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` green; `cargo build` zero warnings.
- [ ] No leftover diagnostic probe (Task 2 Step 1) committed.
- [ ] **Manual live acceptance** (after deploy): `/harvest <category>` autocompletes from the real taxonomy, runs against the member session, and appends `source:"harvest"` observations across bands to the durable corpus; a category with expensive listings shows entries well above the floor (the bands captured the expensive end).
- [ ] Note for Phase 4: harvest stores `category` as the trade2 text (e.g. "Staff") while `/paste` stores the clipboard Item Class (e.g. "Staves"); `base_type` is the consistent join. The learning layer normalizes category keys.
