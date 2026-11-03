#!/usr/bin/env bash
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

# Fetches a Keycloak access token with the client_credentials grant and prints it to
# stdout. Nothing else.
#
# Defaults to the ncx-service client the bundled realm ships, which is the Provider org.
# A token only carries the orgs its client's service account holds roles for, so acting
# as a Tenant needs that Tenant's own client here, not ncx-service.
#
# Usage:
#   ./get-token.sh
#   TOKEN=$(./get-token.sh)
#   KEYCLOAK_CLIENT_ID=acme-corp-service KEYCLOAK_CLIENT_SECRET=... ./get-token.sh
set -euo pipefail

NS="${KEYCLOAK_NS:-nico-rest}"
REALM="${KEYCLOAK_REALM:-nico}"
CLIENT_ID="${KEYCLOAK_CLIENT_ID:-ncx-service}"
CLIENT_SECRET="${KEYCLOAK_CLIENT_SECRET:-nico-local-secret}"
KC_URL="http://keycloak.${NS}:8082"
TOKEN_URL="${KC_URL}/realms/${REALM}/protocol/openid-connect/token"

command -v python3 >/dev/null || { echo "ERROR: python3 is required" >&2; exit 1; }

# The secret reaches python through its environment and curl through a stdin config
# file, so it never appears in a process argument list, the pod spec, Kubernetes audit
# records, or shell history. Form-encoding it here keeps a raw &, =, + or % in a
# Tenant's secret from changing the value Keycloak receives.
_curl_config() {
    KC_CLIENT_ID="${CLIENT_ID}" KC_CLIENT_SECRET="${CLIENT_SECRET}" python3 - <<'PY'
import os, urllib.parse
print('data = "%s"' % urllib.parse.urlencode({
    "grant_type": "client_credentials",
    "client_id": os.environ["KC_CLIENT_ID"],
    "client_secret": os.environ["KC_CLIENT_SECRET"],
}))
PY
}

# Runs curl from inside the cluster via a one-shot pod.
# This ensures JWT issuer matches the internal Keycloak URL.
# Keep the response body for the allowlisted OAuth code, but never print it on
# failure: an endpoint can echo credentials even in its error identifier.
RESPONSE="$(_curl_config \
    | kubectl run -i --rm --restart=Never --image=curlimages/curl "curl-$$" \
        -n "${NS}" --quiet -- -s -K - -w $'\n%{http_code}' "${TOKEN_URL}")" \
    || { echo "ERROR: could not reach Keycloak at ${TOKEN_URL}" >&2; exit 1; }

HTTP_STATUS="${RESPONSE##*$'\n'}"
if [[ ! "${HTTP_STATUS}" =~ ^[0-9]{3}$ ]]; then
    echo "ERROR: could not read Keycloak HTTP status" >&2
    exit 1
fi
RESPONSE="${RESPONSE%$'\n'*}"

printf '%s' "${RESPONSE}" | python3 -c '
import json, sys
status = sys.argv[1]
try:
    response = json.load(sys.stdin)
except ValueError:
    response = {}
if not isinstance(response, dict):
    response = {}
token = response.get("access_token")
if status == "200" and "error" not in response and isinstance(token, str) and token:
    print(token)
    sys.exit(0)

# Match the standard OAuth code allowlist in rest-api/cli/pkg/auth.go.
hints = {
    "invalid_request": "check the token request parameters",
    "invalid_client": "check that the client exists in the realm and its secret matches",
    "invalid_grant": "check the grant credentials and account setup",
    "unauthorized_client": "check that the client is allowed to use this grant",
    "unsupported_grant_type": "check that the realm supports the requested grant",
    "invalid_scope": "check the requested scopes and client permissions",
    "server_error": "check Keycloak availability and server logs",
    "temporarily_unavailable": "retry when Keycloak is available",
}
code = response.get("error")
if not isinstance(code, str) or code not in hints:
    code = ""
hint = hints.get(code, "check the client, realm, and token endpoint configuration")
detail = ", " + code if code else ""
print(f"ERROR: token request failed (HTTP {status}{detail}); {hint}", file=sys.stderr)
sys.exit(1)
' "${HTTP_STATUS}"
