#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFLIGHT_SH="${SCRIPT_DIR}/../preflight.sh"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT

for fn in _yaml_toplevel_value _keycloak_ns _deployed_db_host _db_location \
    _resolve_use_ha_postgres _check_db_cutover; do
    definition="$(sed -n "/^${fn}()/,/^}/p" "${PREFLIGHT_SH}")"
    if [[ -z "${definition}" ]]; then
        echo "could not extract ${fn} from preflight.sh" >&2
        exit 1
    fi
    eval "${definition}"
done
eval "$(grep '^_DB_RECORD_CONFIGMAP=' "${PREFLIGHT_SH}")"

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
# FAKE_RECORD holds the ConfigMap data as key=value pairs separated by ";".
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${FAKE_READS_FAIL}" == "true" ]]; then
    exit 1
fi
case "$*" in
    'get deployment keycloak -n kc-ns --ignore-not-found -o name')
        if [[ -n "${FAKE_KEYCLOAK_URL}" ]]; then
            echo deployment.apps/keycloak
        fi
        ;;
    'get deployment keycloak -n kc-ns -o jsonpath='*)
        # valueFrom stands for a KC_DB_URL with no literal value.
        if [[ "${FAKE_KEYCLOAK_URL}" != "valueFrom" ]]; then
            printf '%s' "${FAKE_KEYCLOAK_URL}"
        fi
        ;;
    'get statefulset/postgres persistentvolumeclaim/postgres-data-postgres-0 -n postgres --ignore-not-found -o name')
        case "${FAKE_STANDALONE}" in
            statefulset) printf '%s\n' statefulset.apps/postgres persistentvolumeclaim/postgres-data-postgres-0 ;;
            pvc) echo persistentvolumeclaim/postgres-data-postgres-0 ;;
        esac
        ;;
    'get configmap nico-workload-databases -n postgres --ignore-not-found -o jsonpath={.data.'*)
        key="${!#}"
        key="${key#jsonpath=\{.data.}"
        key="${key%\}}"
        IFS=';' read -r -a pairs <<< "${FAKE_RECORD}"
        for pair in "${pairs[@]+"${pairs[@]}"}"; do
            if [[ "${pair%%=*}" == "${key}" ]]; then
                printf '%s' "${pair#*=}"
            fi
        done
        ;;
    'get deployment temporal-'*' -n temporal --ignore-not-found -o jsonpath={.metadata.uid}/{.metadata.generation}' \
        | 'get deployment keycloak -n kc-ns --ignore-not-found -o jsonpath={.metadata.uid}/{.metadata.generation}')
        printf '%s' "${FAKE_DEPLOYMENT_STATE}"
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

write_values() {
    local component="$1" value="$2" temporal_value=auto keycloak_value=auto
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
}

# Compares the joined errors with the expected substring; empty expects none.
check_errors() {
    local name="$1" errors="$2" expected_error="$3"
    if [[ -z "${expected_error}" && -n "${errors}" ]]; then
        echo "FAIL ${name}: unexpected errors: ${errors}" >&2
        failures=$((failures + 1))
    elif [[ -n "${expected_error}" && "${errors}" != *"${expected_error}"* ]]; then
        echo "FAIL ${name}: errors '${errors:-none}' do not contain '${expected_error}'" >&2
        failures=$((failures + 1))
    fi
}

# name | component | value | deployed Temporal DB host | deployed Keycloak KC_DB_URL |
#   standalone resources left (statefulset, pvc, or none) | recorded locations |
#   cluster reads fail | expected | expected error
resolver_cases=(
    "new Site|temporal|auto|||||false|true|"
    "existing Site on the StatefulSet|temporal|auto|postgres.postgres.svc.cluster.local.||statefulset||false|false|"
    "migrated Site stays on nico-pg-cluster|temporal|auto|nico-pg-cluster.postgres.svc.cluster.local||statefulset||false|true|"
    "StatefulSet without a deployed Temporal|temporal|auto|||statefulset||false|false|"
    "retained PVC without the StatefulSet or a deployed Temporal|temporal|auto|||pvc||false|false|"
    "deleted Keycloak recorded on nico-pg-cluster while the StatefulSet serves Temporal|keycloak|auto|||statefulset|keycloak=nico-pg-cluster|false|true|"
    "deleted Temporal recorded on the StatefulSet after it was removed|temporal|auto||||temporal=standalone|false|false|"
    "explicit true overrides the deployed database|temporal|true|postgres.postgres.svc.cluster.local.||statefulset||false|true|"
    "Temporal on an unrecognized host|temporal|auto|pg.example.com||||false||auto doesn't recognize pg.example.com, the PostgreSQL host the deployed temporal uses"
    "Keycloak on the StatefulSet|keycloak|auto||jdbc:postgresql://postgres.postgres:5432/keycloak?sslmode=disable|statefulset||false|false|"
    "Keycloak on nico-pg-cluster|keycloak|auto||jdbc:postgresql://nico-pg-cluster.postgres.svc.cluster.local:5432/keycloak?sslmode=require|statefulset||false|true|"
    "Keycloak with KC_DB_URL from valueFrom|keycloak|auto||valueFrom|||false||auto could not tell which PostgreSQL the deployed keycloak uses"
    "unreadable cluster state|temporal|auto|||||true||auto could not tell which PostgreSQL the deployed temporal uses"
    "invalid value|keycloak|maybe|||||false||keycloak.useHaPostgres must be true, false, or auto (got 'maybe')"
)

