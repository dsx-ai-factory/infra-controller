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

# The secret goes to curl in a stdin config file rather than -d, so it stays out of the
# pod spec, Kubernetes audit records, and shell history. Form-encoding it here keeps a
# raw &, =, + or % in a Tenant's secret from changing the value Keycloak receives.
_curl_config() {
    python3 - "$@" <<'PY'
import sys, urllib.parse
print('data = "%s"' % urllib.parse.urlencode({
    "grant_type": "client_credentials",
    "client_id": sys.argv[1],
    "client_secret": sys.argv[2],
}))
PY
}

# Runs curl from inside the cluster via a one-shot pod.
# This ensures JWT issuer matches the internal Keycloak URL.
# Deliberately omits -f: on a bad client or secret Keycloak returns the reason in the
# body, and -f would discard it and leave only a nonzero exit.
RESPONSE="$(_curl_config "${CLIENT_ID}" "${CLIENT_SECRET}" \
    | kubectl run -i --rm --restart=Never --image=curlimages/curl "curl-$$" \
        -n "${NS}" --quiet -- -s -K - "${TOKEN_URL}")" \
    || { echo "ERROR: could not reach Keycloak at ${TOKEN_URL}" >&2; exit 1; }

ACCESS_TOKEN="$(printf '%s' "${RESPONSE}" | python3 -c '
import json, sys
try:
    print(json.load(sys.stdin).get("access_token", ""))
except ValueError:
    pass
')"

if [[ -z "${ACCESS_TOKEN}" ]]; then
    echo "ERROR: no access_token for client ${CLIENT_ID} in realm ${REALM}" >&2
    echo "  Keycloak said: ${RESPONSE}" >&2
    echo "  invalid_client means the client does not exist in the realm, or its secret differs." >&2
    echo "  List the clients that do exist with:" >&2
    echo "    kubectl -n ${NS} exec deployment/keycloak -- \\" >&2
    echo "      /opt/keycloak/bin/kcadm.sh get clients -r ${REALM} --fields clientId" >&2
    exit 1
fi

echo "${ACCESS_TOKEN}"
