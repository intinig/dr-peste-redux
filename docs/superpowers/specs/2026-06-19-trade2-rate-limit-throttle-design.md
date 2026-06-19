# trade2 Proactive Rate-Limit Throttle — Design

**Date:** 2026-06-19
**Status:** Approved in brainstorming; pending spec review before planning.
**Builds on:** the per-member sessions + sticky proxy (PR #10) and the rare-pricing
ablation breakdown (PRs #5/#11/#12).

## Problem

Live rare pricing fails under real use. Production logs (2026-06-18) show the bot
repeatedly 429'd by trade2:

```
WARN trade::client: trade2 rate-limited; backing off wait_secs=60   (×4, 19:42–19:45)
WARN discord::paste: trade breakdown failed error=trade2 search failed   (19:46)
```

**Root cause (confirmed):** the bot issues too many trade2 **searches** in a burst
for trade2's tightest limit (search is ~5/12s on the account scope). A single
"Break it down" runs `breakdown()` sequentially over baseline + one drop per
probed stat + a pairwise probe — and each `estimate()` calls `gather_comparables`,
which can itself relax (re-search) up to `max_relax` times. So one breakdown can
emit ~15–20 searches in seconds, well over the cap → 429 → 60s backoff → the next
search in the sequence also 429s → the breakdown gives up with "trade2 search
failed." Rapid back-to-back `/paste` testing compounds it.

The current defense is **reactive only**: `send_with_retry` sleeps for the
server-advised period *after* a 429 (≤2 retries, then errors). `RateRule`,
`parse_rate_rules`, and `retry_after_secs` already parse trade2's
`X-Rate-Limit-*` headers, but nothing reads the live `-State` usage or paces
*before* sending.

**Scope decision (user):** keep the full ablation breakdown intact — do **not**
reduce probe count or disable relaxation. Fix it purely by throttling so the full
breakdown runs without ever hitting 429 (paced slower, but it always completes).

## Design

A **per-member, header-adaptive sliding-window rate limiter** that gates every
trade2 search/fetch *before* it is sent, calibrated from the rate-limit headers on
each response. The existing reactive 429 backoff stays as a safety net behind it.

### Why per-member

Limits are enforced per-account **and** per-IP. Every member has their own
POESESSID (account budget) and their own sticky residential proxy IP (IP budget),
so their rate budgets are independent. A global limiter would serialize unrelated
members and over-throttle; a per-member limiter paces each member against their
own budget. The limiter is therefore owned per `user_id`, alongside the existing
per-member client cache, and injected into the per-call `TradeSession`.

### Components

**`src/trade/limiter.rs` (new) — `RateLimiter`.**

- Two independent **buckets** per member, keyed by endpoint kind
  (`Endpoint::Search`, `Endpoint::Fetch`) — trade2 tracks and reports search and
  fetch limits separately, so they pace independently.
- Each bucket holds:
  - the learned rules for that endpoint (`Vec<RateRule>`, the union of the
    `Account` and `Ip` scopes), and
  - a record of recent send times within the longest rule period.
- Concurrency: the bucket state lives behind a `tokio::sync::Mutex`. `acquire`
  holds the lock across its sleep, so concurrent callers for the same member queue
  and are paced in order. (Breakdown queries are already sequential; this only
  guards the rare paste+breakdown overlap and two-tasks-same-member case.)

Public surface (used by the client):

- `async fn acquire(&self, ep: Endpoint)` — prune the bucket's window to the
  longest period; if sending now would violate any rule (count within that rule's
  period ≥ `max`), sleep until the window has room (the max wait across all rules);
  then record the send timestamp.
- `fn observe(&self, ep: Endpoint, headers: &HeaderMap)` — replace the bucket's
  learned rules from `X-Rate-Limit-Account` / `X-Rate-Limit-Ip` (via the existing
  `parse_rate_rules`). Reconcile usage from `X-Rate-Limit-Account-State` /
  `-Ip-State`: if the server reports more current hits in a window than our local
  record holds (e.g. usage we didn't generate), seed synthetic timestamps so we
  back off to match. Never *lowers* our count below what we observed.

**Pure core for testability.** The scheduling decision is a pure function

```
fn wait_for(rules: &[RateRule], window: &[f64 /*elapsed secs, ascending*/], now: f64) -> f64
```

returning the seconds to wait before the next send. `acquire` is the thin async
wrapper: lock → compute elapsed-seconds view of the window → `wait_for` → sleep →
record `Instant::now()`. Tests exercise `wait_for` with synthetic timestamps (no
real sleeping, no clock construction).

**Defaults before first response.** Until a bucket has seen a live response, it
uses conservative built-in rules approximating the documented trade2 ceilings, so
the very first burst is already paced rather than relying on a 429 to learn.

### Wiring

- `TradeSession` gains `limiter: Arc<RateLimiter>` (per member). `TradeSession::for_test()`
  gets a limiter with permissive rules so offline tests don't sleep.
- `MemberSessions` caches one `Arc<RateLimiter>` per `user_id` (mirroring the
  client cache), creates it lazily in `session_for`, and drops it in `forget`
  (so a re-prompted member starts fresh — acceptable, the budget is the
  account's, not ours).
- `send_with_retry` gains a `(limiter, endpoint)` parameter. Per attempt:
  `limiter.acquire(ep).await` → send → `limiter.observe(ep, resp.headers())` →
  on 429 (and attempt < 2) back off and retry, else return. This centralizes
  pacing for both `search` and `fetch`.
- The bot-level catalog calls (`fetch_stats_raw`, and `MemberSessions::store`'s
  connectivity probe) are low-frequency and anonymous; they pass a single
  process-wide default `Arc<RateLimiter>` (its own anonymous buckets) — out of the
  hot path, unchanged in behavior beyond a negligible gate.

### Data flow

```
search/fetch
  → send_with_retry(limiter, ep)
      loop:
        limiter.acquire(ep)            # sleeps to stay under the cap
        resp = build().send()
        limiter.observe(ep, headers)   # learn rules + reconcile usage
        if 429 && attempt<2: backoff; continue   # safety net
        return resp.error_for_status()
```

## Error handling

- The throttle never errors — it only delays. Genuine failures (network, non-429
  HTTP, parse) propagate exactly as today.
- The reactive 429 path is retained unchanged as a backstop: if the proactive
  estimate is ever wrong (limits changed mid-flight, usage from another client),
  a 429 still triggers a server-advised backoff instead of a hard failure.
- A malformed/absent header simply leaves the bucket on its current (or default)
  rules; `observe` is best-effort.

## Non-goals (YAGNI)

- No change to the ablation breakdown (probe count, relaxation, sampling all
  unchanged — explicit user decision).
- No change to `COMPARABLE_SAMPLE`, the craftability filter, percentiles, or
  fallback ladder.
- No persistence — limiter state is in-memory and per-process, like sessions.
- No cross-member or global budget coordination (budgets are per-member by design).
- No request coalescing/batching beyond the existing 60s query cache.

## Testing

Offline unit tests (no network, no real sleeps):

- **`wait_for` (pure):** empty window → 0; under cap → 0; at cap → waits until the
  oldest in-window send ages out; multiple rules → returns the max wait; the
  tightest rule governs.
- **`observe` header parsing:** `X-Rate-Limit-Account` / `-Ip` populate the
  bucket's rules (reuse/extend the existing `parse_rate_rules` coverage);
  `-State` showing higher current usage than local seeds back-off; missing headers
  leave rules unchanged.
- **Endpoint independence:** search and fetch buckets don't share a window.
- **Defaults:** a fresh bucket paces a burst before any `observe`.
- **Session plumbing:** `TradeSession::for_test()` limiter is permissive (a tight
  loop of `acquire` returns ~immediately), so existing ablation/client tests stay
  fast and green.

Live acceptance (manual): re-paste the Chiming Staff and run "Break it down";
confirm it completes (slower) with no `trade2 rate-limited` / `trade2 search
failed` in the logs; confirm a plain `/paste` price is still prompt.

## Files touched

| File | Change |
|---|---|
| `src/trade/limiter.rs` | **new** — `RateLimiter`, `Endpoint`, pure `wait_for`, tests |
| `src/trade/mod.rs` | declare `mod limiter;` |
| `src/trade/session.rs` | `TradeSession.limiter`; per-member limiter cache in `MemberSessions` (`session_for`/`forget`); `for_test()` permissive limiter |
| `src/trade/client.rs` | `send_with_retry(limiter, endpoint)`; `search`/`fetch` acquire+observe; catalog calls pass a default limiter |
