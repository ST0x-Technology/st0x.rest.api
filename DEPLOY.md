# Digital Ocean Deployment Guide

This project deploys a NixOS droplet on DigitalOcean via Terraform, then installs
the service using `nixos-anywhere` and `deploy-rs`. All tooling runs inside the
Nix dev shell.

---

## Prerequisites

### 1. Nix with Flakes
Install Nix and enable flakes:
```bash
# Install Nix (if not already installed)
sh <(curl -L https://nixos.org/nix/install) --daemon

# Enable flakes in ~/.config/nix/nix.conf
echo "experimental-features = nix-command flakes" >> ~/.config/nix/nix.conf
```

### 2. SSH key pair
The deployment uses an ed25519 key. Generate one if needed:
```bash
ssh-keygen -t ed25519 -f ~/.ssh/id_ed25519
```

### 3. DigitalOcean account setup
- Create a **Personal Access Token** (read+write) at: DigitalOcean → API → Tokens
- Upload your SSH public key to DigitalOcean under: Settings → Security → SSH Keys
  - Note the **name** you give it — you'll need it below (default expected: `st0x-op`)

### 4. Enter the Nix dev shell
All commands below must be run inside this shell:
```bash
nix develop
```

---

## Step 1 — Add your SSH key to `keys.nix`

Open `keys.nix` and add your SSH public key to the `keys` map and the relevant roles:

```nix
rec {
  keys = {
    your-name = "ssh-ed25519 AAAA...your-pubkey...";
    # ... existing keys
  };

  roles = {
    infra = [ keys.your-name keys.ci ];
    ssh   = [ keys.your-name keys.ci keys.arda ];
  };
}
```

This controls who can decrypt secrets (`infra` role) and SSH into the server (`ssh` role).

---

## Step 2 — Configure Terraform variables

The Terraform variables are stored encrypted. Use the helper to create/edit them:
```bash
nix develop -c tf-edit-vars
```

This opens your `$EDITOR` (defaults to `vi`) with the decrypted vars file.
Fill in the required value:

```hcl
do_token = "your-digitalocean-api-token"
```

Optional overrides (defaults shown):
```hcl
ssh_key_name   = "st0x-op"   # Name of your SSH key in DigitalOcean
region         = "nyc3"       # DigitalOcean region slug
droplet_size   = "s-2vcpu-4gb"
volume_size_gb = 5
```

Save and exit — the file is automatically re-encrypted with `rage` using the keys
in `keys.nix`. **Never commit the plaintext `infra/terraform.tfvars`.**

---

## Step 3 — Provision infrastructure with Terraform

```bash
# Initialize Terraform providers
nix develop -c tf-init

# Preview what will be created
nix develop -c tf-plan

# Apply — creates droplet, volume, and reserved IP
nix develop -c tf-apply
```

This provisions:
- Ubuntu 24.04 droplet (`st0x-rest-api-nixos`) in the chosen region
- 5 GB block storage volume (`st0x-rest-api-data`) mounted at `/mnt/data`
- A reserved IP attached to the droplet

The Terraform state is encrypted with `rage` and committed as
`infra/terraform.tfstate.age`.

---

## Step 4 — Bootstrap NixOS onto the droplet

The droplet boots Ubuntu. This step installs NixOS over it using `nixos-anywhere`:

```bash
nix develop -c bootstrap-nixos
```

This command will:
1. Resolve the droplet IP from the Terraform state
2. Run `nixos-anywhere` to partition the disk (via `disko.nix`) and install NixOS
3. Wait for the host to reboot
4. Read the new SSH host key from the server
5. **Automatically update `keys.nix`** with the real host key

After this step, commit the updated `keys.nix`:
```bash
git add keys.nix
git commit -m "chore: update host SSH key after bootstrap"
```

---

## Step 5 — Re-encrypt secrets with the host key

Now that the host key is in `keys.nix`, re-encrypt all secrets so the server
can decrypt them at runtime:

```bash
nix develop -c tf-rekey
```

Commit the re-encrypted secret files:
```bash
git add infra/terraform.tfvars.age infra/terraform.tfstate.age
git commit -m "chore: rekey secrets with new host key"
```

---

## Step 6 — Deploy the full stack

Deploy both the NixOS system config and the REST API service in one command:

```bash
nix develop -c deploy-all
```

Or deploy them separately:
```bash
# Deploy only the OS/system configuration
nix develop -c deploy-nixos

# Deploy only the REST API service binary
nix develop -c deploy-service rest-api
```

The deploy-rs workflow:
- Builds the Nix derivation locally (or cross-builds for non-Linux hosts)
- Copies the closure to the remote via SSH
- Activates the system profile / restarts the service

---

## Step 7 — DNS and TLS

1. Point your domain (`api.st0x.io` or your fork's domain) to the **reserved IP**
   output by Terraform:
   ```bash
   nix develop -c resolve-ip   # prints the reserved IP
   ```
2. Nginx is pre-configured to terminate TLS via Let's Encrypt (ACME).
   TLS certificates are issued automatically on first HTTP request to port 80.

Check the domain in `os.nix`:
```nix
virtualHosts."api.st0x.io" = { ... };
```
Update it to your domain before deploying if this is a fork.

Also update the ACME contact email:
```nix
security.acme.defaults.email = "ops@your-domain.io";
```

---

## Step 8 — Create an API key

SSH into the server and create the first API key:
```bash
nix develop -c remote   # opens an SSH session as root

# On the server:
/nix/var/nix/profiles/per-service/rest-api/bin/st0x_rest_api \
  keys create --config /nix/var/nix/profiles/per-service/rest-api/../../../... \
  --name "admin"
```

Or more practically, check the systemd service for the exact binary path and config:
```bash
systemctl cat rest-api
```

---

## Post-deployment — Ongoing operations

### Redeploy after code changes
```bash
nix develop -c deploy-service rest-api
```

### SSH into the server
```bash
nix develop -c remote
```

### View service logs
```bash
nix develop -c remote
# on server:
journalctl -u rest-api -f
# or log files:
ls /mnt/data/st0x-rest-api/logs/
```

### Check service status
```bash
nix develop -c remote
# on server:
systemctl status rest-api
```

### Tear down infrastructure
```bash
nix develop -c tf-destroy
```

---

## Architecture summary

```
Your machine
  └─ nix develop shell
       ├─ Terraform (infra/)   → DigitalOcean API → Droplet + Volume + Reserved IP
       ├─ nixos-anywhere        → SSH into droplet → Install NixOS
       └─ deploy-rs             → SSH into server  → Deploy system + service

Server (NixOS on DigitalOcean)
  ├─ Nginx (443)  → reverse proxy → Rocket API (127.0.0.1:8000)
  ├─ SQLite DB    → /mnt/data/st0x-rest-api/st0x.db
  ├─ Logs         → /mnt/data/st0x-rest-api/logs/
  └─ /mnt/data    → DigitalOcean block volume (persists across reboots)
```

---

## Environment variables

Set `RUST_LOG` to control log verbosity (configured in `.env`):
```
RUST_LOG=st0x_rest_api=info,rocket=warn,warn
```

This is read by the systemd service environment. To change it on the server,
update `os.nix` and redeploy with `deploy-nixos`.
