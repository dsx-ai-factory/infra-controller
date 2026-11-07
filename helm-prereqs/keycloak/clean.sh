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

NS="${KEYCLOAK_NS:-nico-rest}"

echo "  Removing Keycloak resources..."
kubectl delete deploy keycloak -n "${NS}" --ignore-not-found 2>/dev/null || true
kubectl delete svc keycloak keycloak-nodeport -n "${NS}" --ignore-not-found 2>/dev/null || true
kubectl delete configmap keycloak-realm -n "${NS}" --ignore-not-found 2>/dev/null || true

echo "  Removing Keycloak secrets..."
kubectl delete secret -n "${NS}" \
    keycloak-client-secret \
    --ignore-not-found 2>/dev/null || true

echo "  Dropping keycloak database..."
_PG_POD="$(kubectl get pods -n postgres -l app=postgres \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)"

if [[ -n "${_PG_POD}" ]]; then
    kubectl exec -n postgres "${_PG_POD}" -- \
        psql -U postgres -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname='keycloak' AND pid <> pg_backend_pid();" 2>/dev/null || true
    kubectl exec -n postgres "${_PG_POD}" -- \
        psql -U postgres -c "DROP DATABASE IF EXISTS keycloak;" 2>/dev/null || true
    kubectl exec -n postgres "${_PG_POD}" -- \
        psql -U postgres -c "DROP USER IF EXISTS keycloak;" 2>/dev/null || true
    echo "  keycloak database dropped"
else
    echo "  WARNING: no postgres pod found — keycloak database not dropped"
fi

echo "  Keycloak cleanup complete"
