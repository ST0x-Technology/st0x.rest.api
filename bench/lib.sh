#!/usr/bin/env bash
# Shared helpers for the bench harness. Source, do not execute.

set -euo pipefail

require_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "ERROR: '$cmd' not found in PATH." >&2
    case "$cmd" in
      oha) echo "Install: cargo install oha   (or use nix develop)" >&2 ;;
      jq)  echo "Install: brew install jq | apt install jq | nix develop" >&2 ;;
    esac
    exit 1
  fi
}

load_env() {
  local env_file="${1:-bench/.env}"
  if [ -f "$env_file" ]; then
    set -a
    # shellcheck disable=SC1090
    . "$env_file"
    set +a
  fi
}

# Echo a base64-encoded "user:pass" suitable for HTTP Basic auth.
# Empty string if either is unset (caller decides how to handle).
basic_b64() {
  local user="${1:-}" pass="${2:-}"
  if [ -z "$user" ] || [ -z "$pass" ]; then
    echo ""
    return
  fi
  printf '%s:%s' "$user" "$pass" | base64 | tr -d '\n'
}

# Resolve {placeholders} in a string using a name=value file.
# Usage: resolve_placeholders "string with {x}" path/to/discovered.env
resolve_placeholders() {
  local s="$1" env_file="$2"
  if [ ! -f "$env_file" ]; then
    echo "$s"
    return
  fi
  while IFS='=' read -r k v; do
    [ -z "$k" ] && continue
    s="${s//\{$k\}/$v}"
  done < "$env_file"
  echo "$s"
}
