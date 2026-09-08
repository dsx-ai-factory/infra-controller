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

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NS="${KEYCLOAK_NS:-nico-rest}"

# KEYCLOAK_DB_* — target database for Keycloak. Defaults to the legacy
# standalone postgres.postgres StatefulSet. setup.sh exports these pointing
# at nico-pg-cluster instead when helm-prereqs/values.yaml::keycloak.useHaPostgres
# is true (see phase 7d), along with KEYCLOAK_DB_PASSWORD_SECRET_NAME/KEY
# pointing at the ESO-synced nico-keycloak-pg-creds Secret.
: "${KEYCLOAK_DB_HOST:=postgres.postgres}"
: "${KEYCLOAK_DB_NAME:=keycloak}"
: "${KEYCLOAK_DB_USER:=keycloak}"
export KEYCLOAK_DB_HOST KEYCLOAK_DB_NAME KEYCLOAK_DB_USER

# KEYCLOAK_DB_SSLMODE — postgres.postgres never terminates TLS, so PgJDBC's
# default sslmode ("prefer") would silently connect in plaintext there;
# "disable" makes that explicit instead of implicit. nico-pg-cluster's
# pg_hba.conf requires TLS, so setup.sh's opt-in path exports "require" here.
# Neither mode validates the server certificate/hostname (that needs a
# truststore wired to the operator's CA, not done here) — "require" is
# encrypted-but-unauthenticated, an improvement over the legacy path's plain
# TLS negotiation, not full verification.
: "${KEYCLOAK_DB_SSLMODE:=disable}"
export KEYCLOAK_DB_SSLMODE

kubectl create namespace "${NS}" 2>/dev/null || true

if [[ "${KEYCLOAK_DB_HOST}" == "postgres.postgres" ]]; then
    echo "  Ensuring keycloak database (legacy postgres.postgres)..."
    _PG_POD="$(kubectl get pods -n postgres -l app=postgres \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)"

    if [[ -n "${_PG_POD}" ]]; then
        kubectl exec -n postgres "${_PG_POD}" -- \
            psql -U postgres -c "CREATE DATABASE keycloak;" 2>/dev/null || true
        kubectl exec -n postgres "${_PG_POD}" -- \
            psql -U postgres -c "CREATE USER keycloak WITH ENCRYPTED PASSWORD 'keycloak';" 2>/dev/null || true
        kubectl exec -n postgres "${_PG_POD}" -- \
            psql -U postgres -c "ALTER USER keycloak WITH ENCRYPTED PASSWORD 'keycloak';"
        kubectl exec -n postgres "${_PG_POD}" -- \
            psql -U postgres -c "GRANT ALL PRIVILEGES ON DATABASE keycloak TO keycloak;"
        kubectl exec -n postgres "${_PG_POD}" -- \
            psql -U postgres -d keycloak -c "GRANT ALL ON SCHEMA public TO keycloak;"
    else
        echo "  WARNING: no postgres pod found — ensure keycloak DB exists"
    fi

    # deployment.yaml's KC_DB_PASSWORD always reads from a Secret (see
    # KEYCLOAK_DB_PASSWORD_SECRET_NAME/KEY below) — provision one here for
    # the legacy dev password rather than embedding it as a plaintext env value.
    : "${KEYCLOAK_DB_PASSWORD_SECRET_NAME:=keycloak-legacy-db-creds}"
    : "${KEYCLOAK_DB_PASSWORD_SECRET_KEY:=password}"
    kubectl create secret generic "${KEYCLOAK_DB_PASSWORD_SECRET_NAME}" -n "${NS}" \
        --from-literal="${KEYCLOAK_DB_PASSWORD_SECRET_KEY}=keycloak" \
        --dry-run=client -o yaml | kubectl apply -f -
else
    # nico-pg-cluster path: the Zalando operator already created the
    # keycloak.nico user and keycloak database (helm-prereqs/templates/postgresql.yaml,
    # gated on keycloak.useHaPostgres) — nothing to provision here. setup.sh phase
    # 7d already exported KEYCLOAK_DB_PASSWORD_SECRET_NAME/KEY pointing at
    # the ESO-synced nico-keycloak-pg-creds Secret in this namespace.
    echo "  Using nico-pg-cluster (${KEYCLOAK_DB_HOST}/${KEYCLOAK_DB_NAME}) — database already provisioned by the postgres operator"
    : "${KEYCLOAK_DB_PASSWORD_SECRET_NAME:?KEYCLOAK_DB_PASSWORD_SECRET_NAME must be set by setup.sh phase 7d when KEYCLOAK_DB_HOST points at nico-pg-cluster}"
    : "${KEYCLOAK_DB_PASSWORD_SECRET_KEY:?KEYCLOAK_DB_PASSWORD_SECRET_KEY must be set by setup.sh phase 7d when KEYCLOAK_DB_HOST points at nico-pg-cluster}"
fi
export KEYCLOAK_DB_PASSWORD_SECRET_NAME KEYCLOAK_DB_PASSWORD_SECRET_KEY

echo "  Deploying Keycloak (quay.io/keycloak/keycloak:24.0)..."
kubectl apply -n "${NS}" \
    -f "${SCRIPT_DIR}/realm-configmap.yaml" \
    -f "${SCRIPT_DIR}/service.yaml"
envsubst '${KEYCLOAK_DB_HOST} ${KEYCLOAK_DB_NAME} ${KEYCLOAK_DB_USER} ${KEYCLOAK_DB_SSLMODE} ${KEYCLOAK_DB_PASSWORD_SECRET_NAME} ${KEYCLOAK_DB_PASSWORD_SECRET_KEY}' \
    < "${SCRIPT_DIR}/deployment.yaml" | kubectl apply -n "${NS}" -f -

echo "  Waiting for Keycloak to be ready..."
kubectl rollout status deployment/keycloak -n "${NS}" --timeout=180s

echo "  Keycloak ready (realm: nico, client: nico-rest)"
