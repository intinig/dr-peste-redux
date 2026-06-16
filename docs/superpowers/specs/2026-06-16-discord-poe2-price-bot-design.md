# Design: PoE2 Guild Price-Check Discord Bot

- **Date:** 2026-06-16
- **Status:** Approved (design); pending implementation plan
- **Language / runtime:** Rust (stable 1.87), async on `tokio`
- **Discord framework:** `poise` (on top of `serenity`)

## 1. Purpose

A single-guild, self-hosted Discord bot for a Path of Exile 2 guild that lets
members:

1. **Price-check** any tracked PoE2 item via name (with autocomplete).
2. **Price-check by pasting** a copied in-game item (modal popup).
3. Ask **what's best to farm**, answered honestly from poe.ninja price + trend
   data (most valuable / biggest movers).

All prices come from poe.ninja's public PoE2 economy API. The bot follows the
current challenge league automatically.

## 2. Scope

### In scope

- Three command surfaces: `/price`, `/pricecheck`, `/farm`.
- All 23 poe.ninja economy categories for the current league.
- Auto-detection of the active challenge league.
- In-memory cache refreshed by a background poller.
- Docker-based deployment for self-hosting.

### Non-goals (explicitly out)

- Rare/magic gear pricing from mods (poe.ninja does not price arbitrary rares).
  Pasted rares get a clear "not tracked" reply.
- True profit-per-hour or drop-rate modelling. "Best to farm" is value + trend
  only, and says so.
- Multi-guild scaling, a database, web dashboard, ladder/build features, trade
  API integration.

## 3. Data source (verified against the live API)

Base: `https://poe.ninja/poe2/api`. No authentication. Cloudflare-cached;
updates roughly hourly. Server-side requests have no CORS restriction.

### 3.1 League detection — `GET /data/index-state`

Returns `economyLeagues[]` with `{ name, url, displayName, hardcore, indexed }`.
The bot selects the active **softcore challenge** league (non-hardcore, indexed,
not a permanent/Standard league). Falls back to the last known league on
failure.

### 3.2 Two category endpoint families

Both return `core` (metadata + conversion rates) and `lines` (per-item data).
Every priced line carries a current value and a trend.

**Exchange family** — bulk/stackable items:
`GET /economy/exchange/current/overview?league=<name>&type=<Type>`

- `core.primary` / `core.secondary`: denomination currencies (e.g. `divine`,
  `exalted`).
- `core.rates`: map of currency id → rate (e.g. `{ "exalted": 184.7, "chaos": 11.01 }`),
  used to convert `primaryValue` into chaos/exalted/divine.
- `items[]`: `{ id, name, image, category, detailsId }` (metadata).
- `lines[]`: `{ id, primaryValue, volumePrimaryValue, maxVolumeCurrency,
  maxVolumeRate, sparkline: { totalChange, data[] } }`.
- Items and lines are joined on `id`.

**Stash item family** — individually-priced items (uniques, etc.):
`GET /economy/stash/current/item/overview?league=<name>&type=<Type>`

- `core`: conversion rates (same role as above).
- `lines[]`: self-contained, e.g. `{ id, itemId, detailsId, name, baseType, icon,
  levelRequired, category, primaryValue, listingCount, corrupted,
  sparkLine: { totalChange, data[] }, explicitModifiers, ... }`.
- Note the field-name differences vs exchange: `sparkLine` (capital L) and
  `listingCount` (instead of `volumePrimaryValue`).

### 3.3 Category registry (23 categories)

A static table maps each poe.ninja slug → `(endpoint family, type param, display
name)`. Known slugs:

- **Exchange-style:** currency, fragments, abyssal-bones, uncut-gems,
  lineage-support-gems, essences, soul-cores, idols, runes, omens, expedition,
  liquid-emotions, breach-catalyst, verisium, precursor-tablets.
- **Stash-item-style:** unique-weapons, unique-armours, unique-accessories,
  unique-flasks, unique-charms, unique-jewels, unique-relics, unique-tablets.

(The exact family + `type` value per slug is finalized during implementation by
confirming each endpoint; e.g. `unique-weapons` → stash item, `type=UniqueWeapons`.)

## 4. Architecture

One long-running `tokio` process. Three isolated units communicating through a
shared in-memory store.

```
Discord  ──gateway──▶  poise handlers ──┐
                                        ├─▶  PriceStore (Arc<RwLock<Snapshot>>)
poe.ninja  ◀──polls──  Refresher task ──┘
```

- **`poeninja`** — fetches and normalizes data. Knows the two endpoint families
  and the category registry. Has no Discord knowledge.
- **`store` / `PriceStore`** — owns the latest `Snapshot` (all normalized items +
  league info). Provides read queries: fuzzy name search, farm ranking. Writers
  swap a fresh snapshot under a write lock; readers take a read lock.
- **`discord`** — command handlers, autocomplete, modal, embed formatting. Reads
  from `PriceStore`; never calls poe.ninja directly.
- **Refresher task** — polls every `POLL_INTERVAL_MINS` (default 30), rebuilds the
  snapshot, and atomically swaps it in. On failure, keeps serving the last good
  snapshot.

### 4.1 Normalized model

Both API shapes collapse into one internal type:

