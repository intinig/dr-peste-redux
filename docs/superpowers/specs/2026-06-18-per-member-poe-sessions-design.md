# Per-Member PoE Sessions + Residential Proxy — Design

**Date:** 2026-06-18
**Status:** Approved in brainstorming; pending spec review before planning.
**Supersedes:** the deferred "Provider B / per-member sessions" sketch in
`2026-06-17-rare-item-pricing-stage1-design.md` (§ SessionProvider).

## 1. Problem & motivation

Rare-item pricing (`/paste` on a rare/magic item) calls the official PoE2
`trade2` search/fetch API live. Today every call shares **one** session — the
optional operator `POESESSID`, or fully anonymous when unset. The anonymous
tier is throttled hard and the guild hits it within a few searches, after which
pricing breaks.

The goal: route **each member's** searches through **that member's own
POESESSID** and **their own sticky residential IP**, so the per-account and
per-IP rate-limit budgets are each member's own rather than one shared bucket.

### Research findings (2026-06-18) that shaped this design

- **OAuth is a dead end.** GGG's OAuth scopes (`service:psapi`, `service:cxapi`,
  `account:*`, …) do **not** cover the trade API; GGG staff confirmed the trade
  endpoints "remain available without authentication for now"
  (forum/view-thread/3293355). Trade limits are IP-keyed, so OAuth would not
  raise them. The maintained PoE2 tool (Exiled-Exchange-2) still uses a session
  cookie, not OAuth.
- **Rate limits are per-IP *and* per-account simultaneously** (forum/view-thread/
  2079853). With one shared box IP, per-member POESESSIDs *alone* raise
  throughput only to the doubled IP ceiling (~2×, capped, non-scaling). The
  thing that actually breaks the per-IP ceiling is **distinct egress IPs** — a
  residential proxy.
- **Cloudflare:** `POST /api/trade2/search` is challenged (403) from datacenter
  IPs in general. Our box currently gets through (it returns prices; breakage is
  429s), but a residential IP is the robust path and removes this fragility.
- **ToU framing:** a member's POESESSID is used **only** for that member's own
  searches, never cross-user — functionally what a browser trade extension does.
  The residual concerns are custody, the evasion-shaped proxy, and a privacy
  disclosure (see §2), not "account sharing."

## 2. Accepted risks (explicitly owned by the operator)

1. **Server-side credential custody.** Unlike a browser extension (session lives
   on the user's machine), the bot holds *N* live sessions on one box → a breach
   is a mass takeover. Mitigated by §7 (in-memory only, zeroize, never-log, TTL,
   revocation).
2. **Proxy egress is "evasion-shaped."** Replaying a session from a rented
   residential IP is a stronger anti-abuse signal than an extension on the user's
   home IP. Mitigated by: honest contact-bearing User-Agent, strict
   `Retry-After` backoff, **never retry through a 429**, aggressive caching,
   snapshot-first (live trade2 only for genuinely rare items — already the case).
3. **Privacy Policy must change.** The bot now holds member credentials
   (transiently). The drp.pme.it "no database / stores no personal data" line is
   updated (§11).

## 3. Non-goals (YAGNI)

Disk persistence; in-memory encryption / `mlock` (no real security in a single
process — the key lives where a breach already is); OAuth; out-of-band web
capture form; per-request IP rotation.

## 4. Architecture

Data flow is unchanged: `poeninja → store → discord` and `discord → trade`. The
`trade/` layer keeps **no Discord knowledge** (the registry is keyed by a raw
`u64` user id, not a serenity type).

Two structural changes:

1. **New unit `src/trade/session.rs`** — `ProxyConfig`, `MemberSession`,
   `MemberSessions` (the registry: in-memory session store + per-member
   proxy-bound `reqwest::Client` cache), and `TradeSession` (the per-call
   `{client, cookie}` handed to the pricer).
2. **Trade calls become session-injected per call** instead of session-baked at
   construction. `TradeClient` keeps a base `http` client used **only** for the
   startup stat-catalog fetch (a cheap cached GET that works direct/anonymous);
   per-member `search`/`fetch` use the injected `TradeSession.client` and set the
   cookie per request.

## 5. Components

### 5.1 `ProxyConfig` and the sticky-session URL (`src/trade/session.rs`)

```rust
#[derive(Clone)] // no Debug derive (custom/omitted — pass is an operator secret)
pub struct ProxyConfig {
    pub gateway: String,         // host:port, e.g. "geo.iproyal.com:12321"
    pub user: String,
    pub pass: String,            // operator secret — kept out of Debug, like DISCORD_TOKEN
    pub country: String,         // ISO-2 lowercase, e.g. "us"
    pub lifetime_mins: u64,      // sticky IP lifetime mins, default 30 (IPRoyal max 7d)
}
```

The proxy password is **operator** config (from `.env`), handled like the
existing `DISCORD_TOKEN`/`POESESSID` plain-`String` secrets — kept out of any
`Debug`, never committed. `secrecy::SecretString` (with zeroize) is reserved for
the new, higher-risk asset: **members'** account cookies (§5.2/§7). (`SecretString`
is intentionally non-`Clone`, so making it a `ProxyConfig` field would also break
the `Clone` derive — another reason the operator password stays a plain `String`.)

