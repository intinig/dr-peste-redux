# dr-peste-redux

A self-hosted Discord bot for a **Path of Exile 2** guild. It price-checks items
and tells you what's worth farming, using [poe.ninja](https://poe.ninja)'s PoE2
economy data.

## Commands

- **`/price item:<name>`** — look up an item's value, with live autocomplete.
- **`/paste`** — opens a popup; paste a copied in-game item (Ctrl+C) and it
  matches and prices it. Rare/magic gear (which poe.ninja doesn't price) gets a
  clear "not tracked" reply.
- **`/help`** — lists the available commands.
- **`/farm [category] [sort:value|trending]`** — the most valuable items, or the
  biggest price movers, ranked from live data with a minimum-volume filter.

## How it works

One long-running process:

- A background **refresher** polls poe.ninja every `POLL_INTERVAL_MINS` and stores
  a normalized snapshot of all 23 economy categories in memory.
- The active challenge **league is auto-detected** from poe.ninja's `index-state`,
  so the bot follows each new league with no config change.
- Command handlers read the in-memory snapshot — they never call poe.ninja
  directly. **No database**; a restart just re-polls.

Prices are converted to chaos/exalted/divine from poe.ninja's rates, and trends
come from its sparkline data. The bot never invents profit-per-hour or drop-rate
numbers — `/farm` is explicitly "most valuable / heating up right now."

See `docs/superpowers/specs/` and `docs/superpowers/plans/` for the full design
and implementation plan, and `CLAUDE.md` for contributor guidance.

## Setup

Requirements: Rust 1.87+ (stable) and a Discord bot.

1. Create a Discord application + bot at <https://discord.com/developers/applications>,
   copy the bot token, and invite it to your guild with the `bot` and
   `applications.commands` scopes.
2. Copy the example env file and fill it in:
   ```bash
   cp .env.example .env
   # edit .env: set DISCORD_TOKEN and GUILD_ID
   ```
3. Run it:
   ```bash
   cargo run
   ```
   On startup you should see a "snapshot refreshed" log and "commands registered;
   bot ready". Slash commands are registered to your guild instantly.

### Configuration

All config is via environment variables (loaded from `.env` locally):

| Variable | Required | Default | Purpose |
|---|---|---|---|
| `DISCORD_TOKEN` | yes | — | Bot token. **Secret — never commit it.** |
| `GUILD_ID` | yes | — | Your server ID, for instant command registration. |
| `POLL_INTERVAL_MINS` | no | `30` | How often to refresh poe.ninja data. |
| `MIN_VOLUME` | no | `0` | Minimum trade volume / listing count for `/farm`. |

`.env` is gitignored; only `.env.example` is tracked.

## Docker

```bash
docker build -t dr-peste-redux .
docker run --rm --env-file .env dr-peste-redux
```

The release build of `serenity` needs a few GB of RAM; if you build inside a small
VM (e.g. Colima), give it 4 GB+ (`colima start --memory 4`).

## Development

```bash
cargo test          # unit tests (offline, against committed fixtures)
cargo clippy --all-targets -- -D warnings
cargo fmt
cargo test -- --ignored   # also runs the live poe.ninja smoke test (needs internet)
```

## License

Personal/guild use. Not affiliated with Grinding Gear Games or poe.ninja.
