# st0x REST API

REST API for st0x orderbook operations. Built with Rocket, backed by SQLite, and authenticated via API keys using HTTP Basic auth.

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

## Usage

The binary has two subcommands:

```
st0x_rest_api serve     Start the API server
st0x_rest_api keys      Manage API keys
```

### Starting the server

```sh
nix develop -c cargo run serve
```

The server starts on `http://localhost:8000` by default. Swagger UI is available at `/swagger`.

### API key management

All API routes (except `/health`) require HTTP Basic authentication. Use the `keys` subcommand to manage credentials.

#### Create a key

```sh
nix develop -c cargo run keys create --label "partner-x" --owner "contact@example.com"
```

Output:

```
API key created successfully

Key ID:  <uuid>
Secret:  <base64-encoded-secret>
Label:   partner-x
Owner:   contact@example.com

IMPORTANT: Store the secret securely. It will not be shown again.
```

The secret is hashed with Argon2 before storage. There is no way to recover it.

#### List keys

```sh
nix develop -c cargo run keys list
```

Shows all keys with their ID, label, owner, active status, and timestamps.

#### Revoke a key

```sh
nix develop -c cargo run keys revoke <KEY_ID>
```

Sets the key to inactive. Revoked keys are rejected at authentication.

#### Delete a key

```sh
nix develop -c cargo run keys delete <KEY_ID>
```

Permanently removes the key from the database.

### Authenticating API requests

Use HTTP Basic auth with the key ID as the username and the secret as the password:

```sh
curl -u "<KEY_ID>:<SECRET>" http://localhost:8000/v1/tokens
```

Or with an explicit header:

```sh
curl -H "Authorization: Basic $(echo -n '<KEY_ID>:<SECRET>' | base64)" http://localhost:8000/v1/tokens
```

## Development

```sh
nix develop -c cargo fmt
nix develop -c rainix-rs-static
nix develop -c cargo test
```