**Per-member sticky proxy URL — format verified against IPRoyal's official docs**
(docs.iproyal.com/proxies/residential/proxy/rotation, fetched 2026-06-18):

```
socks5h://{user}:{pass}_country-{country}_session-{sid}_lifetime-{lifetime}m@{gateway}
```

Verified IPRoyal facts (these correct the earlier tablesnipe-derived guesses):

- **Targeting/session params attach to the PASSWORD**, not the username:
  `username:password_country-br_session-sgn34f3e_lifetime-10m@geo.iproyal.com:12321`.
  Keys/separators are `_key-value`: `_country-`, `_session-`, `_lifetime-` (plus
  optional `_killswitch-1`, `_forcerandom-1`).
- **`sid` (session id) must be exactly 8 alphanumeric characters.** Use an FNV-1a
  **32-bit** hash of the member's user id, zero-padded lowercase hex → exactly
  8 hex chars: deterministic per member (same member ⇒ same IP within the lifetime
  window), no PII, within IPRoyal's length rule. (`DefaultHasher` is avoided — not
  stability-guaranteed across Rust releases.)
- **Lifetime:** min 1s, max 7d, one unit only; `30m` is valid (our default).
- **Gateway/port:** `geo.iproyal.com` is the auto-region endpoint; the doc example
  port is `12321`, but the SOCKS5 port is plan/dashboard-specific — the operator
  copies the exact SOCKS5 `host:port` from their IPRoyal dashboard into
  `PROXY_GATEWAY` (regional hosts like `us.proxy.iproyal.com` also exist; we use
  `geo` + `_country-` instead). We do **not** hardcode a port.
- **Omitting `_session-` yields rotating (new IP per request)** — our sticky
  behavior comes precisely from including `_session-`+`_lifetime-`.
- **Encoding:** percent-encode only the **base** `pass` (operator passwords can
  contain URL-reserved chars); append the literal `_country-…_session-…_lifetime-…`
  suffix after encoding (underscores/hyphens/alphanumerics are URL-safe). Still
  confirm at planning time that reqwest's `socks` feature reads `user:pass` from
  the `socks5h://` URL userinfo as SOCKS5 auth (SOCKS5 auth must live in the URL —
  `Proxy::basic_auth` is HTTP-proxy-only).

A pure helper builds this string and is unit-tested for exact format and stability:

```rust
fn sticky_proxy_url(cfg: &ProxyConfig, user_id: u64) -> String;
fn sticky_session_id(user_id: u64) -> String; // exactly 8 hex chars (FNV-1a 32-bit of user_id)
```

