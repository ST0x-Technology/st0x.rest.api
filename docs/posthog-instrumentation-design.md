# PostHog server-side instrumentation for the st0x REST API

**Date:** 2026-07-14 **Status:** Approved (design), implementing **Author:**
Alastair Ong + Claude

## Goal

Instrument the REST API (Rust / Rocket) with PostHog so we can understand, per
API client, **how much trade volume flows through the API** and **who the unique
traders are across venues** (site vs. third-party integrators). This is the
server-side other half of the picture to the frontend PostHog instrumentation on
the st0x site.

Events go to the **same PostHog project as the site** (project 122548, EU cloud)
so site + API data live in one place. The API uses the **project token**
(`POSTHOG_PROJECT_TOKEN`) for that same project. PostHog's public
event-ingestion API identifies the destination project with this token; it does
not use a personal or project-secret API key.

## Key facts about the codebase

- **Stack:** Rust, Rocket 0.5, SQLite (sqlx), `reqwest` already a dependency.
- **Auth:** HTTP Basic (`key_id:secret`, Argon2). Every key row carries `label`
  and `owner` — so each request is already attributable to a named client. The
  st0x site is one key; each integrator has its own. `AuthenticatedKey` is
  available in handlers.
- **Cross-cutting hook:** the existing `UsageLogger` fairing already runs on
  every authenticated response. We add a sibling `AnalyticsFairing` for the
  generic event.
- **Value endpoints:** `POST /v1/swap/quote` and `/v1/swap/calldata` (+`/v2`).
  The calldata request carries `taker: Address` — the end-user wallet.
- **Order-deploy endpoints (`/v1/order/dca`, `/solver`) are `todo!()` stubs
  today** — instrumentation for TVL-creation is deferred until they are
  implemented.

## Design

### Analytics module (`src/analytics/`)

- `trait AnalyticsSink: Send + Sync { fn capture(&self, event: AnalyticsEvent); }`
  — synchronous, fire-and-forget from the caller's perspective.
- `AnalyticsEvent { event: &'static str, distinct_id: String, properties: Map }`.
- Implementations:
  - `PostHogSink` — holds a `reqwest::Client` (3s timeout), posts to
    `{host}/i/v0/e/`, spawns a bounded (semaphore-capped) tokio task per event.
    Failures are logged at `warn` and **never** affect the request. Drops events
    if saturated (same pattern as `UsageLogger`).
  - Disabled analytics holds no sink. When `POSTHOG_PROJECT_TOKEN` is unset,
    request attribution and event construction are skipped.
  - `RecordingSink` (test-only) — records events into a `Mutex<Vec<_>>` for
    assertions.
- `Analytics(Option<Arc<dyn AnalyticsSink>>)` is Rocket-managed state, built via
  `Analytics::from_env()`. Host defaults to `https://eu.i.posthog.com`.

### Identity / attribution

- Auth caches `AuthClientInfo { key_id, label, owner }` in request-local state
  so the fairing (which has no `AuthenticatedKey`) can attribute the generic
  event.
- Every event carries `api_client_key_id`, `api_client_label`,
  `api_client_owner`. "Site vs. integrator" is defined **in PostHog** by
  filtering on `api_client_owner` (avoids hardcoding a client identity in the
  API).
- `distinct_id`:
  - Trader-bearing event (`swap_calldata_generated`): the **lowercased `taker`
    wallet** — the _same_ identifier the site uses in `posthog.identify()`, so
    one human is one PostHog person across the site and every integrator
    (cross-venue uniqueness).
  - Client-scoped events (`api_request`, `swap_quoted`, `swap_quote_failed`,
    `swap_calldata_failed`): `client:{key_id}`. Quote events never include the
    optional taker wallet. Calldata failure events retain it as an event
    property for parity with successful calldata generation.

### Events

| Event                     | Source                                                                   | distinct_id        | Properties                                                                                                                                                                                                |
| ------------------------- | ------------------------------------------------------------------------ | ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `api_request`             | `AnalyticsFairing` (every auth'd response)                               | `client:{key_id}`  | `method`, `endpoint` (normalized), `status_code`, `latency_ms`, client attribution                                                                                                                        |
| `swap_quoted`             | `POST /v1/swap/quote` + `/v2` (on 200)                                   | `client:{key_id}`  | `input_token`, `output_token`, `denomination`, `estimated_input`, `estimated_output`, `estimated_io_ratio`, `api_version`, optional v2 `mode`, client attribution                                         |
| `swap_quote_failed`       | `POST /v1/swap/quote` + `/v2` (after body parsing and authentication)    | `client:{key_id}`  | `input_token`, `output_token`, numeric `requested_amount` when safe to capture, `denomination`, `api_version`, optional v2 `mode`, `error_code`, `status_code`, `same_token`, client attribution          |
| `swap_calldata_generated` | `POST /v1/swap/calldata` + `/v2` (on 200)                                | lowercased `taker` | `taker`, `input_token`, `output_token`, `denomination`, `estimated_input`, `value`, `api_version`, (`mode` for v2), client attribution                                                                    |
| `swap_calldata_failed`    | `POST /v1/swap/calldata` + `/v2` (after body parsing and authentication) | `client:{key_id}`  | `taker`, `input_token`, `output_token`, numeric `requested_amount` when safe to capture, `denomination`, `api_version`, optional v2 `mode`, `error_code`, `status_code`, `same_token`, client attribution |

`endpoint` normalization collapses `0x…` addresses/hashes and numeric ids to
placeholders (`/v1/tokens/{address}/details`) to keep cardinality bounded.

Failure events begin after Rocket has parsed the request body. Malformed JSON
that the JSON guard rejects with 422 therefore appears only as an `api_request`
event. Success-rate monitoring for swap endpoints must pair the swap success and
failure events with an `api_request` alert for `status_code >= 400` so malformed
requests are not mistaken for absent traffic.

### Volume & privacy semantics (documented, not enforced)

- The API observes **requests**, not on-chain settlement. "Volume" = swaps
  quoted / calldata generated; on-chain truth stays in the subgraph.
- Volume is captured as **raw token amounts + token addresses** (no price
  dependency); USD is derivable downstream.
- Failure amounts are accepted by the API as strings, so analytics includes
  `requested_amount` only when it is a valid numeric value no longer than 128
  bytes. Arbitrary malformed request text is never forwarded to PostHog.

### Config, secrets, deployment

- `POSTHOG_PROJECT_TOKEN` and optional `POSTHOG_HOST` are read from
  **environment** and documented in `.env.example`. The production NixOS
  configuration injects the public PostHog project token and explicit EU capture
  host into the systemd service. Preview does not configure a token and
  therefore keeps analytics disabled. When the project token is unset, analytics
  is disabled.

### Testing

- Unit tests: path normalization, client-attribution props, distinct_id
  selection, event property construction, and the disabled/no-op path.
- Integration tests: `TestClientBuilder` injects a `RecordingSink`; assert that
  success and failure swap events plus `api_request` are captured with the
  expected `distinct_id`, privacy boundaries, and properties.

### Out of scope (this PR)

- `order_deployed` / TVL-creation events (endpoints are `todo!()` stubs).
- Periodic TVL snapshots (TVL is tracked elsewhere).
- USD normalization inside the API.
