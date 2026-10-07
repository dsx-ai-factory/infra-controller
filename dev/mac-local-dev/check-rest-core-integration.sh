#!/bin/bash
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
# Local Core Integration Smoke Test
#
# Exercises the full API → carbide-core path:
#   1. Authenticate via Keycloak
#   2. Get (auto-create) the current Tenant
#   3. Retrieve the local-dev-site ID
#   3b. Force site to Registered via direct DB update (bypasses site-agent handshake)
#   4. Create an IP Block on the site
#   5. Create an Allocation (reserves the IP Block for the Tenant)
#   6. Create a VPC — this triggers a call into carbide-core and validates
#      the full integration with the local or mock core backend.
#
# Prerequisites:
#   - make kind-reset (with or without LOCAL_CORE=true) has completed
#   - jq is installed
#   - kubectl context is pointing at the kind cluster
#
# Override defaults via environment variables:
#   API_URL, KEYCLOAK_URL, ORG, SITE_NAME, PG_NAMESPACE, PG_STATEFULSET, PG_USER, PG_DB
#

set -euo pipefail

API_URL="${API_URL:-http://localhost:8388}"
KEYCLOAK_URL="${KEYCLOAK_URL:-http://localhost:8082}"
ORG="${ORG:-test-org}"
SITE_NAME="${SITE_NAME:-local-dev-site}"

# Postgres access via kubectl exec (no local psql or port-forward needed)
PG_NAMESPACE="${PG_NAMESPACE:-postgres}"
PG_STATEFULSET="${PG_STATEFULSET:-statefulset/postgres}"
PG_USER="${PG_USER:-nico}"
PG_DB="${PG_DB:-nico}"

BASE_URL="$API_URL/v2/org/$ORG/nico"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

step() { echo;          echo "──── $* ────"; } >&2
ok()   { echo "  ✓ $*";                     } >&2
info() { echo "    $*";                      } >&2
die()  { echo; echo "ERROR: $*"; exit 1;    } >&2

api_get() {
    local path="$1"
    local out http_code
    out=$(curl -s -w "\n%{http_code}" "$BASE_URL$path" \
        -H "Authorization: Bearer $TOKEN" \
        -H "Accept: application/json")
    http_code=$(echo "$out" | tail -n1)
    body=$(echo "$out" | sed '$d')
    if [[ "$http_code" -lt 200 || "$http_code" -ge 300 ]]; then
        die "GET $path → HTTP $http_code: $body"
    fi
    echo "$body"
}

api_post() {
    local path="$1"
    local payload="$2"
    local out http_code body existing_id
    out=$(curl -s -w "\n%{http_code}" -X POST "$BASE_URL$path" \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -H "Accept: application/json" \
        -d "$payload")
    http_code=$(echo "$out" | tail -n1)
    body=$(echo "$out" | sed '$d')
    if [[ "$http_code" == "409" ]]; then
        # Resource already exists; the conflict response carries the existing id
        # under .data.id — reuse it so the script is idempotent across runs.
        existing_id=$(echo "$body" | jq -r '.data.id // empty')
        if [[ -n "$existing_id" ]]; then
            info "Already exists (HTTP 409) — reusing id: $existing_id"
            echo "{\"id\":\"$existing_id\"}"
            return
        fi
        die "POST $path → HTTP 409 (conflict) but no id in response: $body"
    fi
    if [[ "$http_code" -lt 200 || "$http_code" -ge 300 ]]; then
        die "POST $path → HTTP $http_code: $body"
    fi
    echo "$body"
}

# Run a SQL statement inside the in-cluster postgres pod.
db_exec() {
    local sql="$1"
    kubectl exec -n "$PG_NAMESPACE" "$PG_STATEFULSET" -- \
        psql -U "$PG_USER" -d "$PG_DB" -tAc "$sql"
}

# ---------------------------------------------------------------------------
# 1. Authenticate
# ---------------------------------------------------------------------------

step "Authenticating via Keycloak"