`ProxyConfig` is `None`-able overall: if proxy env is unset, the registry builds
**unproxied** member clients (per-member cookie still applies). This keeps local
dev / tests runnable without a proxy account.

### 5.2 `MemberSession` and `MemberSessions` (the registry)

```rust
struct MemberSession {
    cookie: Arc<SecretString>, // Arc so session_for can hand out a shared ref
                               // (SecretString is non-Clone); zeroized when the
                               // last Arc — store entry or in-flight call — drops
    captured_at: Instant,
}

pub struct MemberSessions {
    sessions: RwLock<HashMap<u64, MemberSession>>,
    clients:  RwLock<HashMap<u64, Arc<reqwest::Client>>>, // per-member, proxy baked in
    proxy: Option<ProxyConfig>,
    ttl: Duration,                 // SESSION_TTL_MINS
}
// (currency conversion stays in TradeClient.rates — the registry needs no rate table)
```

Deliberately **no `Debug`/`Serialize` derive** on either type.

API:

```rust
impl MemberSessions {
    pub fn new(proxy: Option<ProxyConfig>, ttl: Duration) -> Self;

    /// present AND captured_at.elapsed() < ttl
    pub fn has_live_session(&self, user_id: u64) -> bool;

    /// Validate the cookie+proxy with one live trade2 call, then store.
    /// Returns Err with a member-safe message on validation failure.
    pub async fn store(&self, user_id: u64, poesessid: SecretString) -> Result<()>;

    /// Build the per-call DI handle (returns None if no live session).
    pub fn session_for(&self, user_id: u64) -> Option<TradeSession>;

    /// Revocation: drop session + cached client (zeroizes the cookie).
    pub fn forget(&self, user_id: u64);
}
```

- The per-member `reqwest::Client` is built lazily on first `store`/`session_for`
  with `Client::builder().user_agent(USER_AGENT)` plus, when `proxy` is `Some`,
  `.proxy(reqwest::Proxy::all(sticky_proxy_url(..))?)`. It is cached in `clients`
  and reused (connection pool + stable IP). The **cookie is never baked into the
  client's default headers** (see §7).
- `store` validation: build the member client, issue one cheap live trade2
  `search` (e.g. a minimal known-good query for the active league) using the
  candidate cookie; success ⇒ insert; failure ⇒ `Err`. This also proves the
  proxy works, so a broken proxy is caught at connect time, not mid-pricing.

### 5.3 `TradeSession` (per-call dependency injection)

```rust
pub struct TradeSession {
    pub client: Arc<reqwest::Client>,
    pub cookie: Arc<SecretString>,
}
```

No `Debug`. Built by `MemberSessions::session_for`, which clones the `Arc`s out of
the stored `MemberSession` (cheap; no secret copied). A `TradeSession::for_test()`
constructor (default client + dummy secret) keeps offline tests trivial.

### 5.4 Trade-layer refactor — thread `&TradeSession`

Add `session: &TradeSession` as the trailing parameter and pass it through the
whole call chain. Current → new signatures:

| Symbol (file) | Change |
|---|---|
| `TradeApi::search` (`trade/client.rs`) | `search(&self, query, session: &TradeSession)` — use `session.client`; set cookie per request |
| `TradeApi::fetch` (`trade/client.rs`) | `fetch(&self, query_id, hashes, session: &TradeSession)` — same |
| `Comparables::comparables` (`trade/ablation.rs`) | `comparables(&self, query, limit, session: &TradeSession)` |
| `ablation::gather_comparables` | add `session`, forward to `api.search/fetch` |
| `ablation::estimate` | add `session`, forward to `comparables` |
| `ablation::breakdown` | add `session`, forward to `comparables` |
| `TradePricer::price` (`trade/mod.rs`) | `price(&self, item, league, session: &TradeSession)` |
| `TradePricer::breakdown` (`trade/mod.rs`) | `breakdown(&self, item, league, session: &TradeSession)` |

