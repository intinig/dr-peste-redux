# PoE2 Guild Price-Check Discord Bot — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a self-hosted Rust Discord bot for a PoE2 guild that price-checks items (`/price`, `/pricecheck` modal) and surfaces the best things to farm (`/farm`), using poe.ninja's PoE2 economy API.

**Architecture:** One long-running `tokio` process. A background refresher polls poe.ninja every N minutes and atomically swaps a normalized in-memory snapshot into a shared `PriceStore`. poise command handlers read the store only — they never call poe.ninja directly. Pure logic (config, normalization, search, ranking, item-text parsing) is unit-tested offline against committed fixtures; Discord wiring is built and smoke-tested live.

**Tech Stack:** Rust 1.87, tokio, poise 0.6 (on serenity 0.12), reqwest 0.12 (rustls), serde, fuzzy-matcher, tracing, dotenvy, anyhow/thiserror.

**Reference:** Design spec at `docs/superpowers/specs/2026-06-16-discord-poe2-price-bot-design.md`. Project conventions in `CLAUDE.md`.

---

## Verified API facts (do not re-derive)

- Base URL: `https://poe.ninja/poe2/api`. No auth. Server-side requests have no CORS restriction.
- League list: `GET /data/index-state` → `economyLeagues[]` of `{name, url, hardcore, indexed}`.
- Two endpoint families (both take `?league=<name>&type=<Type>`), both return `core` with `{primary, secondary, rates}`:
  - `economy/exchange/current/overview` — `items[]` (`{id,name,image,detailsId}`) + `lines[]` (`{id, primaryValue, volumePrimaryValue, sparkline:{totalChange}}`), joined on `id`.
  - `economy/stash/current/item/overview` — self-contained `lines[]` (`{name, baseType, detailsId, icon, primaryValue, listingCount, sparkLine:{totalChange}}`). Note `sparkLine` capital L.
- `primaryValue` is denominated in `core.primary` (e.g. `divine`). `core.rates` maps currency id → multiplier (e.g. `{"exalted":186.7,"chaos":11.27}`), so `value_in(X) = primaryValue * (if X==primary {1.0} else rates[X])`.
- Exchange `image` is a site-relative path (`/gen/image/...`); prefix with `https://poe.ninja`. Stash `icon` is already absolute.

### The 23-category registry (slug → family, type) — all verified live

Exchange family: `currency`→`Currency`, `fragments`→`Fragments`, `abyssal-bones`→`Abyss`, `uncut-gems`→`UncutGems`, `lineage-support-gems`→`LineageSupportGems`, `essences`→`Essences`, `soul-cores`→`SoulCores`, `idols`→`Idols`, `runes`→`Runes`, `omens`→`Ritual`, `expedition`→`Expedition`, `liquid-emotions`→`Delirium`, `breach-catalyst`→`Breach`, `verisium`→`Verisium`.

Stash-item family: `precursor-tablets`→`PrecursorTablets`, `unique-weapons`→`UniqueWeapons`, `unique-armours`→`UniqueArmours`, `unique-accessories`→`UniqueAccessories`, `unique-flasks`→`UniqueFlasks`, `unique-charms`→`UniqueCharms`, `unique-jewels`→`UniqueJewels`, `unique-relics`→`UniqueSanctumRelics`, `unique-tablets`→`UniqueTablets`.

---

## Task 1: Project scaffold

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `.env.example`
- Create: `rust-toolchain.toml`

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[package]
name = "dr-peste-redux"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
serenity = { version = "0.12", default-features = false, features = ["client", "gateway", "rustls_backend", "model"] }
poise = "0.6"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
dotenvy = "0.15"
fuzzy-matcher = "0.3"
futures = "0.3"
```

- [ ] **Step 2: Create `rust-toolchain.toml`**

```toml
[toolchain]
channel = "stable"
```

- [ ] **Step 3: Create a placeholder `src/main.rs`**

```rust
fn main() {
    println!("dr-peste-redux: scaffold");
}
```

- [ ] **Step 4: Create `.env.example`**

```bash
# Discord bot token (https://discord.com/developers/applications) — SECRET
DISCORD_TOKEN=your-bot-token-here
# Guild (server) ID for instant slash-command registration
GUILD_ID=000000000000000000
# How often to refresh poe.ninja data, in minutes (default 30)
POLL_INTERVAL_MINS=30
# Minimum trade volume / listing count for an item to appear in /farm (default 0)
MIN_VOLUME=0
```

- [ ] **Step 5: Build to fetch and lock dependencies**

Run: `cargo build`
Expected: compiles successfully (downloads crates), prints `Finished` and produces `target/debug/dr-peste-redux`. The crate `Cargo.lock` is created.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml src/main.rs .env.example
git commit -m "chore: scaffold Rust project with dependencies"
```

---

## Task 2: Config module

**Files:**
- Create: `src/config.rs`
- Modify: `src/main.rs` (add `mod config;`)

- [ ] **Step 1: Add the module declaration to `src/main.rs`**

Replace the entire file with:

```rust
mod config;

fn main() {
    println!("dr-peste-redux: scaffold");
}
```

- [ ] **Step 2: Write `src/config.rs` with a failing test first**

```rust
use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub discord_token: String,
    pub guild_id: u64,
    pub poll_interval_mins: u64,
    pub min_volume: f64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let discord_token = get("DISCORD_TOKEN")
            .filter(|s| !s.is_empty())
            .context("DISCORD_TOKEN must be set")?;
        let guild_id = get("GUILD_ID")
            .context("GUILD_ID must be set")?
            .parse::<u64>()
            .context("GUILD_ID must be a valid u64")?;
        let poll_interval_mins = match get("POLL_INTERVAL_MINS") {
            Some(v) => v.parse::<u64>().context("POLL_INTERVAL_MINS must be a u64")?,
            None => 30,
        };
        let min_volume = match get("MIN_VOLUME") {
            Some(v) => v.parse::<f64>().context("MIN_VOLUME must be a number")?,
            None => 0.0,
        };
        Ok(Self { discord_token, guild_id, poll_interval_mins, min_volume })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn parses_full_config() {
        let cfg = Config::from_lookup(lookup(&[
            ("DISCORD_TOKEN", "abc"),
            ("GUILD_ID", "123"),
            ("POLL_INTERVAL_MINS", "15"),
            ("MIN_VOLUME", "5.5"),
        ]))
        .unwrap();
        assert_eq!(cfg.discord_token, "abc");
        assert_eq!(cfg.guild_id, 123);
        assert_eq!(cfg.poll_interval_mins, 15);
        assert_eq!(cfg.min_volume, 5.5);
    }

    #[test]
    fn applies_defaults() {
        let cfg = Config::from_lookup(lookup(&[("DISCORD_TOKEN", "abc"), ("GUILD_ID", "1")])).unwrap();
        assert_eq!(cfg.poll_interval_mins, 30);
        assert_eq!(cfg.min_volume, 0.0);
    }

    #[test]
    fn missing_token_errors() {
        assert!(Config::from_lookup(lookup(&[("GUILD_ID", "1")])).is_err());
    }

    #[test]
    fn non_numeric_guild_errors() {
        assert!(Config::from_lookup(lookup(&[("DISCORD_TOKEN", "a"), ("GUILD_ID", "x")])).is_err());
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test config::`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: config loading from environment with defaults"
```

---

## Task 3: poe.ninja category registry

**Files:**
- Create: `src/poeninja/mod.rs` (module stub for now)
- Create: `src/poeninja/categories.rs`
- Modify: `src/main.rs` (add `mod poeninja;`)

- [ ] **Step 1: Add `mod poeninja;` to `src/main.rs`**

```rust
mod config;
mod poeninja;