TOKEN=$(curl -sf -X POST \
    "$KEYCLOAK_URL/realms/nico-dev/protocol/openid-connect/token" \
    -H "Content-Type: application/x-www-form-urlencoded" \
    -d "client_id=nico-api" \
    -d "client_secret=nico-local-secret" \
    -d "grant_type=password" \
    -d "username=admin@example.com" \
    -d "password=adminpassword" \
    | jq -r '.access_token')

[[ -z "$TOKEN" || "$TOKEN" == "null" ]] && die "Failed to acquire token from Keycloak"
ok "Token acquired (${#TOKEN} chars)"

# ---------------------------------------------------------------------------
# 2. Get current Tenant (auto-created on first call)
# ---------------------------------------------------------------------------

step "Getting current Tenant"

TENANT=$(api_get "/tenant/current")
TENANT_ID=$(echo "$TENANT" | jq -r '.id')
TENANT_ORG=$(echo "$TENANT" | jq -r '.org')

[[ -z "$TENANT_ID" || "$TENANT_ID" == "null" ]] && die "Could not retrieve tenant ID"

ok "Tenant: $TENANT_ORG  (id: $TENANT_ID)"

# ---------------------------------------------------------------------------
# 3. Retrieve the site ID
# ---------------------------------------------------------------------------

step "Retrieving site '$SITE_NAME'"

SITES=$(api_get "/site")
SITE_ID=$(echo "$SITES" | jq -r --arg name "$SITE_NAME" '.[] | select(.name == $name) | .id')
SITE_STATUS=$(echo "$SITES" | jq -r --arg name "$SITE_NAME" '.[] | select(.name == $name) | .status')

[[ -z "$SITE_ID" || "$SITE_ID" == "null" ]] \
    && die "Site '$SITE_NAME' not found. Has 'make kind-reset' completed?"

ok "Site id:     $SITE_ID"
ok "Site status: $SITE_STATUS"

# ---------------------------------------------------------------------------
# 3b. Force site to Registered via direct DB update
# ---------------------------------------------------------------------------

step "Forcing site status to Registered (direct DB update)"

if [[ "$SITE_STATUS" == "Registered" ]]; then
    ok "Site is already Registered — skipping DB update"
else
    info "Current status: '$SITE_STATUS' → updating to 'Registered' in postgres"

    # psql prints the command tag "UPDATE n" to stdout; capture it to verify rows matched.
    UPDATE_TAG=$(kubectl exec -n "$PG_NAMESPACE" "$PG_STATEFULSET" -- \
        psql -U "$PG_USER" -d "$PG_DB" -c \
        "UPDATE site SET status = 'Registered', updated = NOW() WHERE id = '$SITE_ID';" \
        2>&1 | grep -E '^UPDATE' || true)

    [[ -z "$UPDATE_TAG" ]] && die "DB update produced no UPDATE tag — check site ID and postgres connectivity"
    UPDATED_ROWS=$(echo "$UPDATE_TAG" | awk '{print $2}')
    [[ "$UPDATED_ROWS" -eq 0 ]] && die "DB update matched 0 rows — site id '$SITE_ID' not found in table"

    ok "Updated $UPDATED_ROWS row(s)"

    # Confirm via API
    SITE_STATUS=$(api_get "/site/$SITE_ID" | jq -r '.status')
    ok "Site status now: $SITE_STATUS"
    [[ "$SITE_STATUS" != "Registered" ]] \
        && info "Warning: API still shows '$SITE_STATUS' — the API may cache status; proceeding anyway"
fi

# ---------------------------------------------------------------------------
# 3c. Enable ImageBasedOperatingSystem capability (direct DB update)
# ---------------------------------------------------------------------------

step "Enabling ImageBasedOperatingSystem capability (direct DB update)"

SITE_CAPS=$(db_exec "SELECT config->>'image_based_operating_system' FROM site WHERE id = '$SITE_ID';")

if [[ "$SITE_CAPS" == "true" ]]; then
    ok "ImageBasedOperatingSystem is already enabled — skipping DB update"