Per-request cookie application (replaces the construction-time default header):

```rust
fn with_cookie(rb: reqwest::RequestBuilder, cookie: &SecretString) -> reqwest::RequestBuilder {
    let mut v = HeaderValue::from_str(&format!("POESESSID={}", cookie.expose_secret()))
        .expect("cookie header");
    v.set_sensitive(true); // keep it out of any header-logging layer
    rb.header(header::COOKIE, v)
}
```

`TradeClient` keeps its `http` field and `fetch_stats_raw` (catalog) **unchanged**
— catalog uses the base client. `send_with_retry`, `parse_fetch`, the 60s query
cache, and the 429 `Retry-After` backoff are unchanged; `search`/`fetch` just
build their request from `session.client` instead of `self.http`.

`TradeClient::new` keeps `(poesessid, rates)` — the operator session remains the
base client for the catalog GET (optional; anonymous works). It is no longer the
session used for member searches.

The mock `Comparables` in tests and every `estimate`/`breakdown`/
`gather_comparables` call site gain the param (tests pass `TradeSession::for_test()`).

### 5.5 Discord capture flow (`src/discord/paste.rs`)

`paste` → `PasteModal` (item text) → parse → `store::route` → `MatchOutcome::Rare`
→ `price_rare`. Today `price_rare` calls `pricer.price(parsed, league)`. New:

1. `let uid = ctx.author().id.get();`
2. `if let Some(session) = ctx.data().sessions.session_for(uid) { run_pricing(ctx, parsed, league, &session).await }`
3. `else { prompt_connect(ctx, parsed, league).await }`

**Factor the existing pricing body into `run_pricing(ctx, parsed, league, session)`** —
the embed + "Break it down" button + `ComponentInteractionCollector` logic moves
there verbatim, with `pricer.price/breakdown` now taking `session`.

**`prompt_connect`** (Discord disallows opening a modal directly off a modal
submit, so we bounce through a button):

1. Stash `parsed` in a short-TTL pending map keyed by `uid`
   (`pending: RwLock<HashMap<u64,(ParsedItem, Instant)>>` — lives on **`Data`**,
   the Discord layer, not the registry: `ParsedItem` is an `itemtext` type and the
   `trade/` registry must stay Discord-agnostic; 5-min TTL).
2. Send an **ephemeral** reply: short "🔑 Connect your PoE account to price rares"
   explainer + privacy-policy link + a `drp_connect` button.
3. Collect the `drp_connect` component interaction (existing collector pattern).
4. On click, open the POESESSID modal via
   `poise::modal::execute_modal_on_component_interaction::<ConnectModal>(...)`
   (confirm exact poise-0.6 signature during planning; query context7 if needed).
5. On modal submit: `sessions.store(uid, SecretString::new(input)).await`.
   - Ok ⇒ pull the stashed `parsed`, `session_for(uid)`, and call `run_pricing`
     (ephemeral followup).
   - Err ⇒ ephemeral "Couldn't reach trade with that session — is your POESESSID
     current? Copy it again and retry." Never echo the value.

```rust
#[derive(poise::Modal)]            // NOTE: no #[derive(Debug)] — holds the secret
#[name = "Connect your PoE account"]
struct ConnectModal {
    #[name = "POESESSID (from your pathofexile.com cookies)"]
    #[placeholder = "32-character hex value"]
    poesessid: String,
}
```

Light input validation before the live call: trim, reject empty / obviously wrong
(POESESSID is a 32-char hex string) with a friendly message — avoids burning a
trade2 call on a paste error.

### 5.6 `/logout` command + `/help`

- New `src/discord/logout.rs`: `#[poise::command(slash_command)] pub async fn logout(...)`
  → `ctx.data().sessions.forget(uid)` → **ephemeral** "Disconnected. Also log out
  on pathofexile.com to invalidate the cookie server-side." Registered in
  `main`'s command list.