for row in "${resolver_cases[@]}"; do
    IFS='|' read -r name component value temporal_host keycloak_url standalone record \
        reads_fail expected expected_error <<< "${row}"
    write_values "${component}" "${value}"
    ERRORS=()
    FAKE_TEMPORAL_HOST="${temporal_host}" FAKE_KEYCLOAK_URL="${keycloak_url}" \
        FAKE_STANDALONE="${standalone}" FAKE_RECORD="${record}" FAKE_DEPLOYMENT_STATE="" \
        FAKE_READS_FAIL="${reads_fail}" PATH="${TEST_TMP_DIR}/bin:${PATH}" \
        _resolve_use_ha_postgres "${component}"
    if [[ "${_USE_HA_POSTGRES}" != "${expected}" ]]; then
        echo "FAIL ${name}: resolved '${_USE_HA_POSTGRES}', want '${expected}' (errors: ${ERRORS[*]:-none})" >&2
        failures=$((failures + 1))
        continue
    fi
    check_errors "${name}" "${ERRORS[*]:-}" "${expected_error}"
done

# The cutover check runs for an explicit true only. Same columns as above
# without value and expected, plus the current uid/generation every Deployment
# of the workload reports.
temporal_receipt="temporal-migrated=temporal-frontend=uid-t/4 temporal-history=uid-t/4 temporal-matching=uid-t/4 temporal-worker=uid-t/4"
cutover_cases=(
    "migrated Site already on nico-pg-cluster|temporal|nico-pg-cluster.postgres.svc.cluster.local||statefulset||uid-t/9|false|"
    "Temporal cutover after a migration, Deployments unchanged|temporal|postgres.postgres.svc.cluster.local.||statefulset|${temporal_receipt}|uid-t/4|false|"
    "Keycloak cutover after a migration, Deployment unchanged|keycloak||jdbc:postgresql://postgres.postgres:5432/keycloak?sslmode=disable|statefulset|keycloak-migrated=keycloak=uid-k/2|uid-k/2|false|"
    "Temporal on the StatefulSet without a migration|temporal|postgres.postgres.svc.cluster.local.||statefulset||uid-t/4|false|true moves temporal off the standalone postgres.postgres StatefulSet, but its data hasn't been migrated"
    "Temporal restarted and stopped again after its migration|temporal|postgres.postgres.svc.cluster.local.||statefulset|${temporal_receipt}|uid-t/6|false|temporal/temporal-frontend changed after the migration"
    "unreadable cluster state|temporal||||||true|so it can't rule out moving temporal off the standalone postgres.postgres StatefulSet"
)

for row in "${cutover_cases[@]}"; do
    IFS='|' read -r name component temporal_host keycloak_url standalone record deployment_state \
        reads_fail expected_error <<< "${row}"
    write_values "${component}" true
    BLOCKING_ERRORS=()
    FAKE_TEMPORAL_HOST="${temporal_host}" FAKE_KEYCLOAK_URL="${keycloak_url}" \
        FAKE_STANDALONE="${standalone}" FAKE_RECORD="${record}" \
        FAKE_DEPLOYMENT_STATE="${deployment_state}" FAKE_READS_FAIL="${reads_fail}" \
        PATH="${TEST_TMP_DIR}/bin:${PATH}" _check_db_cutover "${component}"
    check_errors "cutover: ${name}" "${BLOCKING_ERRORS[*]:-}" "${expected_error}"
done

# -y continues past ordinary preflight errors but never past a blocking one.
sed -n '/^# Output and prompts/,$p' "${PREFLIGHT_SH}" > "${TEST_TMP_DIR}/output.sh"
run_output() {
    local blocking="$1"
    # shellcheck disable=SC2034 # Read by the sourced output section.
    (
        _SOURCED=true
        AUTO_YES=true
        _NICO_REST_ENABLED=false
        ERRORS=("an ordinary error")
        WARNINGS=()
        BLOCKING_ERRORS=()
        if [[ "${blocking}" == "true" ]]; then
            BLOCKING_ERRORS=("a database error")
        fi
        # shellcheck disable=SC1091 # Generated from preflight.sh above.
        source "${TEST_TMP_DIR}/output.sh"
    )
}
if run_output true > "${TEST_TMP_DIR}/blocking.out" 2>&1; then
    echo "FAIL -y: preflight continued past a blocking error" >&2
    failures=$((failures + 1))
elif ! grep -Fq "a database error" "${TEST_TMP_DIR}/blocking.out"; then
    echo "FAIL -y: preflight did not print the blocking error" >&2
    failures=$((failures + 1))
fi
if ! run_output false > /dev/null 2>&1; then
    echo "FAIL -y: preflight stopped on an ordinary error" >&2
    failures=$((failures + 1))
fi

if (( failures > 0 )); then
    exit 1
fi
echo "preflight useHaPostgres resolution tests passed"