else
    info "Enabling image_based_operating_system in site config"

    UPDATE_TAG=$(kubectl exec -n "$PG_NAMESPACE" "$PG_STATEFULSET" -- \
        psql -U "$PG_USER" -d "$PG_DB" -c \
        "UPDATE site SET config = jsonb_set(COALESCE(config, '{}'), '{image_based_operating_system}', 'true'), updated = NOW() WHERE id = '$SITE_ID';" \
        2>&1 | grep -E '^UPDATE' || true)

    [[ -z "$UPDATE_TAG" ]] && die "DB update produced no UPDATE tag — check site ID and postgres connectivity"
    UPDATED_ROWS=$(echo "$UPDATE_TAG" | awk '{print $2}')
    [[ "$UPDATED_ROWS" -eq 0 ]] && die "DB update matched 0 rows — site id '$SITE_ID' not found in table"

    ok "Updated $UPDATED_ROWS row(s)"
fi

# ---------------------------------------------------------------------------
# 4. Create an IP Block
# ---------------------------------------------------------------------------

step "Creating IP Block"

IPBLOCK=$(api_post "/ipblock" "$(jq -n \
    --arg site "$SITE_ID" '{
        name:            "smoke-test-ipblock",
        description:     "IP block created by test-local-core.sh",
        siteId:          $site,
        routingType:     "DatacenterOnly",
        prefix:          "10.100.0.0",
        prefixLength:    24,
        protocolVersion: "IPv4"
    }')")

IPBLOCK_ID=$(echo "$IPBLOCK" | jq -r '.id')
[[ -z "$IPBLOCK_ID" || "$IPBLOCK_ID" == "null" ]] && die "IP Block creation failed"

ok "IP Block id: $IPBLOCK_ID  (10.100.0.0/24)"

# ---------------------------------------------------------------------------
# 5. Create an Allocation for the IP Block
# ---------------------------------------------------------------------------

step "Creating Allocation"

ALLOCATION=$(api_post "/allocation" "$(jq -n \
    --arg tenant "$TENANT_ID" \
    --arg site   "$SITE_ID" \
    --arg ipb    "$IPBLOCK_ID" '{
        name:        "smoke-test-allocation",
        description: "Allocation created by test-local-core.sh",
        tenantId:    $tenant,
        siteId:      $site,
        allocationConstraints: [{
            resourceType:     "IPBlock",
            resourceTypeId:   $ipb,
            constraintType:   "Reserved",
            constraintValue:  28
        }]
    }')")

ALLOCATION_ID=$(echo "$ALLOCATION" | jq -r '.id')
[[ -z "$ALLOCATION_ID" || "$ALLOCATION_ID" == "null" ]] && die "Allocation creation failed"

ok "Allocation id: $ALLOCATION_ID"

# ---------------------------------------------------------------------------
# 6. Create a VPC  (calls into carbide-core)
# ---------------------------------------------------------------------------

step "Creating VPC  ← this calls carbide-core"

VPC=$(api_post "/vpc" "$(jq -n \
    --arg site "$SITE_ID" '{
        name:        "smoke-test-vpc",
        description: "VPC created by test-local-core.sh",
        siteId:      $site
    }')")

VPC_ID=$(echo "$VPC" | jq -r '.id')
VPC_STATUS=$(echo "$VPC" | jq -r '.status // "unknown"')

[[ -z "$VPC_ID" || "$VPC_ID" == "null" ]] && die "VPC creation failed"

ok "VPC id:     $VPC_ID"
ok "VPC status: $VPC_STATUS"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo
echo "════════════════════════════════════════════════════════════"
echo " Smoke test passed"
echo "════════════════════════════════════════════════════════════"
echo " Tenant:     $TENANT_ID"
echo " Site:       $SITE_ID  (Registered)"
echo " IP Block:   $IPBLOCK_ID"
echo " Allocation: $ALLOCATION_ID"
echo " VPC:        $VPC_ID  ($VPC_STATUS)"
echo "════════════════════════════════════════════════════════════"