fn main() {
    println!("dr-peste-redux: scaffold");
}
```

- [ ] **Step 2: Create `src/poeninja/mod.rs` declaring the submodule**

```rust
pub mod categories;
```

- [ ] **Step 3: Write `src/poeninja/categories.rs` with tests**

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    Exchange,
    StashItem,
}

impl Family {
    pub fn path(self) -> &'static str {
        match self {
            Family::Exchange => "exchange/current/overview",
            Family::StashItem => "stash/current/item/overview",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Category {
    pub slug: &'static str,
    pub type_param: &'static str,
    pub display: &'static str,
    pub family: Family,
}

use Family::{Exchange as EX, StashItem as ST};

pub const CATEGORIES: &[Category] = &[
    Category { slug: "currency", type_param: "Currency", display: "Currency", family: EX },
    Category { slug: "fragments", type_param: "Fragments", display: "Fragments", family: EX },
    Category { slug: "abyssal-bones", type_param: "Abyss", display: "Abyssal Bones", family: EX },
    Category { slug: "uncut-gems", type_param: "UncutGems", display: "Uncut Gems", family: EX },
    Category { slug: "lineage-support-gems", type_param: "LineageSupportGems", display: "Lineage Support Gems", family: EX },
    Category { slug: "essences", type_param: "Essences", display: "Essences", family: EX },
    Category { slug: "soul-cores", type_param: "SoulCores", display: "Soul Cores", family: EX },
    Category { slug: "idols", type_param: "Idols", display: "Idols", family: EX },
    Category { slug: "runes", type_param: "Runes", display: "Runes", family: EX },
    Category { slug: "omens", type_param: "Ritual", display: "Omens", family: EX },
    Category { slug: "expedition", type_param: "Expedition", display: "Expedition", family: EX },
    Category { slug: "liquid-emotions", type_param: "Delirium", display: "Liquid Emotions", family: EX },
    Category { slug: "breach-catalyst", type_param: "Breach", display: "Breach Catalysts", family: EX },
    Category { slug: "verisium", type_param: "Verisium", display: "Verisium", family: EX },
    Category { slug: "precursor-tablets", type_param: "PrecursorTablets", display: "Precursor Tablets", family: ST },
    Category { slug: "unique-weapons", type_param: "UniqueWeapons", display: "Unique Weapons", family: ST },
    Category { slug: "unique-armours", type_param: "UniqueArmours", display: "Unique Armours", family: ST },
    Category { slug: "unique-accessories", type_param: "UniqueAccessories", display: "Unique Accessories", family: ST },
    Category { slug: "unique-flasks", type_param: "UniqueFlasks", display: "Unique Flasks", family: ST },
    Category { slug: "unique-charms", type_param: "UniqueCharms", display: "Unique Charms", family: ST },
    Category { slug: "unique-jewels", type_param: "UniqueJewels", display: "Unique Jewels", family: ST },
    Category { slug: "unique-relics", type_param: "UniqueSanctumRelics", display: "Unique Relics", family: ST },
    Category { slug: "unique-tablets", type_param: "UniqueTablets", display: "Unique Tablets", family: ST },
];

pub fn by_slug(slug: &str) -> Option<&'static Category> {
    CATEGORIES.iter().find(|c| c.slug == slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_all_23_categories() {
        assert_eq!(CATEGORIES.len(), 23);
    }

    #[test]
    fn slugs_are_unique() {
        let mut slugs: Vec<&str> = CATEGORIES.iter().map(|c| c.slug).collect();
        slugs.sort();
        slugs.dedup();
        assert_eq!(slugs.len(), 23);
    }

    #[test]
    fn lookup_resolves_tricky_types() {
        assert_eq!(by_slug("omens").unwrap().type_param, "Ritual");
        assert_eq!(by_slug("liquid-emotions").unwrap().type_param, "Delirium");
        assert_eq!(by_slug("unique-relics").unwrap().type_param, "UniqueSanctumRelics");
        assert_eq!(by_slug("breach-catalyst").unwrap().type_param, "Breach");
    }

    #[test]
    fn families_use_correct_paths() {
        assert_eq!(by_slug("currency").unwrap().family.path(), "exchange/current/overview");
        assert_eq!(by_slug("unique-weapons").unwrap().family.path(), "stash/current/item/overview");
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test categories::`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/poeninja/mod.rs src/poeninja/categories.rs src/main.rs
git commit -m "feat: poe.ninja category registry (23 categories)"
```

---

## Task 4: API models + normalization

**Files:**
- Create: `src/poeninja/model.rs`
- Create: `src/poeninja/fixtures/exchange_currency.json`
- Create: `src/poeninja/fixtures/item_uniqueweapons.json`
- Modify: `src/poeninja/mod.rs` (add `pub mod model;`)

- [ ] **Step 1: Create fixture `src/poeninja/fixtures/exchange_currency.json`**

```json
{
  "core": { "primary": "divine", "secondary": "exalted", "rates": { "exalted": 184.7, "chaos": 11.01 } },
  "lines": [
    { "id": "divine", "primaryValue": 1, "volumePrimaryValue": 97385, "sparkline": { "totalChange": 74.02 } },
    { "id": "exalted", "primaryValue": 0.005415, "volumePrimaryValue": 97385, "sparkline": { "totalChange": -42.65 } }
  ],
  "items": [
    { "id": "divine", "name": "Divine Orb", "image": "/gen/image/divine.png", "detailsId": "divine-orb" },
    { "id": "exalted", "name": "Exalted Orb", "image": "/gen/image/exalted.png", "detailsId": "exalted-orb" }
  ]
}
```

- [ ] **Step 2: Create fixture `src/poeninja/fixtures/item_uniqueweapons.json`**

```json
{
  "core": { "primary": "divine", "secondary": "exalted", "rates": { "exalted": 186.7, "chaos": 11.27 } },
  "lines": [
    {
      "id": 754,
      "name": "The Dancing Dervish",
      "baseType": "Scimitar",
      "detailsId": "the-dancing-dervish-scimitar",
      "icon": "https://web.poecdn.com/gen/image/dervish.png",
      "primaryValue": 5822,
      "listingCount": 2,
      "sparkLine": { "totalChange": 16.02 }
    }
  ]
}
```

- [ ] **Step 3: Add `pub mod model;` to `src/poeninja/mod.rs`**

```rust
pub mod categories;
pub mod model;
```

- [ ] **Step 4: Write `src/poeninja/model.rs` with tests**

```rust
use std::collections::HashMap;

