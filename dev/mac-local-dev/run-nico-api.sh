#!/usr/bin/env bash
#
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Self-contained script to start nico-api on macOS.
# Run from the repository root: ./dev/mac-local-dev/run-nico-api.sh
#
# Prerequisites: Docker Desktop, Rust toolchain, jq
#

set -euo pipefail

# -----------------------------------------------------------------------------
# Configuration
# -----------------------------------------------------------------------------
VAULT_CONTAINER="nico-vault"
VAULT_PORT=8201
VAULT_ADDR="http://localhost:$VAULT_PORT"
TOKEN_FILE="/tmp/nico-localdev-vault-root-token"
PG_CONTAINER="pgdev"

# -----------------------------------------------------------------------------
# Helpers
# -----------------------------------------------------------------------------
die() {
  echo "❌ $*" >&2
  exit 1
}

info() {
  echo "ℹ️  $*"
}

ok() {
  echo "✓ $*"
}

# Ensure we're in the repo root
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

# -----------------------------------------------------------------------------
# Prerequisites
# -----------------------------------------------------------------------------
echo ""
echo "=== Nico API - Mac Local Development ==="
echo ""

for cmd in docker cargo jq curl; do
  command -v "$cmd" >/dev/null 2>&1 || die "$cmd not found. Please install it."
done
ok "Required binaries: docker, cargo, jq, curl"

if ! docker ps >/dev/null 2>&1; then
  die "Docker is not running. Start Docker Desktop."
fi
ok "Docker is running"

# -----------------------------------------------------------------------------
# Start Vault (port 8201 - dedicated for nico, avoids kind cluster conflict)
# -----------------------------------------------------------------------------
if docker ps --format '{{.Names}}' | grep -w "$VAULT_CONTAINER" >/dev/null; then
  ok "Vault container already running"
  [ -f "$TOKEN_FILE" ] || die "Token file missing. Remove container and retry: docker rm -f $VAULT_CONTAINER"
  chmod 600 "$TOKEN_FILE"
else
  info "Starting Vault on port $VAULT_PORT..."
  docker rm -f "$VAULT_CONTAINER" 2>/dev/null || true

  docker run --rm --detach --name "$VAULT_CONTAINER" --cap-add=IPC_LOCK \
    -e 'VAULT_LOCAL_CONFIG={"storage": {"file": {"path": "/vault/file"}}, "listener": [{"tcp": { "address": "0.0.0.0:8200", "tls_disable": true}}], "default_lease_ttl": "168h", "max_lease_ttl": "720h", "ui": true}' \
    -p "$VAULT_PORT:8200" hashicorp/vault:1.20.2 server >/dev/null 2>&1 || die "Failed to start vault"

  echo "Waiting for vault..."
  sleep 2
  until curl -s "$VAULT_ADDR/v1/sys/health" >/dev/null 2>&1; do sleep 1; done

  info "Initializing vault..."
  INIT=$(docker exec "$VAULT_CONTAINER" sh -c "export VAULT_ADDR=http://127.0.0.1:8200; vault operator init -key-shares=1 -key-threshold=1 -format=json")
  UNSEAL_KEY=$(echo "$INIT" | jq -r ".unseal_keys_b64[0]")
  ROOT_TOKEN=$(echo "$INIT" | jq -r ".root_token")
  (umask 077 && echo "$ROOT_TOKEN" > "$TOKEN_FILE")

  info "Configuring vault secrets..."
  docker exec "$VAULT_CONTAINER" sh -c "
    export VAULT_ADDR=http://127.0.0.1:8200
    vault operator unseal \"$UNSEAL_KEY\"
    vault login \"$ROOT_TOKEN\"
    vault secrets enable -path=secrets -version=2 kv
    echo '{\"UsernamePassword\": {\"username\": \"root\", \"password\": \"vault-password\" }}' | vault kv put /secrets/machines/bmc/site/root -
    echo '{\"UsernamePassword\": {\"username\": \"root\", \"password\": \"vault-password\" }}' | vault kv put /secrets/machines/all_dpus/site_default/uefi-metadata-items/auth -
    echo '{\"UsernamePassword\": {\"username\": \"root\", \"password\": \"vault-password\" }}' | vault kv put /secrets/machines/all_hosts/site_default/uefi-metadata-items/auth -
    vault secrets enable -path=certs pki
    vault write certs/root/generate/internal common_name=myvault.com ttl=87600h
    vault write certs/config/urls issuing_certificates=\"http://vault.example.com:8200/v1/pki/ca\" crl_distribution_points=\"http://vault.example.com:8200/v1/pki/crl\"
    vault write certs/roles/role allowed_domains=example.com allow_subdomains=true max_ttl=72h require_cn=false allowed_uri_sans=\"spiffe://nico.local/*\"
  " >/dev/null 2>&1

  ok "Vault initialized at $VAULT_ADDR"
fi

