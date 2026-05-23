# Agents

## Build & Run
- All commands must be run inside nix: `nix develop -c <command>`
- Run checks: `nix develop -c cargo check`
- Run tests: `nix develop -c cargo test`
- Run server: `nix develop -c cargo run`

## Submodule Patch (rain.orderbook)
- The build depends on uncommitted patches against `lib/rain.orderbook` at SHA `57253129e47b1c7f744a514c131a638bb0d7607a` (the `IERC20` `amount→value` rename + signed_contexts threading). These patches live only on the deployed preview server and in `patches/rain-orderbook-deployed.patch`.
- After a fresh clone or `git submodule update --init --force` (which wipes the patches), re-apply with:
  - `git -C lib/rain.orderbook checkout 57253129e47b1c7f744a514c131a638bb0d7607a`
  - `git -C lib/rain.orderbook apply ../../patches/rain-orderbook-deployed.patch`
- Forge artifacts must be built before `cargo`. `prep-sol-artifacts` covers `lib/rain.orderbook` and `lib/.../rain.math.float`, but the nested `lib/rain.orderbook/lib/rain.interpreter` needs `forge build` run inside it as well.

## Preview Server (api.preview.st0x.io)
- SSH: `ssh root@api.preview.st0x.io`
- Running deployment: `/root/st0x.rest.api/` — submodule pinned to `57253129e` with the patches above applied in the working tree (source of `patches/rain-orderbook-deployed.patch`).
- Manual build clone: `/root/st0x.rest.api-preview/` — used for ad-hoc rebuilds of the `preview` branch. After a fresh `git pull`/submodule init here, overlay the patched submodule from the running deployment: `rm -rf lib/rain.orderbook && cp -a /root/st0x.rest.api/lib/rain.orderbook lib/rain.orderbook`.
- Build sequence (run inside `nix develop`, takes ~7 min cold):
  1. `prep-sol-artifacts`
  2. `(cd lib/rain.orderbook/lib/rain.interpreter && forge build)`
  3. `cargo build --release`
- Run under `nohup ... &` so the build survives SSH disconnects; log to `/root/preview-build.log`. Binary lands at `target/release/st0x_rest_api`.

## Post-Implementation
- After every implementation, run formatter and linter before committing:
  - `nix develop -c cargo fmt`
  - `nix develop -c rainix-rs-static`

## Code Rules
- Never use `expect` or `unwrap` in production code; handle errors gracefully or exit with a message
- Every route handler must log appropriately using tracing (request received, errors, key decisions)
- All async route handlers must use `TracingSpan` and `.instrument(span.0)` for span propagation
- All API errors must go through the `ApiError` enum, never return raw status codes
- Keep OpenAPI annotations (`#[utoipa::path(...)]`) in sync when adding or modifying routes
- Do not commit `.env` or secrets; use `.env.example` for documenting env vars