- `/help` (`src/discord/help.rs`): add a line describing connect-on-first-paste
  and `/logout`.

## 6. Data flow — first `/paste` of a rare (no session yet)

```
member: /paste → PasteModal(item) → parse → route = Rare
  → session_for(uid) = None
  → stash parsed; ephemeral [🔑 Connect] button
member: clicks Connect → ConnectModal(POESESSID)
  → sessions.store(uid, secret):
        build member client (sticky proxy) → live trade2 search (validate)
        ok → insert SecretString
  → session_for(uid) = Some → run_pricing(parsed, session)
        pricer.price(parsed, league, session) → embed + [Break it down]
```

Subsequent pastes (live session): straight to `run_pricing`. After TTL/`/logout`:
re-prompt.

## 7. Security (store hardening — as agreed)

1. **`secrecy::SecretString`** for every member cookie (stored as `Arc<SecretString>`
   so it can be shared without copying and is zeroized when the last holder —
   store entry or in-flight call — drops): redacted `Debug`/`Display`, **zeroize
   on drop**. The proxy password is operator config, handled like the existing
   plain-`String` operator secrets (kept out of `Debug`, never committed).
2. **Secret never baked into a long-lived client.** Source of truth is the
   `SecretString`; materialized into a **sensitive** `HeaderValue` per request,
   then dropped. The per-member client carries only the *proxy* config.
3. **TTL eviction** (`SESSION_TTL_MINS`); stale credentials don't linger.
4. **`/logout` revocation** + advice to log out on the website.
5. **Capture hygiene:** modal text input (never a chat message → never in channel
   history), all bot replies ephemeral, value never echoed. Honest caveat:
   Discord sees the value in the interaction payload (TLS) — inherent to in-Discord
   capture; out of scope to avoid.
6. **Never in any export path:** not in the probe log, not in `/paste` breakdown
   output, no `Debug`/`serde` on `MemberSession`/`MemberSessions`/`TradeSession`/
   `ConnectModal`.

## 8. Config & dependencies

New env (all in `.env.example` with placeholders; secrets are **never** committed):

| Var | Meaning | Default |
|---|---|---|
| `PROXY_GATEWAY` | IPRoyal SOCKS5 gateway `host:port` (copy from dashboard; e.g. `geo.iproyal.com:12321`) | (unset ⇒ unproxied) |
| `PROXY_USER` | proxy username | — |
| `PROXY_PASS` | proxy password — **secret** | — |
| `PROXY_COUNTRY` | residential country code | `us` |
| `PROXY_SESSION_LIFETIME_MINS` | sticky IP lifetime | `30` |
| `SESSION_TTL_MINS` | member session lifetime in memory | `180` |

`Config` (`src/config.rs`) gains: `proxy: Option<ProxyConfig>` and
`session_ttl_mins: u64`. `proxy` is `Some` only when `PROXY_GATEWAY`+`PROXY_USER`+
`PROXY_PASS` are all present. `PROXY_PASS` and the operator `POESESSID` stay out of
the hand-written `Debug` impl.

The existing operator-session env var is **renamed `POE_SESSID` → `POESESSID`** to
match the real cookie name (config field `poe_sessid` → `poesessid`); it is
retained as the base/catalog client + local-testing fallback. **Deployment note:**
the box `.env` / terraform secret must be renamed to `POESESSID` at rollout, or the
operator session goes unset (anonymous catalog fetch — still works).

Construction (`src/main.rs`): build `MemberSessions::new(config.proxy.clone(),
ttl, rates.clone())`, wrap in `Arc`, add `pub sessions: Arc<MemberSessions>` to
`Data`. Register `discord::logout::logout()` in the command list.

`Cargo.toml`: add `secrecy = "0.8"` (brings `zeroize`); add `"socks"` to
`reqwest` features (SOCKS5 + URL-embedded credentials for `socks5h://`).

