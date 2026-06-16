# CLAUDE.md

Project guidance for working in this repository.

## What this is

A single-guild, self-hosted **Discord bot for a Path of Exile 2 guild**, written
in **Rust**. It price-checks PoE2 items and answers "what's best to farm" using
data from poe.ninja's public PoE2 economy API.

Full design: `docs/superpowers/specs/2026-06-16-discord-poe2-price-bot-design.md`.
Read it before making architectural changes.

## Command surfaces

- `/price item:<autocomplete>` — look up an item's value by name.
- `/pricecheck` — opens a modal; user pastes a copied in-game item, bot parses
  and matches it.
- `/farm [category] [sort:value|trending]` — most valuable / biggest movers.

## Architecture

One long-running `tokio` process, three isolated units sharing an in-memory store:

- `poeninja/` — fetches + normalizes poe.ninja data. No Discord knowledge.
- `store.rs` (`PriceStore`) — owns the latest `Snapshot` behind
  `Arc<RwLock<…>>`; provides fuzzy search + farm ranking. No I/O.
- `discord/` — command handlers, autocomplete, modal, embeds. Reads the store
  only; never calls poe.ninja directly.
- A background **refresher task** polls poe.ninja every `POLL_INTERVAL_MINS` and
  atomically swaps in a fresh snapshot. On failure it keeps the last good one.

**No database.** State is in-memory; a restart just re-polls.

### Module layout

```
src/
  main.rs            config.rs            itemtext.rs (PoE2 clipboard parser)
  poeninja/          mod.rs  categories.rs  model.rs
  store.rs
  discord/           mod.rs  price.rs  pricecheck.rs  farm.rs  embeds.rs
```

## poe.ninja API notes (load-bearing details)

Base `https://poe.ninja/poe2/api`. No auth. Cloudflare-cached, updates ~hourly.
Server-side requests have no CORS limit (browser cross-origin is blocked — N/A here).

- **League detection:** `GET /data/index-state` → `economyLeagues[]`
  (`name, url, hardcore, indexed`). Pick the active softcore challenge league.
- **Two endpoint families**, both returning `core` (+ `core.rates` for currency
  conversion) and `lines[]`:
  - Exchange (currency-style): `/economy/exchange/current/overview?league=&type=`
    — line has `primaryValue`, `volumePrimaryValue`, `sparkline` (lowercase l).
  - Stash item (uniques, etc.): `/economy/stash/current/item/overview?league=&type=`
    — line has `primaryValue`, `listingCount`, `sparkLine` (capital L), `baseType`.
- **Watch the field-name drift:** `sparkline` vs `sparkLine`,
  `volumePrimaryValue` vs `listingCount`. Both normalize into one `PricedItem`.
- Convert `primaryValue` to chaos/exalted/divine once at ingest using `core.rates`.
- League name in the query is space-separated and URL-encoded (e.g.
  `Runes+of+Aldur`).

## Commands

```bash
cargo build              # build
cargo test               # unit tests (offline, against committed JSON fixtures)
cargo test -- --ignored  # include the live-API smoke test (hits poe.ninja)
cargo run                # run the bot (needs env vars / .env)
cargo fmt                # format
cargo clippy             # lint
```

## Configuration

Set via environment (or a local `.env`, loaded by `dotenvy`):

- `DISCORD_TOKEN` — bot token. **Secret.**
- `GUILD_ID` — guild for instant slash-command registration.
- `POLL_INTERVAL_MINS` — refresh interval (default 30).
- `MIN_VOLUME` — liquidity threshold for `/farm`.

Keep a committed `.env.example` documenting the keys with placeholder values.

## Conventions

- Async throughout (`tokio`); the store is the only shared mutable state.
- Keep `poeninja`, `store`, and `discord` decoupled — data flows
  poe.ninja → store → discord, never sideways.
- Errors: `anyhow` at boundaries, `thiserror` for typed module errors; log with
  `tracing`. The refresher must never panic the process on a fetch/parse error.
- Tests are offline by default; anything hitting the network is `#[ignore]`d.

## Hard rules

- **NEVER commit secrets.** `.env` is gitignored; only `.env.example` is tracked.
  Verify before every commit.
- Be polite to poe.ninja: cache aggressively, poll infrequently, no tight loops.
- Only commit when asked. Stage files by name, never `git add -A`.