use serde::Deserialize;

use super::categories::Category;

/// Normalized, Discord-ready representation of a priced item.
#[derive(Clone, Debug, PartialEq)]
pub struct PricedItem {
    pub name: String,
    pub base_type: Option<String>,
    pub category: String,
    pub slug: String,
    pub details_id: String,
    pub value_chaos: f64,
    pub value_exalted: f64,
    pub value_divine: f64,
    pub change_pct: f64,
    pub volume: f64,
    pub icon_url: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Sparkline {
    #[serde(rename = "totalChange", default)]
    pub total_change: f64,
}

#[derive(Debug, Deserialize)]
pub struct Core {
    #[serde(default = "default_primary")]
    pub primary: String,
    #[serde(default)]
    pub rates: HashMap<String, f64>,
}

fn default_primary() -> String {
    "divine".to_string()
}

// ---- Exchange family ----

#[derive(Debug, Deserialize)]
pub struct ExchangeOverview {
    pub core: Core,
    #[serde(default)]
    pub lines: Vec<ExchangeLine>,
    #[serde(default)]
    pub items: Vec<ExchangeItem>,
}

#[derive(Debug, Deserialize)]
pub struct ExchangeItem {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(rename = "detailsId", default)]
    pub details_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ExchangeLine {
    pub id: String,
    #[serde(rename = "primaryValue")]
    pub primary_value: f64,
    #[serde(rename = "volumePrimaryValue", default)]
    pub volume_primary_value: f64,
    #[serde(default)]
    pub sparkline: Sparkline,
}

// ---- Stash item family ----

#[derive(Debug, Deserialize)]
pub struct ItemOverview {
    pub core: Core,
    #[serde(default)]
    pub lines: Vec<ItemLine>,
}

#[derive(Debug, Deserialize)]
pub struct ItemLine {
    pub name: String,
    #[serde(rename = "baseType", default)]
    pub base_type: Option<String>,
    #[serde(rename = "detailsId", default)]
    pub details_id: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(rename = "primaryValue")]
    pub primary_value: f64,
    #[serde(rename = "listingCount", default)]
    pub listing_count: f64,
    #[serde(rename = "sparkLine", default)]
    pub spark_line: Sparkline,
}

// ---- Conversion ----

/// Returns (chaos, exalted, divine) values for a primary-denominated price.
fn convert(core: &Core, primary_value: f64) -> (f64, f64, f64) {
    let to = |target: &str| -> f64 {
        if core.primary == target {
            primary_value
        } else {
            primary_value * core.rates.get(target).copied().unwrap_or(0.0)
        }
    };
    (to("chaos"), to("exalted"), to("divine"))
}

fn absolute_icon(path: String) -> String {
    if path.starts_with("http") {
        path
    } else {
        format!("https://poe.ninja{path}")
    }
}

pub fn normalize_exchange(cat: &Category, ov: ExchangeOverview) -> Vec<PricedItem> {
    let meta: HashMap<&str, &ExchangeItem> =
        ov.items.iter().map(|i| (i.id.as_str(), i)).collect();
    ov.lines
        .iter()
        .filter_map(|line| {
            let item = meta.get(line.id.as_str())?;
            let (chaos, exalted, divine) = convert(&ov.core, line.primary_value);
            Some(PricedItem {
                name: item.name.clone(),
                base_type: None,
                category: cat.display.to_string(),
                slug: cat.slug.to_string(),
                details_id: item.details_id.clone(),
                value_chaos: chaos,
                value_exalted: exalted,
                value_divine: divine,
                change_pct: line.sparkline.total_change,
                volume: line.volume_primary_value,
                icon_url: item.image.clone().map(absolute_icon),
            })
        })
        .collect()
}