```
PricedItem {
  name: String,
  base_type: Option<String>,
  category: String,          // display name
  slug: String,              // poe.ninja category slug, for building links
  details_id: String,        // for the item's poe.ninja URL
  value_divine: f64,
  value_exalted: f64,
  value_chaos: f64,
  change_pct: f64,           // sparkline.totalChange
  volume: f64,               // volumePrimaryValue or listingCount
  icon_url: Option<String>,
}
```

Conversion to chaos/exalted/divine happens once at ingest using each category's
`core.rates`.

## 5. Commands

### 5.1 `/price item:<autocomplete>`

- Autocomplete suggests cached item names (fuzzy, typo-tolerant) as the user
  types.
- Replies with an embed: icon, name, value in the most sensible currency (divine
  if ≥1 divine, else exalted, else chaos), the trend (`totalChange` % + the
  7-point sparkline rendered compactly), trade volume, and a link to the item's
  poe.ninja page.
- Ambiguous match → show top 3 candidates.

### 5.2 `/pricecheck` (modal)

- Opens a modal with one multi-line paragraph input.
- User pastes a copied in-game item; on submit the bot parses it (see §6),
  matches against the cache, and replies with the same embed as `/price`.

### 5.3 `/farm [category:optional] [sort:value|trending]`

- Replies with an embed listing the top ~10 items.
- `sort=value` (default): ranked by chaos-equivalent value.
- `sort=trending`: ranked by `totalChange`.
- A minimum-volume / listing filter (`MIN_VOLUME`) removes illiquid, noise-priced
  items so they don't dominate.
- Optional `category` narrows to one slug.
- The embed states the basis plainly ("ranked by current value / trend on
  poe.ninja") and never implies drop-rate or profit/hour knowledge.

## 6. Item-text parser (`itemtext.rs`)

Parses the PoE2 clipboard format (the `Item Class:` / `Rarity:` header block,
sections separated by lines of `--------`). Extracts **rarity**, **name**, and
**base type**.

Match routing by rarity:

- **Unique** → match the unique name against cached unique categories → price
  embed.
- **Currency / Fragment / other exchange items** → match by name → price embed.
- **Normal / Magic / Rare gear** → reply "rare/magic gear isn't tracked by
  poe.ninja" (consistent with non-goals).
- **No match** → "couldn't find this item in the current league data" + closest
  suggestions.

> Validation caveat: the exact PoE2 clipboard format will be confirmed against a
> real copied-item sample during implementation before finalizing the parser.

All three surfaces share the same matcher and `embeds.rs` formatting.

## 7. Error handling & resilience

- poe.ninja fetch failure: refresher logs and keeps the last good snapshot; never
  crashes the bot. A single category that fails to fetch/parse is skipped, not
  fatal.
- Cold start (no snapshot yet): commands reply "still warming up, try again in a
  few seconds."
- League auto-detection failure: fall back to last known league.
- Discord/network errors: logged via `tracing`; serenity auto-reconnects the
  gateway.

## 8. Configuration & deployment

Config via environment (`.env` locally via `dotenvy`, real env in production):

- `DISCORD_TOKEN` — bot token. **Secret; `.env` is gitignored, never committed.**
- `GUILD_ID` — for instant guild-scoped slash command registration.
- `POLL_INTERVAL_MINS` — default 30.
- `MIN_VOLUME` — farm-list liquidity threshold.

Ships with a multi-stage `Dockerfile` for self-hosting on a VPS, home server, or
container host.

## 9. Project structure

```
src/
  main.rs            // load config, build store, spawn refresher, start bot
  config.rs
  itemtext.rs        // PoE2 clipboard parser
  poeninja/
    mod.rs           // client + fetch-all-categories
    categories.rs    // the 23-entry registry
    model.rs         // raw API structs (exchange + stash item) + normalization
  store.rs           // PriceStore: cache + fuzzy search + farm ranking
  discord/
    mod.rs           // bot wiring, autocomplete
    price.rs         // /price
    pricecheck.rs    // /pricecheck modal + submit
    farm.rs          // /farm
    embeds.rs        // shared embed formatting
```

## 10. Testing

- **Unit (offline, against committed JSON fixtures):**
  - Normalization for both endpoint families → expected `PricedItem`.
  - Currency conversion math (rates → chaos/exalted/divine).
  - Fuzzy search ranking.
  - Farm filtering/sorting.
  - Item-text parser: unique / currency / rare samples → expected parse + routing.
- **Integration (feature-gated / ignored by default):** one live-API smoke test
  to catch schema drift in the two endpoint families and `index-state`.

## 11. Dependencies (anticipated)

- `poise` + `serenity` — Discord.
- `tokio` — async runtime.
- `reqwest` (JSON, rustls) — HTTP client for poe.ninja.
- `serde` / `serde_json` — API deserialization.
- `tracing` / `tracing-subscriber` — logging.
- `dotenvy` — local env loading.
- A fuzzy-matching crate (e.g. `fuzzy-matcher`) for name search/autocomplete.
- `anyhow` / `thiserror` — error handling.

## 12. Open items for implementation

- Confirm endpoint family + `type` value for each of the 23 slugs.
- Confirm the exact PoE2 clipboard text format with a real sample.
- Decide the compact sparkline rendering in embeds (e.g. unicode blocks vs. just
  the % change).