## 9. Error handling

- **Invalid/expired POESESSID at capture:** `store` returns `Err`; ephemeral
  retry prompt. No partial state stored.
- **Proxy failure:** surfaced by the same validation call → same retry prompt
  (message mentions trade unreachable; logs the proxy error **without**
  credentials).
- **Session expires mid-use (TTL):** `session_for` returns `None` on next paste →
  re-prompt. In-flight calls finish on the client they already hold.
- **429:** unchanged `send_with_retry` backoff; never retry through a 429.
- **No proxy configured:** registry builds unproxied member clients; per-member
  cookies still apply (acceptable for dev; production sets the proxy).
- The refresher / catalog / poe.ninja paths are untouched and never panic.

## 10. Testing (offline by default; network is `#[ignore]`d)

Unit:
- `sticky_session_id` is stable & deterministic per `user_id`; differs across ids.
- `sticky_proxy_url` exact format (country/session/lifetime placement).
- Registry: `store`/`has_live_session`/`session_for`/`forget`; TTL expiry
  (inject an old `captured_at`); `forget` removes both session and cached client.
- `SecretString` redaction: `format!("{:?}", session)` / log lines never contain
  the value (assert the secret substring is absent).
- `ConnectModal` POESESSID format validation (accept 32-hex, reject empty/short).
- Pending-stash TTL.
- Trade-layer threading compiles with the mock `Comparables` ignoring the session;
  existing ablation `estimate`/`breakdown` tests pass `TradeSession::for_test()`.

`#[ignore]` smoke: real `store` validation through a real proxy + a real (env)
POESESSID against the live league.

## 11. Privacy Policy update (separate `drp-legal` repo — required deliverable)

Update drp.pme.it. Replace the "no database / stores no personal data" statement
with disclosure that the bot:

- captures and holds a member's **POESESSID in memory only**, transiently, used
  **solely to perform that member's own** trade searches;
- never shares it, never writes it to disk, loses it on restart and after the
  session TTL;
- routes that member's trade traffic via a residential proxy;
- lets a member delete it at any time with `/logout`.

This is a doc change in the other repo; tracked as a plan task but implemented
there. Pricing capture must not ship to members before the policy is live.

## 12. Files touched (summary)

| File | Change |
|---|---|
| `src/trade/session.rs` | **new** — `ProxyConfig`, `MemberSession`, `MemberSessions`, `TradeSession`, url helpers |
| `src/trade/mod.rs` | export `session`; `TradePricer::{price,breakdown}` take `&TradeSession` |
| `src/trade/client.rs` | `TradeApi::{search,fetch}` + `Comparables::comparables` take `&TradeSession`; per-request sensitive cookie; base `http` keeps catalog |
| `src/trade/ablation.rs` | `comparables`/`gather_comparables`/`estimate`/`breakdown` thread `&TradeSession`; mock updated |
| `src/discord/paste.rs` | `price_rare` session check; extract `run_pricing`; add `prompt_connect` + `ConnectModal` |
| `src/discord/logout.rs` | **new** — `/logout` |
| `src/discord/mod.rs` | `Data.sessions: Arc<MemberSessions>`; `Data.pending` map; `logout` module decl |
| `src/discord/help.rs` | mention connect + `/logout` |
| `src/config.rs` | `proxy: Option<ProxyConfig>`, `session_ttl_mins`; keep secrets out of `Debug` |
| `src/main.rs` | build `MemberSessions`, add to `Data`, register `logout` |
| `Cargo.toml` | `secrecy`; reqwest `socks` feature |
| `.env.example` | document the new vars; rename `POE_SESSID` → `POESESSID` |
| `CLAUDE.md` | update the Configuration section (`POE_SESSID` → `POESESSID`; note proxy vars) |
| `drp-legal` (other repo) | Privacy Policy update |
