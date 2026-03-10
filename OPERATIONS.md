# Operations

Production server management for the st0x REST API.

## Server Access

```sh
ssh root@<SERVER_IP>
```

## Service Management

The API runs as a systemd service called `rest-api.service`.

```sh
# Status
systemctl status rest-api.service

# Restart
systemctl restart rest-api.service

# Stop / Start
systemctl stop rest-api.service
systemctl start rest-api.service
```

## Logs

Live logs:

```sh
journalctl -u rest-api.service -f
```

Historical logs (last hour, last 100 lines, since a date):

```sh
journalctl -u rest-api.service --since "1 hour ago"
journalctl -u rest-api.service -n 100
journalctl -u rest-api.service --since "2026-03-01"
```

### File-based logs

All tracing output is also written as structured JSON to daily-rotating log files. The log directory is configured via `log_dir` in the config file.

Production log directory: `/mnt/data/st0x-rest-api/logs`

Files follow the pattern `st0x-rest-api.log.YYYY-MM-DD` — a new file is created each day:

```
st0x-rest-api.log.2026-03-08
st0x-rest-api.log.2026-03-09
st0x-rest-api.log.2026-03-10
```

To browse them:

```sh
ls -lt /mnt/data/st0x-rest-api/logs/
tail -f /mnt/data/st0x-rest-api/logs/st0x-rest-api.log.$(date +%Y-%m-%d)
```

Since the files are JSON, you can use `jq` to filter:

```sh
# All errors from today
cat /mnt/data/st0x-rest-api/logs/st0x-rest-api.log.$(date +%Y-%m-%d) | jq 'select(.level == "ERROR")'

# Requests to a specific endpoint
cat /mnt/data/st0x-rest-api/logs/st0x-rest-api.log.$(date +%Y-%m-%d) | jq 'select(.fields.uri | contains("/v1/tokens"))'
```

Locally, the same file-based logs are written to `./logs/` (relative to where you run the server).

## Configuration

The production config is passed via the `ExecStart` line in the service file. To find the current config path:

```sh
systemctl cat rest-api.service | grep ExecStart | grep -o '\-\-config [^ ]*' | cut -d' ' -f2
```

Production database: `sqlite:///mnt/data/st0x-rest-api/st0x.db`

## Production Key Management

Key management commands on the server use the production config path. First, find it:

```sh
CONFIG=$(systemctl cat rest-api.service | grep ExecStart | grep -o '\-\-config [^ ]*' | cut -d' ' -f2)
```

Then use the binary directly:

```sh
BINARY=/nix/var/nix/profiles/per-service/rest-api/bin/st0x_rest_api

# Create a key
$BINARY keys --config $CONFIG create --label "partner-name" --owner "contact@example.com"

# List keys
$BINARY keys --config $CONFIG list

# Revoke a key
$BINARY keys --config $CONFIG revoke <KEY_ID>

# Delete a key
$BINARY keys --config $CONFIG delete <KEY_ID>
```

## Registry Updates

The strategy registry URL can be updated at runtime via the admin API:

```sh
curl -X PUT \
  -u "ADMIN_KEY_ID:ADMIN_SECRET" \
  -H "Content-Type: application/json" \
  -d '{"registry_url":"https://raw.githubusercontent.com/rainlanguage/rain.strategies/<COMMIT>/registry"}' \
  https://<SERVER_IP>/admin/registry -k
```

Admin credentials are shared separately and should never be committed.

## Deployment

Deployments are triggered manually via the **Deploy** workflow in GitHub Actions (`cd.yaml`, `workflow_dispatch`). To deploy:

1. Go to the repo's **Actions** tab
2. Select the **Deploy** workflow
3. Click **Run workflow** on the target branch

The workflow builds the Nix package and deploys it to the server via `deploy-rs`.

## API Documentation

User-facing API docs are built with mdbook from `docs/src/`. On the server, the built docs are served from `/var/lib/st0x-docs`.

To update the docs:

1. Edit files in `docs/src/`
2. Rebuild locally: `nix develop -c mdbook build docs`
3. Deploy (docs are included in the deployment)
