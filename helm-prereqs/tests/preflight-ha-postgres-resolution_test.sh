#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFLIGHT_SH="${SCRIPT_DIR}/../preflight.sh"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT

for fn in _yaml_toplevel_value _deployed_db_host _resolve_use_ha_postgres; do
    definition="$(sed -n "/^${fn}()/,/^}/p" "${PREFLIGHT_SH}")"
    if [[ -z "${definition}" ]]; then
        echo "could not extract ${fn} from preflight.sh" >&2
        exit 1
    fi
    eval "${definition}"
done

mkdir -p "${TEST_TMP_DIR}/bin"
cat > "${TEST_TMP_DIR}/bin/helm" <<'FAKE_HELM'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${FAKE_READS_FAIL}" == "true" ]]; then
    exit 1
fi
case "$*" in
    'list -n temporal --short --filter ^temporal$')
        if [[ -n "${FAKE_TEMPORAL_HOST}" ]]; then
            echo temporal
        fi
        ;;
    'get values temporal -n temporal -o json')
        printf '{"server":{"config":{"persistence":{"default":{"sql":{"host":"%s"}}}}}}\n' "${FAKE_TEMPORAL_HOST}"
        ;;
    *)
        echo "unexpected: helm $*" >&2
        exit 2
        ;;
esac
FAKE_HELM
# Answers only for the keycloak.namespace in the test values, so a lookup in
# the wrong namespace fails the case instead of reading as "not deployed".
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${FAKE_READS_FAIL}" == "true" ]]; then
    exit 1
fi
case "$*" in
    'get deployment keycloak -n kc-ns --ignore-not-found -o jsonpath='*)
        printf '%s' "${FAKE_KEYCLOAK_URL}"
        ;;
    'get statefulset postgres -n postgres --ignore-not-found -o name')
        if [[ "${FAKE_LEGACY_STS}" == "true" ]]; then
            echo statefulset.apps/postgres
        fi
        ;;
    *)
        echo "unexpected: kubectl $*" >&2
        exit 2
        ;;
esac
FAKE_KUBECTL
chmod +x "${TEST_TMP_DIR}/bin/helm" "${TEST_TMP_DIR}/bin/kubectl"

unset KEYCLOAK_NS
_SITE_VALUES_CFG="${TEST_TMP_DIR}/values.yaml"
failures=0

# name | component | value | deployed Temporal DB host | deployed Keycloak KC_DB_URL |
#   standalone StatefulSet exists | cluster reads fail | expected | expected error
cases=(
    "new Site|temporal|auto|||false|false|true|"
    "existing Site on the StatefulSet|temporal|auto|postgres.postgres.svc.cluster.local.||true|false|false|"
    "migrated Site stays on nico-pg-cluster|temporal|auto|nico-pg-cluster.postgres.svc.cluster.local||true|false|true|"
    "StatefulSet without a deployed Temporal|temporal|auto|||true|false|false|"
    "explicit true overrides the deployed database|temporal|true|postgres.postgres.svc.cluster.local.||true|false|true|"
    "Keycloak on the StatefulSet|keycloak|auto||jdbc:postgresql://postgres.postgres:5432/keycloak?sslmode=disable|true|false|false|"
    "Keycloak on nico-pg-cluster|keycloak|auto||jdbc:postgresql://nico-pg-cluster.postgres.svc.cluster.local:5432/keycloak?sslmode=require|true|false|true|"
    "unreadable cluster state|temporal|auto|||false|true||auto could not read which PostgreSQL the deployed temporal uses"
    "invalid value|keycloak|maybe|||false|false||keycloak.useHaPostgres must be true, false, or auto (got 'maybe')"
)

for row in "${cases[@]}"; do
    IFS='|' read -r name component value temporal_host keycloak_url legacy_sts reads_fail expected expected_error <<< "${row}"
    temporal_value=auto
    keycloak_value=auto
    if [[ "${component}" == "temporal" ]]; then
        temporal_value="${value}"
    else
        keycloak_value="${value}"
    fi
    cat > "${_SITE_VALUES_CFG}" <<EOF
temporal:
  useHaPostgres: ${temporal_value}

keycloak:
  useHaPostgres: ${keycloak_value}
  namespace: kc-ns
EOF

    ERRORS=()
    FAKE_TEMPORAL_HOST="${temporal_host}" FAKE_KEYCLOAK_URL="${keycloak_url}" \
        FAKE_LEGACY_STS="${legacy_sts}" FAKE_READS_FAIL="${reads_fail}" \
        PATH="${TEST_TMP_DIR}/bin:${PATH}" _resolve_use_ha_postgres "${component}"
    errors="${ERRORS[*]:-}"

    if [[ "${_USE_HA_POSTGRES}" != "${expected}" ]]; then
        echo "FAIL ${name}: resolved '${_USE_HA_POSTGRES}', want '${expected}' (errors: ${errors:-none})" >&2
        failures=$((failures + 1))
    elif [[ -z "${expected_error}" && -n "${errors}" ]]; then
        echo "FAIL ${name}: unexpected errors: ${errors}" >&2
        failures=$((failures + 1))
    elif [[ -n "${expected_error}" && "${errors}" != *"${expected_error}"* ]]; then
        echo "FAIL ${name}: errors '${errors:-none}' do not contain '${expected_error}'" >&2
        failures=$((failures + 1))
    fi
done

if (( failures > 0 )); then
    exit 1
fi
echo "preflight useHaPostgres resolution tests passed"
