# st0x REST API

REST API for st0x orderbook operations. Built with Rocket, backed by SQLite, and authenticated via API keys using HTTP Basic auth.

## Prerequisites

- [Nix](https://nixos.org/download/) with flakes enabled

## Setup

### 1. Clone and initialize submodules

```sh
git clone <repo-url>
cd st0x-rest-api
git submodule update --init --recursive
```

### 2. Run the prep script

```sh
nix develop -c bash prep.sh
```

This writes `COMMIT_SHA` to `.env` and bootstraps the orderbook submodule.

### 3. Configure

The config file is at `config/dev.toml`. The only value that may need updating is `registry_url` — it points to a pinned GitHub raw URL so it should work out of the box.

If it's stale, grab the latest from production:

```sh
curl -u "KEY_ID:SECRET" https://<SERVER_IP>/registry -k
```

and update `registry_url` in `config/dev.toml`.

### 4. Create an API key

```sh
nix develop -c cargo run -- keys --config config/dev.toml create \
  --label "dev-key" \
  --owner "you@example.com"
```

This prints a **Key ID** (UUID) and **Secret** (base64 string). Save both — the secret is hashed with Argon2 and cannot be recovered.

### 5. Set log level (optional)

```sh
export RUST_LOG=st0x_rest_api=info,rocket=warn,warn
```

### 6. Start the server

```sh
COMMIT_SHA=$(git rev-parse HEAD) nix develop -c cargo run -- serve --config config/dev.toml
```

Server runs on `http://127.0.0.1:8000`. Swagger UI is available at `/swagger`.

The database (`data/st0x.db`) is created and migrated automatically on first run — no manual DB setup needed.

### 7. Test it

```sh
curl -u "KEY_ID:SECRET" http://localhost:8000/v1/tokens
```

All endpoints except `/health` require HTTP Basic Auth with the key ID and secret.

## API Key Management

Use the `keys` subcommand to manage credentials. All commands require `--config` to point at the config file.

```sh
# List keys
nix develop -c cargo run -- keys --config config/dev.toml list

# Revoke a key (sets it to inactive, rejected at auth)
nix develop -c cargo run -- keys --config config/dev.toml revoke <KEY_ID>

# Delete a key (permanent removal)
nix develop -c cargo run -- keys --config config/dev.toml delete <KEY_ID>
```

## Authenticating API Requests

Use HTTP Basic auth with the key ID as the username and the secret as the password:

```sh
curl -u "KEY_ID:SECRET" http://localhost:8000/v1/tokens
```

Or with an explicit header:

```sh
curl -H "Authorization: Basic $(echo -n 'KEY_ID:SECRET' | base64)" http://localhost:8000/v1/tokens
```

## Development

```sh
nix develop -c cargo fmt          # format
nix develop -c rainix-rs-static   # lint
nix develop -c cargo test         # test
```

Always run the formatter and linter before committing.
