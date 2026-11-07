#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -eEuo pipefail

# Optional: enable verbose debug
[[ -n "${DEBUG:-}" ]] && set -xv

banner() {
  printf '\n\033[1;36m%s\033[0m\n' "$*"
}

# ------------------------------------------------------------------------------
# Config
# ------------------------------------------------------------------------------

# Optional: set KUBECTL_CONTEXT env if you want a specific context:
#   KUBECTL_CONTEXT=my-context ./create-postgres-extensions.sh
KUBECTL_CONTEXT="${KUBECTL_CONTEXT:-}"

# Build kubectl command with optional context
KUBECTL=(kubectl)
if [[ -n "$KUBECTL_CONTEXT" ]]; then
  KUBECTL+=(--context "$KUBECTL_CONTEXT")
fi

# DB_NAME: from env if provided, otherwise from cloud-db-config, fallback to "nico"
if [[ -z "${DB_NAME:-}" ]]; then
  DB_NAME="$("${KUBECTL[@]}" -n cloud-db get configmap cloud-db-config \
    -o jsonpath='{.data.dbName}' 2>/dev/null || true)"
  DB_NAME="${DB_NAME:-nico}"
fi

banner "📦 Using database name: ${DB_NAME}"

# ------------------------------------------------------------------------------
# Wait for Postgres StatefulSet to be Ready
# ------------------------------------------------------------------------------

banner "⏳  Waiting for StatefulSet postgres/nico-pg-cluster replicas to be Ready…"

for i in {1..120}; do
  READY="$("${KUBECTL[@]}" -n postgres get sts nico-pg-cluster \
    -o jsonpath='{.status.readyReplicas}' 2>/dev/null || true)"
  TOTAL="$("${KUBECTL[@]}" -n postgres get sts nico-pg-cluster \
    -o jsonpath='{.status.replicas}' 2>/dev/null || true)"

  if [[ -n "$TOTAL" && "$READY" == "$TOTAL" && "$TOTAL" -gt 0 ]]; then
    banner "✅  Postgres StatefulSet is Ready (${READY}/${TOTAL})"
    break
  fi

  if [[ "$i" == 120 ]]; then
    echo "❌  StatefulSet not Ready after 10 minutes"
    exit 2
  fi

  sleep 5
done

# ------------------------------------------------------------------------------
# Find master pod and run CREATE EXTENSION inside it
# ------------------------------------------------------------------------------

MASTER_POD="$("${KUBECTL[@]}" -n postgres get pods \
  -l spilo-role=master \
  -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)"

if [[ -z "$MASTER_POD" ]]; then
  echo "❌  Could not locate master pod (label spilo-role=master)"
  exit 2
fi

echo "🔑  Running extension SQL inside pod ${MASTER_POD}"

"${KUBECTL[@]}" -n postgres exec "${MASTER_POD}" -- \
  psql -U postgres -d "${DB_NAME}" \
    -c 'CREATE EXTENSION IF NOT EXISTS btree_gin;' \
    -c 'CREATE EXTENSION IF NOT EXISTS pg_trgm;'

echo "✅  Postgres extensions ensured."