# -----------------------------------------------------------------------------
# TLS certificates
# Runs unconditionally; gen-certs.sh is idempotent (skips files that exist
# and are newer than their signing key), so this is fast on subsequent runs.
# -----------------------------------------------------------------------------
info "Ensuring TLS certificates are up to date..."
(cd "$REPO_ROOT/dev/certs/localhost" && ./gen-certs.sh) >/dev/null 2>&1
ok "TLS certificates ready"

# -----------------------------------------------------------------------------
# Start Postgres
# -----------------------------------------------------------------------------
if docker ps --format '{{.Names}}' | grep -w "$PG_CONTAINER" >/dev/null; then
  ok "Postgres already running"
else
  CERTS_DIR="$REPO_ROOT/dev/certs/localhost"
  info "Starting Postgres..."
  docker run --rm --detach --name "$PG_CONTAINER" \
    -e POSTGRES_PASSWORD="admin" \
    -e POSTGRES_HOST_AUTH_METHOD=trust \
    -v "$CERTS_DIR/localhost.crt:/var/lib/postgresql/server.crt:ro" \
    -v "$CERTS_DIR/localhost.key:/var/lib/postgresql/server.key:ro" \
    -p 5432:5432 \
    postgres:14.5-alpine \
    -c ssl=on \
    -c ssl_cert_file=/var/lib/postgresql/server.crt \
    -c ssl_key_file=/var/lib/postgresql/server.key \
    -c max_connections=300 >/dev/null 2>&1 || die "Failed to start postgres"

  sleep 2
  ok "Postgres started"
fi

# -----------------------------------------------------------------------------
# Environment
# -----------------------------------------------------------------------------
# Local development explicitly disables auth; production/Helm defaults to Basic.
# api-web currently reads CARBIDE_WEB_AUTH_TYPE; keep both vars in sync during rename.
WEB_AUTH_TYPE="${CARBIDE_WEB_AUTH_TYPE:-${NICO_WEB_AUTH_TYPE:-none}}"
export NICO_WEB_AUTH_TYPE="$WEB_AUTH_TYPE"
export CARBIDE_WEB_AUTH_TYPE="$WEB_AUTH_TYPE"
export DATABASE_URL="postgresql://postgres:admin@localhost"
export VAULT_ADDR="$VAULT_ADDR"
# Vault runs without TLS in local dev (HTTP). The code requires VAULT_CACERT to
# point to an existing file; for HTTP connections the cert is never actually used.
export VAULT_CACERT="$REPO_ROOT/dev/certs/localhost/ca.crt"
export VAULT_KV_MOUNT_LOCATION="secrets"
export VAULT_PKI_MOUNT_LOCATION="certs"
export VAULT_PKI_ROLE_NAME="role"
export VAULT_TOKEN="$(cat "$TOKEN_FILE")"

# -----------------------------------------------------------------------------
# Firmware directory (nico expects this)
# -----------------------------------------------------------------------------
if [ ! -d /opt/carbide/firmware ]; then
  info "Creating /opt/carbide/firmware (may prompt for password)..."
  sudo mkdir -p /opt/carbide/firmware
fi

# -----------------------------------------------------------------------------
# Generate a resolved config with absolute TLS paths
#
# nico-api opens TLS paths relative to the process working directory, not
# relative to the config file.  The checked-in config uses relative paths
# (e.g. "dev/certs/…") which only work when CWD == repo root.  When launched
# from an IDE or any other directory the cert load will silently fail.
# We rewrite those paths to absolute ones in a throwaway /tmp copy so the
# binary is always given correct paths regardless of CWD.
# -----------------------------------------------------------------------------
# Create the config inside a private temp directory rather than naming the file
# directly. macOS's BSD mktemp only substitutes *trailing* X's, so a template
# like "nico-api-config.XXXXXX.toml" would be taken literally and collide across
# runs. mktemp -d gives us a uniquely-named directory (portable across BSD/GNU),
# and we place a fixed-name .toml file inside it — no rename, so no chance of
# clobbering an unrelated file.
NICO_TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/nico-api-config.XXXXXX")" || die "Failed to create temporary config directory"
NICO_TMP_CONFIG="$NICO_TMP_DIR/nico-api-config.toml"
trap 'rm -rf "$NICO_TMP_DIR"' EXIT
sed "s|= \"dev/|= \"$REPO_ROOT/dev/|g" \
  "$REPO_ROOT/dev/mac-local-dev/nico-api-config.toml" > "$NICO_TMP_CONFIG"
ok "Resolved config written to $NICO_TMP_CONFIG"

# -----------------------------------------------------------------------------
# Migrations & Run
# -----------------------------------------------------------------------------
echo ""
echo "=== Running migrations ==="
cargo run --package carbide-api --no-default-features migrate || die "Database migrations failed; fix the issue above and re-run this script."

echo ""
echo "=== Starting Nico API ==="
info "TPM/attestation features are not supported on Mac (requires Linux + TPM)."
echo "   All other functionality is available."
echo ""
echo "   Web UI: https://localhost:1079/admin"
echo "   gRPC:   grpcurl -insecure localhost:1079 list"
echo ""

exec env RUST_BACKTRACE=1 cargo run --package carbide-api --no-default-features -- run \
  --config-path "$NICO_TMP_CONFIG"
