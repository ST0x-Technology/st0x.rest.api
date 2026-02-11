# Agents

## Build & Run
- All commands must be run inside nix: `nix develop -c <command>`
- Run checks: `nix develop -c cargo check`
- Run tests: `nix develop -c cargo test`
- Run server: `nix develop -c cargo run`

## Code Rules
- Never use `expect` or `unwrap` in production code; handle errors gracefully or exit with a message
- Every route handler must log appropriately using tracing (request received, errors, key decisions)
- All async route handlers must use `TracingSpan` and `.instrument(span.0)` for span propagation
- All API errors must go through the `ApiError` enum, never return raw status codes
- Keep OpenAPI annotations (`#[utoipa::path(...)]`) in sync when adding or modifying routes
- Do not commit `.env` or secrets; use `.env.example` for documenting env vars
