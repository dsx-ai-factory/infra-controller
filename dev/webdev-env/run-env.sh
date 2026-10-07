#!/bin/bash
#
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#
#
# Carbide UI development
#
# Usage: ./run-env.sh /path/to/pg_dump.sql
#

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PG_PORT=5433
PID=""

cleanup() {
    echo -e "\nStopping containers..."
    [[ -n "$PID" ]] && kill $PID 2>/dev/null
    docker rm -f vault-webdev pg-webdev >/dev/null 2>&1 || true
    echo "Done."
    exit 0
}
trap cleanup SIGINT

[[ -z "$1" ]] && { echo "Usage: $0 /path/to/pg_dump.sql"; exit 1; }
[[ ! -f "$1" ]] && { echo "Error: File not found: $1"; exit 1; }
PG_DUMP_FILE="$1"

if ! docker ps --filter name=vault-webdev --format '{{.Names}}' | grep -q vault-webdev || \
   ! docker ps --filter name=pg-webdev --format '{{.Names}}' | grep -q pg-webdev; then
    echo "Setting up containers..."
    docker rm -f vault-webdev pg-webdev >/dev/null 2>&1 || true

    # Vault
    docker run --rm -d --name vault-webdev --cap-add=IPC_LOCK \
        -e 'VAULT_LOCAL_CONFIG={"storage": {"file": {"path": "/vault/file"}}, "listener": [{"tcp": { "address": "0.0.0.0:8200", "tls_disable": true}}], "default_lease_ttl": "168h", "max_lease_ttl": "720h", "ui": true}' \
        -p 8200:8200 hashicorp/vault server >/dev/null
    sleep 2
    VAULT_INIT="$(docker exec vault-webdev sh -c 'export VAULT_ADDR="http://127.0.0.1:8200" && vault operator init -key-shares=1 -key-threshold=1 -format=json' 2>/dev/null)"
    export VAULT_TOKEN="$(echo "$VAULT_INIT" | jq -r '.root_token')"
    export VAULT_ADDR="http://127.0.0.1:8200"
    vault operator unseal "$(echo "$VAULT_INIT" | jq -r '.unseal_keys_b64[0]')" >/dev/null
    vault login $VAULT_TOKEN >/dev/null 2>&1

    # Postgres
    docker run --rm -d --name pg-webdev -p ${PG_PORT}:5432 \
        -e POSTGRES_PASSWORD=admin -e POSTGRES_HOST_AUTH_METHOD=trust \
        postgres:14.5-alpine -c max_connections=300 >/dev/null
    sleep 3
    for _ in {1..30}; do docker exec pg-webdev pg_isready -U postgres >/dev/null 2>&1 && break; sleep 1; done
    docker exec -i pg-webdev psql -U postgres < "$PG_DUMP_FILE" >/dev/null 2>&1
    echo "Setup complete."
else
    echo "Containers already running."
    export VAULT_ADDR="http://127.0.0.1:8200"
    export VAULT_TOKEN="$(docker exec vault-webdev cat /root/.vault-token 2>/dev/null)"
fi

export DATABASE_URL="postgresql://postgres:admin@localhost:${PG_PORT}?sslmode=disable"
export DISABLE_TLS_ENFORCEMENT=1
export VAULT_KV_MOUNT_LOCATION="secrets"
export VAULT_PKI_MOUNT_LOCATION="certs"
export VAULT_PKI_ROLE_NAME="role"
export CARBIDE_WEB_AUTH_TYPE="none"
# This local-only environment explicitly disables auth. Deployed WebUIs default to Basic.

# Run SQL migrations
echo "Running database migrations..."
cargo run --package carbide-api --no-default-features -- migrate

menu() {
    clear
    cat << 'EOF'

  ┌─────────────────────────────────────────┐
  │  http://localhost:1079/admin/           │
  │  No auth (local development only)       │
  │  Deployed WebUIs default to Basic       │
  │                                         │
  │  Templates: crates/api/templates/       │
  │                                         │
  │  [r] Rebuild    [q] Quit                │
  └─────────────────────────────────────────┘

EOF
}

wait_for_key() {
    while read -rsn1 key; do
        case "$key" in
            r|R) return 0 ;;
            q|Q) cleanup ;;
        esac
    done
}

# Main loop
menu
echo "  Building..."

while true; do
    # Build Rust
    if cargo build --package carbide-api --no-default-features; then
        echo -e "\n  ✓ Build successful! Starting server..."
        cargo run --package carbide-api --no-default-features -- run \
            --config-path "${SCRIPT_DIR}/carbide-api-config.toml" >/dev/null 2>&1 &
        PID=$!
        sleep 6
        menu
        wait_for_key
        kill $PID 2>/dev/null; wait $PID 2>/dev/null || true
    else
        echo -e "\n  ✗ Build failed! Press r to retry, q to quit..."
        wait_for_key
    fi
    echo "  Rebuilding..."
done