pub fn normalize_item(cat: &Category, ov: ItemOverview) -> Vec<PricedItem> {
    ov.lines
        .iter()
        .map(|line| {
            let (chaos, exalted, divine) = convert(&ov.core, line.primary_value);
            PricedItem {
                name: line.name.clone(),
                base_type: line.base_type.clone(),
                category: cat.display.to_string(),
                slug: cat.slug.to_string(),
                details_id: line.details_id.clone(),
                value_chaos: chaos,
                value_exalted: exalted,
                value_divine: divine,
                change_pct: line.spark_line.total_change,
                volume: line.listing_count,
                icon_url: line.icon.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poeninja::categories::by_slug;

    #[test]
    fn normalizes_exchange_with_join_and_conversion() {
        let ov: ExchangeOverview =
            serde_json::from_str(include_str!("fixtures/exchange_currency.json")).unwrap();
        let items = normalize_exchange(by_slug("currency").unwrap(), ov);
        assert_eq!(items.len(), 2);

        let divine = items.iter().find(|i| i.name == "Divine Orb").unwrap();
        assert_eq!(divine.value_divine, 1.0);
        assert!((divine.value_chaos - 11.01).abs() < 1e-6);
        assert!((divine.value_exalted - 184.7).abs() < 1e-6);
        assert_eq!(divine.change_pct, 74.02);
        assert_eq!(divine.category, "Currency");
        assert_eq!(divine.icon_url.as_deref(), Some("https://poe.ninja/gen/image/divine.png"));
    }

    #[test]
    fn normalizes_stash_item() {
        let ov: ItemOverview =
            serde_json::from_str(include_str!("fixtures/item_uniqueweapons.json")).unwrap();
        let items = normalize_item(by_slug("unique-weapons").unwrap(), ov);
        assert_eq!(items.len(), 1);

        let d = &items[0];
        assert_eq!(d.name, "The Dancing Dervish");
        assert_eq!(d.base_type.as_deref(), Some("Scimitar"));
        assert_eq!(d.value_divine, 5822.0);
        assert!((d.value_chaos - 5822.0 * 11.27).abs() < 1e-3);
        assert_eq!(d.volume, 2.0);
        assert_eq!(d.icon_url.as_deref(), Some("https://web.poecdn.com/gen/image/dervish.png"));
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test model::`
Expected: 2 tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/poeninja/model.rs src/poeninja/mod.rs src/poeninja/fixtures/
git commit -m "feat: API models and normalization for both endpoint families"
```

---

## Task 5: League selection + HTTP client

**Files:**
- Create: `src/poeninja/fixtures/index_state.json`
- Modify: `src/poeninja/mod.rs` (add `League`, `select_current_league`, `NinjaClient`)

- [ ] **Step 1: Create fixture `src/poeninja/fixtures/index_state.json`**

```json
{
  "economyLeagues": [
    { "name": "Runes of Aldur", "url": "runesofaldur", "hardcore": false, "indexed": true },
    { "name": "HC Runes of Aldur", "url": "runesofaldurhc", "hardcore": true, "indexed": true },
    { "name": "Standard", "url": "standard", "hardcore": false, "indexed": true },
    { "name": "Hardcore", "url": "hardcore", "hardcore": true, "indexed": true }
  ]
}
```

- [ ] **Step 2: Replace `src/poeninja/mod.rs` with the client + league selection and tests**

```rust
pub mod categories;
pub mod model;

use anyhow::{Context, Result};
use reqwest::Client;

use categories::{Category, Family, CATEGORIES};
use model::{normalize_exchange, normalize_item, ExchangeOverview, ItemOverview, PricedItem};

const BASE: &str = "https://poe.ninja/poe2/api";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct League {
    pub name: String,
    pub url: String,
}

/// Picks the active softcore challenge league: first indexed, non-hardcore
/// league that is not the permanent "Standard" league. Falls back to the first.
pub fn select_current_league(v: &serde_json::Value) -> Option<League> {
    let leagues = v.get("economyLeagues")?.as_array()?;
    let pick = leagues
        .iter()
        .find(|l| {
            let indexed = l.get("indexed").and_then(|x| x.as_bool()).unwrap_or(false);
            let hardcore = l.get("hardcore").and_then(|x| x.as_bool()).unwrap_or(false);
            let name = l.get("name").and_then(|x| x.as_str()).unwrap_or("");
            indexed && !hardcore && name != "Standard"
        })
        .or_else(|| leagues.first())?;
    Some(League {
        name: pick.get("name")?.as_str()?.to_string(),
        url: pick.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    })
}

pub struct NinjaClient {
    http: Client,
}

impl NinjaClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .user_agent("dr-peste-redux/0.1 (Discord guild price bot)")
            .build()?;
        Ok(Self { http })
    }

    pub async fn current_league(&self) -> Result<League> {
        let url = format!("{BASE}/data/index-state");
        let v: serde_json::Value = self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        select_current_league(&v).context("no current league found in index-state")
    }

    pub async fn fetch_category(&self, league: &str, cat: &Category) -> Result<Vec<PricedItem>> {
        let url = format!("{BASE}/economy/{}", cat.family.path());
        let resp = self
            .http
            .get(url)
            .query(&[("league", league), ("type", cat.type_param)])
            .send()
            .await?
            .error_for_status()?;
        match cat.family {
            Family::Exchange => {
                let ov: ExchangeOverview = resp.json().await?;
                Ok(normalize_exchange(cat, ov))
            }
            Family::StashItem => {
                let ov: ItemOverview = resp.json().await?;
                Ok(normalize_item(cat, ov))
            }
        }
    }

    /// Fetches every category sequentially (polite). A failing category is
    /// logged and skipped, never fatal.
    pub async fn fetch_all(&self, league: &str) -> Vec<PricedItem> {
        let mut all = Vec::new();
        for cat in CATEGORIES {
            match self.fetch_category(league, cat).await {
                Ok(mut items) => all.append(&mut items),
                Err(e) => tracing::warn!(category = cat.slug, error = %e, "failed to fetch category"),
            }
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_challenge_league_over_standard_and_hc() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/index_state.json")).unwrap();
        let league = select_current_league(&v).unwrap();
        assert_eq!(league.name, "Runes of Aldur");
        assert_eq!(league.url, "runesofaldur");
    }

    #[test]
    fn falls_back_to_first_when_none_match() {
        let v = serde_json::json!({
            "economyLeagues": [{ "name": "Standard", "url": "standard", "hardcore": false, "indexed": true }]
        });
        assert_eq!(select_current_league(&v).unwrap().name, "Standard");
    }

    #[test]
    fn returns_none_without_leagues() {
        let v = serde_json::json!({});
        assert!(select_current_league(&v).is_none());
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test poeninja::tests`
Expected: 3 tests pass.

- [ ] **Step 4: Verify the whole crate still builds**

Run: `cargo build`
Expected: `Finished`. (Unused-code warnings for `NinjaClient`/`fetch_all` are expected until wired up; that's fine.)

- [ ] **Step 5: Commit**

```bash
git add src/poeninja/mod.rs src/poeninja/fixtures/index_state.json
git commit -m "feat: league auto-detection and poe.ninja HTTP client"
```

---

## Task 6: Item-text parser

**Files:**
- Create: `src/itemtext.rs`
- Modify: `src/main.rs` (add `mod itemtext;`)

- [ ] **Step 1: Add `mod itemtext;` to `src/main.rs`**

```rust
mod config;
mod itemtext;
mod poeninja;

fn main() {
    println!("dr-peste-redux: scaffold");
}
```

- [ ] **Step 2: Write `src/itemtext.rs` with tests**

```rust
/// Rarity as reported by the in-game clipboard "Rarity:" line.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Rarity {
    Normal,
    Magic,
    Rare,
    Unique,
    Currency,
    Other(String),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ParsedItem {
    pub rarity: Rarity,
    pub name: String,
    pub base_type: Option<String>,
}

fn is_separator(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c == '-')
}

/// Parses the PoE2 clipboard format. Returns None if no "Rarity:" line or no
/// name line is present.
pub fn parse(text: &str) -> Option<ParsedItem> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let idx = lines.iter().position(|l| l.starts_with("Rarity:"))?;

    let rarity_str = lines[idx].trim_start_matches("Rarity:").trim();
    let rarity = match rarity_str {
        "Normal" => Rarity::Normal,
        "Magic" => Rarity::Magic,
        "Rare" => Rarity::Rare,
        "Unique" => Rarity::Unique,
        "Currency" => Rarity::Currency,
        other => Rarity::Other(other.to_string()),
    };

    let name = lines
        .get(idx + 1)
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty() && !is_separator(s))?;

    let base_type = lines
        .get(idx + 2)
        .filter(|s| !s.is_empty() && !is_separator(s))
        .map(|s| s.to_string());

    Some(ParsedItem { rarity, name, base_type })
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIQUE: &str = "Item Class: One Hand Swords\r\nRarity: Unique\r\nThe Dancing Dervish\r\nScimitar\r\n--------\r\nLevel: 16\r\n";
    const CURRENCY: &str = "Item Class: Stackable Currency\nRarity: Currency\nDivine Orb\n--------\nStack Size: 1/10\n";
    const RARE: &str = "Item Class: Body Armours\nRarity: Rare\nCorpse Bramble\nVaal Regalia\n--------\n";

    #[test]
    fn parses_unique_with_base() {
        let p = parse(UNIQUE).unwrap();
        assert_eq!(p.rarity, Rarity::Unique);
        assert_eq!(p.name, "The Dancing Dervish");
        assert_eq!(p.base_type.as_deref(), Some("Scimitar"));
    }

    #[test]
    fn parses_currency_without_base() {
        let p = parse(CURRENCY).unwrap();
        assert_eq!(p.rarity, Rarity::Currency);
        assert_eq!(p.name, "Divine Orb");
        assert_eq!(p.base_type, None);
    }

    #[test]
    fn parses_rare_name_and_base() {
        let p = parse(RARE).unwrap();
        assert_eq!(p.rarity, Rarity::Rare);
        assert_eq!(p.name, "Corpse Bramble");
        assert_eq!(p.base_type.as_deref(), Some("Vaal Regalia"));
    }

    #[test]
    fn returns_none_without_rarity_line() {
        assert!(parse("just some text\nnothing here").is_none());
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test itemtext::`
Expected: 4 tests pass.

> **Note for the implementer:** before relying on this in production, confirm the
> exact PoE2 clipboard text against a real copied item (Ctrl+C in-game). If the
> header lines differ, only the `parse` line-offset logic needs adjusting; the
> tests above encode the assumed format.

- [ ] **Step 4: Commit**

```bash
git add src/itemtext.rs src/main.rs
git commit -m "feat: PoE2 clipboard item-text parser"
```

---

## Task 7: PriceStore — cache, search, ranking, routing

**Files:**
- Create: `src/store.rs`
- Modify: `src/main.rs` (add `mod store;`)

- [ ] **Step 1: Add `mod store;` to `src/main.rs`**

```rust
mod config;
mod itemtext;
mod poeninja;
mod store;

fn main() {
    println!("dr-peste-redux: scaffold");
}
```

- [ ] **Step 2: Write `src/store.rs` with tests**

```rust
use std::sync::Arc;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use tokio::sync::RwLock;

use crate::itemtext::{ParsedItem, Rarity};
use crate::poeninja::model::PricedItem;
use crate::poeninja::League;

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub league: League,
    pub items: Vec<PricedItem>,
}

/// Thread-safe holder for the latest snapshot. `None` until the first refresh.
#[derive(Clone, Default)]
pub struct PriceStore {
    inner: Arc<RwLock<Option<Snapshot>>>,
}

impl PriceStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn replace(&self, snap: Snapshot) {
        *self.inner.write().await = Some(snap);
    }

    pub async fn snapshot(&self) -> Option<Snapshot> {
        self.inner.read().await.clone()
    }
}

pub fn find_exact<'a>(items: &'a [PricedItem], name: &str) -> Option<&'a PricedItem> {
    let q = name.trim().to_lowercase();
    items.iter().find(|it| it.name.to_lowercase() == q)
}

pub fn search<'a>(items: &'a [PricedItem], query: &str, limit: usize) -> Vec<&'a PricedItem> {
    let query = query.trim();
    if query.is_empty() {
        return items.iter().take(limit).collect();
    }
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, &PricedItem)> = items
        .iter()
        .filter_map(|it| matcher.fuzzy_match(&it.name, query).map(|s| (s, it)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.len().cmp(&b.1.name.len())));
    scored.into_iter().take(limit).map(|(_, it)| it).collect()
}

#[derive(Clone, Copy, Debug)]
pub enum FarmSort {
    Value,
    Trending,
}

pub fn farm<'a>(
    items: &'a [PricedItem],
    sort: FarmSort,
    min_volume: f64,
    slug: Option<&str>,
    limit: usize,
) -> Vec<&'a PricedItem> {
    let mut filtered: Vec<&PricedItem> = items
        .iter()
        .filter(|it| it.volume >= min_volume)
        .filter(|it| slug.map_or(true, |s| it.slug == s))
        .collect();
    let key = |it: &&PricedItem| match sort {
        FarmSort::Value => it.value_chaos,
        FarmSort::Trending => it.change_pct,
    };
    filtered.sort_by(|a, b| key(b).partial_cmp(&key(a)).unwrap_or(std::cmp::Ordering::Equal));
    filtered.into_iter().take(limit).collect()
}

#[derive(Debug)]
pub enum MatchOutcome<'a> {
    Found(&'a PricedItem),
    Suggestions(Vec<&'a PricedItem>),
    NotTracked,
    NotFound,
}

/// Routes a parsed pasted item to a price match. Magic/Rare gear is not priced
/// by poe.ninja and returns NotTracked.
pub fn route<'a>(items: &'a [PricedItem], parsed: &ParsedItem) -> MatchOutcome<'a> {
    if matches!(parsed.rarity, Rarity::Magic | Rarity::Rare) {
        return MatchOutcome::NotTracked;
    }
    if let Some(found) = find_exact(items, &parsed.name) {
        return MatchOutcome::Found(found);
    }
    let suggestions = search(items, &parsed.name, 3);
    if suggestions.is_empty() {
        MatchOutcome::NotFound
    } else {
        MatchOutcome::Suggestions(suggestions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, slug: &str, chaos: f64, change: f64, volume: f64) -> PricedItem {
        PricedItem {
            name: name.to_string(),
            base_type: None,
            category: slug.to_string(),
            slug: slug.to_string(),
            details_id: name.to_lowercase().replace(' ', "-"),
            value_chaos: chaos,
            value_exalted: chaos,
            value_divine: chaos / 100.0,
            change_pct: change,
            volume,
            icon_url: None,
        }
    }

    fn sample() -> Vec<PricedItem> {
        vec![
            item("Divine Orb", "currency", 11.0, 74.0, 1000.0),
            item("Exalted Orb", "currency", 0.06, -42.0, 5000.0),
            item("Mirror of Kalandra", "currency", 50000.0, 2.0, 1.0),
            item("The Dancing Dervish", "unique-weapons", 65000.0, 16.0, 2.0),
        ]
    }

    #[test]
    fn exact_match_is_case_insensitive() {
        let items = sample();
        assert_eq!(find_exact(&items, "divine orb").unwrap().name, "Divine Orb");
    }

    #[test]
    fn fuzzy_search_finds_typos() {
        let items = sample();
        let hits = search(&items, "dancing", 5);
        assert_eq!(hits[0].name, "The Dancing Dervish");
    }

    #[test]
    fn farm_by_value_respects_min_volume() {
        let items = sample();
        // Mirror (50000) has volume 1; with min_volume 10 it is filtered out.
        let top = farm(&items, FarmSort::Value, 10.0, None, 10);
        assert_eq!(top[0].name, "Divine Orb");
        assert!(top.iter().all(|i| i.name != "Mirror of Kalandra"));
    }

    #[test]
    fn farm_by_trending_sorts_by_change() {
        let items = sample();
        let top = farm(&items, FarmSort::Trending, 0.0, None, 2);
        assert_eq!(top[0].name, "Divine Orb"); // +74%
    }

    #[test]
    fn farm_filters_by_category_slug() {
        let items = sample();
        let top = farm(&items, FarmSort::Value, 0.0, Some("unique-weapons"), 10);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].name, "The Dancing Dervish");
    }

    #[test]
    fn route_rejects_rare_gear() {
        let items = sample();
        let parsed = ParsedItem { rarity: Rarity::Rare, name: "Corpse Bramble".into(), base_type: Some("Vaal Regalia".into()) };
        assert!(matches!(route(&items, &parsed), MatchOutcome::NotTracked));
    }

    #[test]
    fn route_finds_unique_by_name() {
        let items = sample();
        let parsed = ParsedItem { rarity: Rarity::Unique, name: "The Dancing Dervish".into(), base_type: Some("Scimitar".into()) };
        assert!(matches!(route(&items, &parsed), MatchOutcome::Found(_)));
    }

    #[test]
    fn route_suggests_when_no_exact_match() {
        let items = sample();
        let parsed = ParsedItem { rarity: Rarity::Currency, name: "Divine".into(), base_type: None };
        assert!(matches!(route(&items, &parsed), MatchOutcome::Suggestions(_)));
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test store::`
Expected: 8 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/store.rs src/main.rs
git commit -m "feat: PriceStore with fuzzy search, farm ranking, and paste routing"
```

---

## Task 8: Discord module skeleton + embeds

**Files:**
- Create: `src/discord/mod.rs`
- Create: `src/discord/embeds.rs`
- Modify: `src/main.rs` (add `mod discord;`)

- [ ] **Step 1: Add `mod discord;` to `src/main.rs`**

```rust
mod config;
mod discord;
mod itemtext;
mod poeninja;
mod store;

fn main() {
    println!("dr-peste-redux: scaffold");
}
```

- [ ] **Step 2: Create `src/discord/mod.rs`**

```rust
pub mod embeds;
pub mod farm;
pub mod price;
pub mod pricecheck;

use futures::Stream;

use crate::config::Config;
use crate::store::{self, PriceStore};

pub struct Data {
    pub store: PriceStore,
    pub config: Config,
}

pub type Error = anyhow::Error;
pub type Context<'a> = poise::Context<'a, Data, Error>;
pub type AppContext<'a> = poise::ApplicationContext<'a, Data, Error>;

/// Autocomplete callback shared by `/price`. Returns up to 25 item names that
/// fuzzy-match the partial input.
pub async fn autocomplete_item<'a>(
    ctx: Context<'a>,
    partial: &'a str,
) -> impl Stream<Item = String> + 'a {
    let names: Vec<String> = match ctx.data().store.snapshot().await {
        Some(snap) => store::search(&snap.items, partial, 25)
            .into_iter()
            .map(|it| it.name.clone())
            .collect(),
        None => Vec::new(),
    };
    futures::stream::iter(names)
}

/// Autocomplete callback for `/farm`'s category argument. Returns matching slugs.
pub async fn autocomplete_category<'a>(
    _ctx: Context<'a>,
    partial: &'a str,
) -> impl Stream<Item = String> + 'a {
    let partial = partial.to_lowercase();
    let slugs: Vec<String> = crate::poeninja::categories::CATEGORIES
        .iter()
        .filter(|c| c.slug.contains(&partial) || c.display.to_lowercase().contains(&partial))
        .map(|c| c.slug.to_string())
        .take(25)
        .collect();
    futures::stream::iter(slugs)
}
```

- [ ] **Step 3: Create `src/discord/embeds.rs` with tests**

```rust
use poise::serenity_prelude as serenity;

use crate::poeninja::model::PricedItem;
use crate::poeninja::League;

/// Picks a human-friendly value string: divine if ≥1 divine, else exalted if
/// ≥1 exalted, else chaos.
pub fn best_price_string(it: &PricedItem) -> String {
    if it.value_divine >= 1.0 {
        format!("{:.2} divine", it.value_divine)
    } else if it.value_exalted >= 1.0 {
        format!("{:.1} exalted", it.value_exalted)
    } else {
        format!("{:.1} chaos", it.value_chaos)
    }
}

pub fn trend_string(change: f64) -> String {
    let arrow = if change > 0.5 {
        "📈"
    } else if change < -0.5 {
        "📉"
    } else {
        "➡️"
    };
    format!("{arrow} {change:+.1}%")
}

fn ninja_url(it: &PricedItem, league: &League) -> String {
    format!(
        "https://poe.ninja/poe2/economy/{}/{}/{}",
        league.url, it.slug, it.details_id
    )
}

pub fn item_embed(it: &PricedItem, league: &League) -> serenity::CreateEmbed {
    let mut e = serenity::CreateEmbed::default()
        .title(&it.name)
        .url(ninja_url(it, league))
        .field("Value", best_price_string(it), true)
        .field("Trend", trend_string(it.change_pct), true)
        .field("Category", &it.category, true)
        .footer(serenity::CreateEmbedFooter::new(format!(
            "poe.ninja • {}",
            league.name
        )));
    if let Some(base) = &it.base_type {
        e = e.description(base);
    }
    if let Some(icon) = &it.icon_url {
        e = e.thumbnail(icon);
    }
    e
}

pub fn farm_embed(title: &str, items: &[&PricedItem], league: &League) -> serenity::CreateEmbed {
    let body = if items.is_empty() {
        "No items matched the current filter.".to_string()
    } else {
        items
            .iter()
            .enumerate()
            .map(|(i, it)| {
                format!(
                    "**{}. {}** — {} ({})",
                    i + 1,
                    it.name,
                    best_price_string(it),
                    trend_string(it.change_pct)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    serenity::CreateEmbed::default()
        .title(title)
        .description(body)
        .footer(serenity::CreateEmbedFooter::new(format!(
            "poe.ninja • {} • ranked from live data",
            league.name
        )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(divine: f64, exalted: f64, chaos: f64) -> PricedItem {
        PricedItem {
            name: "X".into(),
            base_type: None,
            category: "Currency".into(),
            slug: "currency".into(),
            details_id: "x".into(),
            value_chaos: chaos,
            value_exalted: exalted,
            value_divine: divine,
            change_pct: 0.0,
            volume: 0.0,
            icon_url: None,
        }
    }

    #[test]
    fn price_string_picks_largest_sensible_unit() {
        assert_eq!(best_price_string(&item(2.0, 200.0, 2000.0)), "2.00 divine");
        assert_eq!(best_price_string(&item(0.5, 90.0, 900.0)), "90.0 exalted");
        assert_eq!(best_price_string(&item(0.001, 0.2, 2.5)), "2.5 chaos");
    }

    #[test]
    fn trend_string_has_direction() {
        assert!(trend_string(5.0).contains("+5.0%"));
        assert!(trend_string(-5.0).contains("-5.0%"));
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test embeds::`
Expected: 2 tests pass. (The crate will NOT fully build yet because `discord/mod.rs` references `price`, `farm`, `pricecheck` modules created in later tasks. That is expected — run only this module's tests with `--lib`-style filtering is not possible in a bin crate, so instead verify with the next note.)

> **Build note:** `cargo test embeds::` compiles the whole binary, which needs
> the command modules. To keep this task self-contained, create empty stubs now
> and fill them in Tasks 9–11:
> - `src/discord/price.rs` containing `// filled in Task 9`
> - `src/discord/farm.rs` containing `// filled in Task 10`
> - `src/discord/pricecheck.rs` containing `// filled in Task 11`
>
> Then temporarily comment out the `pub mod price; pub mod farm; pub mod
> pricecheck;` lines in `src/discord/mod.rs` and the corresponding usages, OR
> proceed straight through Tasks 9–11 before running a full build. Recommended:
> proceed to Task 9 now and run the first full build at the end of Task 11.

- [ ] **Step 5: Commit**

```bash
git add src/discord/mod.rs src/discord/embeds.rs src/main.rs
git commit -m "feat: discord module skeleton, autocomplete, and embed formatting"
```

---

## Task 9: `/price` command

**Files:**
- Create: `src/discord/price.rs`

- [ ] **Step 1: Write `src/discord/price.rs`**

```rust
use super::{autocomplete_item, embeds, Context, Error};
use crate::store;

/// Look up the value of a tracked PoE2 item.
#[poise::command(slash_command)]
pub async fn price(
    ctx: Context<'_>,
    #[description = "Item name"]
    #[autocomplete = "autocomplete_item"]
    item: String,
) -> Result<(), Error> {
    let Some(snap) = ctx.data().store.snapshot().await else {
        ctx.say("Still warming up — try again in a few seconds.").await?;
        return Ok(());
    };

    if let Some(found) = store::find_exact(&snap.items, &item) {
        ctx.send(poise::CreateReply::default().embed(embeds::item_embed(found, &snap.league)))
            .await?;
        return Ok(());
    }

    let suggestions = store::search(&snap.items, &item, 3);
    if suggestions.is_empty() {
        ctx.say(format!("No match for **{item}** in {}.", snap.league.name))
            .await?;
    } else {
        let names = suggestions
            .iter()
            .map(|i| format!("• {}", i.name))
            .collect::<Vec<_>>()
            .join("\n");
        ctx.say(format!("No exact match for **{item}**. Did you mean:\n{names}"))
            .await?;
    }
    Ok(())
}
```

- [ ] **Step 2: Confirm it parses (full build happens at end of Task 11)**

No standalone test (Discord glue). Proceed to Task 10.

- [ ] **Step 3: Commit**

```bash
git add src/discord/price.rs
git commit -m "feat: /price slash command with autocomplete"
```

---

## Task 10: `/farm` command

**Files:**
- Create: `src/discord/farm.rs`

- [ ] **Step 1: Write `src/discord/farm.rs`**

```rust
use super::{autocomplete_category, embeds, Context, Error};
use crate::poeninja::categories::by_slug;
use crate::store::{self, FarmSort};

#[derive(Debug, poise::ChoiceParameter)]
pub enum SortChoice {
    #[name = "Value"]
    Value,
    #[name = "Trending"]
    Trending,
}

/// Show the most valuable or fastest-rising items to farm right now.
#[poise::command(slash_command)]
pub async fn farm(
    ctx: Context<'_>,
    #[description = "Sort by value (default) or trending"] sort: Option<SortChoice>,
    #[description = "Restrict to one category slug (optional)"]
    #[autocomplete = "autocomplete_category"]
    category: Option<String>,
) -> Result<(), Error> {
    let Some(snap) = ctx.data().store.snapshot().await else {
        ctx.say("Still warming up — try again in a few seconds.").await?;
        return Ok(());
    };

    if let Some(slug) = &category {
        if by_slug(slug).is_none() {
            ctx.say(format!("Unknown category `{slug}`. Try autocomplete.")).await?;
            return Ok(());
        }
    }

    let sort = match sort {
        Some(SortChoice::Trending) => FarmSort::Trending,
        _ => FarmSort::Value,
    };
    let min_volume = ctx.data().config.min_volume;
    let top = store::farm(&snap.items, sort, min_volume, category.as_deref(), 10);

    let title = match sort {
        FarmSort::Value => "💰 Most valuable right now",
        FarmSort::Trending => "📈 Heating up right now",
    };
    ctx.send(poise::CreateReply::default().embed(embeds::farm_embed(title, &top, &snap.league)))
        .await?;
    Ok(())
}
```

- [ ] **Step 2: Commit**

```bash
git add src/discord/farm.rs
git commit -m "feat: /farm slash command (value/trending ranking)"
```

---

## Task 11: `/pricecheck` modal command

**Files:**
- Create: `src/discord/pricecheck.rs`

- [ ] **Step 1: Write `src/discord/pricecheck.rs`**

```rust
use super::{embeds, AppContext, Context, Error};
use crate::store::{self, MatchOutcome};
use crate::{itemtext, poeninja::League};

#[derive(Debug, poise::Modal)]
#[name = "Price Check"]
struct PriceCheckModal {
    #[name = "Paste your item"]
    #[placeholder = "Ctrl+C an item in-game, then paste it here"]
    #[paragraph]
    item_text: String,
}

/// Paste a copied in-game item to price it.
#[poise::command(slash_command)]
pub async fn pricecheck(app_ctx: AppContext<'_>) -> Result<(), Error> {
    use poise::Modal as _;

    let Some(modal) = PriceCheckModal::execute(app_ctx).await? else {
        return Ok(());
    };
    let ctx = Context::Application(app_ctx);

    let Some(parsed) = itemtext::parse(&modal.item_text) else {
        ctx.say("Couldn't read that — paste the full item text copied with Ctrl+C.")
            .await?;
        return Ok(());
    };

    let Some(snap) = ctx.data().store.snapshot().await else {
        ctx.say("Still warming up — try again in a few seconds.").await?;
        return Ok(());
    };

    match store::route(&snap.items, &parsed) {
        MatchOutcome::Found(it) => {
            ctx.send(poise::CreateReply::default().embed(embeds::item_embed(it, &snap.league)))
                .await?;
        }
        MatchOutcome::Suggestions(s) => {
            let names = s.iter().map(|i| format!("• {}", i.name)).collect::<Vec<_>>().join("\n");
            ctx.say(format!("No exact match for **{}**. Did you mean:\n{names}", parsed.name))
                .await?;
        }
        MatchOutcome::NotTracked => {
            ctx.say("That looks like rare/magic gear, which poe.ninja doesn't price. Try a unique or currency item.")
                .await?;
        }
        MatchOutcome::NotFound => {
            ctx.say(format_not_found(&parsed.name, &snap.league)).await?;
        }
    }
    Ok(())
}

fn format_not_found(name: &str, league: &League) -> String {
    format!("Couldn't find **{name}** in {} data.", league.name)
}
```

- [ ] **Step 2: Full crate build (first time all modules exist)**

Run: `cargo build`
Expected: `Finished`. Fix any compile errors before continuing. Common adjustments: `poise::Context::Application` variant name, and confirming `poise::Modal` derive is available (it is, in poise 0.6).

- [ ] **Step 3: Run the entire test suite**

Run: `cargo test`
Expected: all unit tests from Tasks 2–8 pass (config, categories, model, poeninja, itemtext, store, embeds).

- [ ] **Step 4: Commit**

```bash
git add src/discord/pricecheck.rs
git commit -m "feat: /pricecheck modal command for pasted items"
```

---

## Task 12: Wire up `main.rs` (refresher + bot)

**Files:**
- Modify: `src/main.rs` (full rewrite)

- [ ] **Step 1: Replace `src/main.rs` entirely**

```rust
mod config;
mod discord;
mod itemtext;
mod poeninja;
mod store;

use std::time::Duration;

use anyhow::Result;
use poise::serenity_prelude as serenity;
use tracing_subscriber::EnvFilter;

use discord::Data;
use poeninja::NinjaClient;
use store::{PriceStore, Snapshot};

async fn refresh_once(client: &NinjaClient, store: &PriceStore) -> Result<()> {
    let league = client.current_league().await?;
    let items = client.fetch_all(&league.name).await;
    tracing::info!(league = %league.name, count = items.len(), "snapshot refreshed");
    store.replace(Snapshot { league, items }).await;
    Ok(())
}

fn spawn_refresher(client: NinjaClient, store: PriceStore, interval: Duration) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = refresh_once(&client, &store).await {
                tracing::error!(error = %e, "refresh failed; keeping last snapshot");
            }
            tokio::time::sleep(interval).await;
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let config = config::Config::from_env()?;
    let store = PriceStore::new();
    let client = NinjaClient::new()?;

    // Best-effort initial refresh so commands have data quickly.
    if let Err(e) = refresh_once(&client, &store).await {
        tracing::warn!(error = %e, "initial refresh failed; will retry in background");
    }

    let interval = Duration::from_secs(config.poll_interval_mins * 60);
    spawn_refresher(client, store.clone(), interval);

    let token = config.discord_token.clone();
    let guild_id = serenity::GuildId::new(config.guild_id);
    let intents = serenity::GatewayIntents::non_privileged();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
                discord::price::price(),
                discord::farm::farm(),
                discord::pricecheck::pricecheck(),
            ],
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id)
                    .await?;
                tracing::info!("commands registered; bot ready");
                Ok(Data { store, config })
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await?;
    client.start().await?;
    Ok(())
}
```

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: `Finished`, no errors. Warnings about `by_slug`/`autocomplete_category` should be gone now that they are used.

- [ ] **Step 3: Run clippy and fmt**

Run: `cargo clippy --all-targets -- -D warnings` then `cargo fmt`
Expected: clippy clean (fix any lints), fmt makes no further changes after a second run.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire refresher task and poise bot startup"
```

---

## Task 13: Live smoke test (ignored by default)

**Files:**
- Modify: `src/poeninja/mod.rs` (add an `#[ignore]` integration test in the existing test module)

- [ ] **Step 1: Append a live test to the `tests` module in `src/poeninja/mod.rs`**

Add inside the existing `#[cfg(test)] mod tests { ... }` block:

```rust
    #[tokio::test]
    #[ignore = "hits the live poe.ninja API"]
    async fn live_fetch_currency_has_divine() {
        let client = NinjaClient::new().unwrap();
        let league = client.current_league().await.unwrap();
        let cat = categories::by_slug("currency").unwrap();
        let items = client.fetch_category(&league.name, cat).await.unwrap();
        assert!(items.iter().any(|i| i.name == "Divine Orb"), "expected Divine Orb in currency");
    }
```

- [ ] **Step 2: Run the live test explicitly**

Run: `cargo test --package dr-peste-redux poeninja::tests::live_fetch_currency_has_divine -- --ignored`
Expected: PASS (requires internet). If the schema has drifted, this is where it surfaces.

- [ ] **Step 3: Confirm the default suite still skips it**

Run: `cargo test`
Expected: the live test shows as `ignored`; all others pass.

- [ ] **Step 4: Commit**

```bash
git add src/poeninja/mod.rs
git commit -m "test: live poe.ninja smoke test (ignored by default)"
```

---

## Task 14: Dockerfile + README pointers

**Files:**
- Create: `Dockerfile`
- Create: `.dockerignore`

- [ ] **Step 1: Create `Dockerfile`**

```dockerfile
FROM rust:1.87-slim AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/dr-peste-redux /usr/local/bin/dr-peste-redux
ENTRYPOINT ["/usr/local/bin/dr-peste-redux"]
```

- [ ] **Step 2: Create `.dockerignore`**

```
/target
.git
.env
.env.*
!.env.example
docs
.playwright-mcp
.remember
```

- [ ] **Step 3: Build the image to verify it compiles in-container**

Run: `docker build -t dr-peste-redux .`
Expected: image builds successfully. (Skip if Docker is unavailable; note it in the handoff.)

- [ ] **Step 4: Commit**

```bash
git add Dockerfile .dockerignore
git commit -m "chore: multi-stage Dockerfile for self-hosting"
```

---

## Task 15: Manual end-to-end verification

This cannot be unit-tested. Perform once against a real Discord bot.

- [ ] **Step 1:** Create a Discord application + bot, copy the token. Invite it to your guild with the `applications.commands` and `bot` scopes.
- [ ] **Step 2:** Copy `.env.example` to `.env`, fill in `DISCORD_TOKEN` and `GUILD_ID`. Confirm `.env` is gitignored (`git check-ignore .env` prints `.env`).
- [ ] **Step 3:** Run `cargo run`. Confirm logs show "snapshot refreshed" with a non-zero count and "commands registered; bot ready".
- [ ] **Step 4:** In the guild, run `/price item:Divine` — confirm autocomplete suggests "Divine Orb" and the embed shows a value, trend, and a working poe.ninja link.
- [ ] **Step 5:** Run `/farm` and `/farm sort:Trending` — confirm a ranked top-10 embed.
- [ ] **Step 6:** Run `/pricecheck`, paste a real copied unique item, submit — confirm it matches and prices it. Paste a rare item — confirm the "not tracked" reply.
- [ ] **Step 7:** Note any format mismatches (especially the clipboard parser in `itemtext.rs`) and fix, with a regression test added to the relevant `#[cfg(test)]` module.

---

## Self-review notes (author)

- **Spec coverage:** §3 data source → Tasks 4–5; §3.3 registry → Task 3; §4 architecture/refresher/store → Tasks 7, 12; §5 commands → Tasks 9–11; §6 parser/routing → Tasks 6–7; §7 error handling → `fetch_all` skip-on-error + cold-start replies (Tasks 5, 9–12); §8 config/deploy → Tasks 2, 14; §9 module layout → matched; §10 testing → unit tests per task + Task 13. All covered.
- **Type consistency:** `PricedItem`, `Core`, `Sparkline`, `League`, `Snapshot`, `FarmSort`, `MatchOutcome`, `Rarity`, `ParsedItem` are defined once and referenced with the same field/variant names throughout. `search`/`find_exact`/`farm`/`route` signatures are stable across call sites.
- **Known soft spots flagged for the implementer:** exact PoE2 clipboard format (Task 6 note), and the `poise::Context::Application` / modal API surface (Task 11 Step 2) — both to be confirmed at first full build / manual test.
```
