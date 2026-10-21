#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFLIGHT_SH="${SCRIPT_DIR}/../preflight.sh"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT

for fn in _yaml_toplevel_value _keycloak_ns _deployed_db_host _db_location \
    _resolve_use_ha_postgres _check_db_cutover _can_move_db _check_keycloak_ns \
    _check_use_ha_postgres_value; do
    definition="$(sed -n "/^${fn}()/,/^}/p" "${PREFLIGHT_SH}")"
    if [[ -z "${definition}" ]]; then
        echo "could not extract ${fn} from preflight.sh" >&2
        exit 1
    fi
    eval "${definition}"
done
eval "$(grep '^_DB_RECORD_CONFIGMAP=' "${PREFLIGHT_SH}")"
eval "$(sed -n '/^_recheck_db_cutover()/,/^}/p' "${SETUP_SH}")"

mkdir -p "${TEST_TMP_DIR}/bin"
# Helm reports its latest revision. FAKE_HELM_TEMPORAL_HOST stands for one whose
# pre-upgrade hook failed, so its host never reached the ConfigMaps below.
cat > "${TEST_TMP_DIR}/bin/helm" <<'FAKE_HELM'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${FAKE_READS_FAIL}" == "true" ]]; then
    exit 1
fi
host="${FAKE_HELM_TEMPORAL_HOST:-${FAKE_TEMPORAL_HOST}}"
case "$*" in
    'list -n temporal --short --filter ^temporal$')
        if [[ -n "${host}" ]]; then
            echo temporal
        fi
        ;;
    'get values temporal -n temporal -o json')
        printf '{"server":{"config":{"persistence":{"default":{"sql":{"host":"%s"}}}}}}\n' "${host}"
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
    'get configmap temporal-'*'-config -n temporal --ignore-not-found -o jsonpath='*)
        # FAKE_TEMPORAL_WORKER_HOST stands for services that disagree.
        host="${FAKE_TEMPORAL_HOST}"
        if [[ "$*" == 'get configmap temporal-worker-config '* ]]; then
            host="${FAKE_TEMPORAL_WORKER_HOST:-${host}}"
        fi
        if [[ -n "${host}" ]]; then
            printf 'persistence:\n  datastores:\n    default:\n      sql:\n        connectAddr: "%s:5432"\n    visibility:\n      sql:\n        connectAddr: "%s:5432"\n' \
                "${host}" "${host}"
        fi
        ;;
    'get deployment keycloak -n kc-ns --ignore-not-found -o name')
        if [[ -n "${FAKE_KEYCLOAK_URL}" ]]; then
            echo deployment.apps/keycloak
        fi
        ;;
    'get deployment keycloak -n kc-ns -o jsonpath='*)
        # valueFrom stands for a KC_DB_URL without a literal value.
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
_REST_VALUES_CFG="${TEST_TMP_DIR}/nico-rest.yaml"
failures=0

write_rest_values() {
    cat > "${_REST_VALUES_CFG}" <<EOF
nico-rest-api:
  config:
    keycloak:
      enabled: $1
EOF
}
write_rest_values true

write_values() {
    local component="$1" value="$2" postgresql_enabled="${3:-true}"
    local temporal_value=auto keycloak_value=auto
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

postgresql:
  enabled: ${postgresql_enabled}
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
    "Keycloak enabled for the first time while the StatefulSet serves Temporal|keycloak|auto|||statefulset||false|true|"
    "deleted Temporal recorded on a StatefulSet whose volume is gone too|temporal|auto||||temporal=standalone|false|true|"
    "deleted Keycloak recorded on the StatefulSet that still exists|keycloak|auto|||statefulset|keycloak=standalone|false|false|"
    "explicit true overrides the deployed database|temporal|true|postgres.postgres.svc.cluster.local.||statefulset||false|true|"
    "Temporal on an unrecognized host|temporal|auto|pg.example.com||||false||auto doesn't recognize pg.example.com, the PostgreSQL host the deployed temporal uses"
    "Temporal on a host that only starts like nico-pg-cluster|temporal|auto|nico-pg-cluster.postgres.example.net|||statefulset|false||auto doesn't recognize nico-pg-cluster.postgres.example.net"
    "Keycloak on a host that only starts like the StatefulSet|keycloak|auto||jdbc:postgresql://postgres.postgres.example.net:5432/keycloak||statefulset|false||auto doesn't recognize postgres.postgres.example.net"
    "Keycloak on the StatefulSet|keycloak|auto||jdbc:postgresql://postgres.postgres:5432/keycloak?sslmode=disable|statefulset||false|false|"
    "Keycloak on nico-pg-cluster|keycloak|auto||jdbc:postgresql://nico-pg-cluster.postgres.svc.cluster.local:5432/keycloak?sslmode=require|statefulset||false|true|"
    "Keycloak with KC_DB_URL from valueFrom|keycloak|auto||valueFrom|||false||auto could not tell which PostgreSQL the deployed keycloak uses"
    "unreadable cluster state|temporal|auto|||||true||auto could not tell which PostgreSQL the deployed temporal uses"
)

for row in "${resolver_cases[@]}"; do
    IFS='|' read -r name component value temporal_host keycloak_url standalone record \
        reads_fail expected expected_error <<< "${row}"
    write_values "${component}" "${value}"
    BLOCKING_ERRORS=()
    FAKE_TEMPORAL_HOST="${temporal_host}" FAKE_KEYCLOAK_URL="${keycloak_url}" \
        FAKE_STANDALONE="${standalone}" FAKE_RECORD="${record}" FAKE_DEPLOYMENT_STATE="" \
        FAKE_READS_FAIL="${reads_fail}" PATH="${TEST_TMP_DIR}/bin:${PATH}" \
        _resolve_use_ha_postgres "${component}"
    if [[ "${_USE_HA_POSTGRES}" != "${expected}" ]]; then
        echo "FAIL ${name}: resolved '${_USE_HA_POSTGRES}', want '${expected}' (errors: ${BLOCKING_ERRORS[*]:-none})" >&2
        failures=$((failures + 1))
        continue
    fi
    # Phase 7 can't run without a resolved value, so -y must not continue.
    check_errors "${name}" "${BLOCKING_ERRORS[*]:-}" "${expected_error}"
done

# A cutover whose pre-upgrade hook failed leaves Helm's latest revision on
# nico-pg-cluster, while the ConfigMaps Temporal loads still name the
# StatefulSet. auto must keep Temporal there, and true must still require the
# receipt, or the next run could switch Temporal to a stale copy.
write_values temporal auto
BLOCKING_ERRORS=()
FAKE_HELM_TEMPORAL_HOST=nico-pg-cluster.postgres.svc.cluster.local \
    FAKE_TEMPORAL_HOST=postgres.postgres.svc.cluster.local. FAKE_KEYCLOAK_URL="" \
    FAKE_STANDALONE=statefulset FAKE_RECORD="" FAKE_DEPLOYMENT_STATE="" FAKE_READS_FAIL=false \
    PATH="${TEST_TMP_DIR}/bin:${PATH}" _resolve_use_ha_postgres temporal
if [[ "${_USE_HA_POSTGRES}" != "false" ]]; then
    echo "FAIL failed pre-upgrade hook: auto resolved '${_USE_HA_POSTGRES}', want 'false'" >&2
    failures=$((failures + 1))
fi
write_values temporal true
FAKE_HELM_TEMPORAL_HOST=nico-pg-cluster.postgres.svc.cluster.local \
    FAKE_TEMPORAL_HOST=postgres.postgres.svc.cluster.local. FAKE_KEYCLOAK_URL="" \
    FAKE_STANDALONE=statefulset FAKE_RECORD="" FAKE_DEPLOYMENT_STATE="" FAKE_READS_FAIL=false \
    PATH="${TEST_TMP_DIR}/bin:${PATH}" _check_db_cutover temporal true
check_errors "failed pre-upgrade hook" "${BLOCKING_ERRORS[*]:-}" \
    "true moves temporal off the standalone postgres.postgres StatefulSet, but its data hasn't been migrated"

# Services that name different hosts fail closed instead of picking one.
write_values temporal auto
BLOCKING_ERRORS=()
FAKE_TEMPORAL_HOST=postgres.postgres.svc.cluster.local. \
    FAKE_TEMPORAL_WORKER_HOST=nico-pg-cluster.postgres.svc.cluster.local FAKE_KEYCLOAK_URL="" \
    FAKE_STANDALONE=statefulset FAKE_RECORD="" FAKE_DEPLOYMENT_STATE="" FAKE_READS_FAIL=false \
    PATH="${TEST_TMP_DIR}/bin:${PATH}" _resolve_use_ha_postgres temporal
check_errors "Temporal services disagree" "${BLOCKING_ERRORS[*]:-}" \
    "auto could not tell which PostgreSQL the deployed temporal uses"

# Phase 7d doesn't deploy a Keycloak that nico-rest.yaml disables, so a
# Deployment the Site runs itself under that name can't block setup, even one
# whose KC_DB_URL comes from valueFrom.
write_rest_values false
write_values keycloak auto
BLOCKING_ERRORS=()
FAKE_TEMPORAL_HOST="" FAKE_KEYCLOAK_URL=valueFrom FAKE_STANDALONE=statefulset FAKE_RECORD="" \
    FAKE_DEPLOYMENT_STATE="" FAKE_READS_FAIL=false PATH="${TEST_TMP_DIR}/bin:${PATH}" \
    _resolve_use_ha_postgres keycloak
if [[ "${_USE_HA_POSTGRES}" != "true" ]]; then
    echo "FAIL disabled Keycloak: resolved '${_USE_HA_POSTGRES}', want 'true'" >&2
    failures=$((failures + 1))
fi
check_errors "disabled Keycloak" "${BLOCKING_ERRORS[*]:-}" ""
write_rest_values true

# The chart fails to render any other value, so preflight stops on it without
# cluster access.
write_values keycloak maybe
BLOCKING_ERRORS=()
_check_use_ha_postgres_value keycloak
check_errors "invalid value" "${BLOCKING_ERRORS[*]:-}" \
    "keycloak.useHaPostgres must be true, false, or auto (got 'maybe')"

# With postgresql.enabled false, nico-pg-cluster is an external or out-of-band PostgreSQL.
# A new Site's auto can't assume it's provisioned, but a Site already on it
# stays there. Columns: name|value|deployed Temporal DB host|expected.
external_cases=(
    "new Site keeps the StatefulSet|auto||false"
    "Site already on an external or out-of-band PostgreSQL|auto|nico-pg-cluster.postgres.svc.cluster.local|true"
    "explicit true selects an external or out-of-band PostgreSQL|true||true"
)
for row in "${external_cases[@]}"; do
    IFS='|' read -r name value temporal_host expected <<< "${row}"
    write_values temporal "${value}" false
    BLOCKING_ERRORS=()
    FAKE_TEMPORAL_HOST="${temporal_host}" FAKE_KEYCLOAK_URL="" FAKE_STANDALONE="" FAKE_RECORD="" \
        FAKE_DEPLOYMENT_STATE="" FAKE_READS_FAIL=false PATH="${TEST_TMP_DIR}/bin:${PATH}" \
        _resolve_use_ha_postgres temporal
    if [[ "${_USE_HA_POSTGRES}" != "${expected}" ]]; then
        echo "FAIL postgresql.enabled false, ${name}: resolved '${_USE_HA_POSTGRES}', want '${expected}' (errors: ${BLOCKING_ERRORS[*]:-none})" >&2
        failures=$((failures + 1))
    fi
    check_errors "postgresql.enabled false, ${name}" "${BLOCKING_ERRORS[*]:-}" ""
done

# The migration script can't copy into an external or out-of-band PostgreSQL yet, so the
# missing receipt points there instead of at the script.
write_values temporal true false
BLOCKING_ERRORS=()
FAKE_TEMPORAL_HOST="postgres.postgres.svc.cluster.local." FAKE_KEYCLOAK_URL="" \
    FAKE_STANDALONE=statefulset FAKE_RECORD="" FAKE_DEPLOYMENT_STATE="" FAKE_READS_FAIL=false \
    PATH="${TEST_TMP_DIR}/bin:${PATH}" _check_db_cutover temporal true
check_errors "postgresql.enabled false cutover without a migration" "${BLOCKING_ERRORS[*]:-}" \
    "2.4 doesn't support copying it into an external or out-of-band PostgreSQL"

# The move check runs for an explicit value. Same columns as above without
# expected, plus the current uid/generation every Deployment of the workload
# reports.
temporal_receipt="temporal-migrated=temporal-frontend=uid-t/4 temporal-history=uid-t/4 temporal-matching=uid-t/4 temporal-worker=uid-t/4"
cutover_cases=(
    "migrated Site already on nico-pg-cluster|temporal|true|nico-pg-cluster.postgres.svc.cluster.local||statefulset||uid-t/9|false|"
    "Temporal cutover after a migration, Deployments unchanged|temporal|true|postgres.postgres.svc.cluster.local.||statefulset|${temporal_receipt}|uid-t/4|false|"
    "Keycloak cutover after a migration, Deployment unchanged|keycloak|true||jdbc:postgresql://postgres.postgres:5432/keycloak?sslmode=disable|statefulset|keycloak-migrated=keycloak=uid-k/2|uid-k/2|false|"
    "explicit true for a Keycloak never deployed while the StatefulSet serves Temporal|keycloak|true|||statefulset|||false|"
    "Temporal on the StatefulSet without a migration|temporal|true|postgres.postgres.svc.cluster.local.||statefulset||uid-t/4|false|true moves temporal off the standalone postgres.postgres StatefulSet, but its data hasn't been migrated"
    "Temporal restarted and stopped again after its migration|temporal|true|postgres.postgres.svc.cluster.local.||statefulset|${temporal_receipt}|uid-t/6|false|temporal/temporal-frontend changed after the migration"
    "explicit false keeps an existing Site on the StatefulSet|temporal|false|postgres.postgres.svc.cluster.local.||statefulset||uid-t/4|false|"
    "explicit false on a Site already on nico-pg-cluster|temporal|false|nico-pg-cluster.postgres.svc.cluster.local||statefulset||uid-t/9|false|false would move temporal off nico-pg-cluster onto the standalone postgres.postgres StatefulSet"
    "explicit true on an unrecognized host|temporal|true|pg.example.com||||uid-t/4|false|setup.sh only runs temporal on nico-pg-cluster or the standalone postgres.postgres StatefulSet"
    "unreadable cluster state|temporal|true||||||true|so it can't rule out moving temporal between the standalone postgres.postgres StatefulSet and nico-pg-cluster"
)

for row in "${cutover_cases[@]}"; do
    IFS='|' read -r name component value temporal_host keycloak_url standalone record deployment_state \
        reads_fail expected_error <<< "${row}"
    write_values "${component}" "${value}"
    BLOCKING_ERRORS=()
    FAKE_TEMPORAL_HOST="${temporal_host}" FAKE_KEYCLOAK_URL="${keycloak_url}" \
        FAKE_STANDALONE="${standalone}" FAKE_RECORD="${record}" \
        FAKE_DEPLOYMENT_STATE="${deployment_state}" FAKE_READS_FAIL="${reads_fail}" \
        PATH="${TEST_TMP_DIR}/bin:${PATH}" _check_db_cutover "${component}" "${value}"
    check_errors "cutover: ${name}" "${BLOCKING_ERRORS[*]:-}" "${expected_error}"
done

# Preflight runs the move check above for every value that can move a
# database. Columns: value|whether the check runs.
can_move_cases=(
    "false|yes"
    "auto|no"
)
for row in "${can_move_cases[@]}"; do
    IFS='|' read -r value expected <<< "${row}"
    write_values temporal "${value}"
    actual=no
    if _can_move_db temporal; then
        actual=yes
    fi
    if [[ "${actual}" != "${expected}" ]]; then
        echo "FAIL move check for ${value}: runs ${actual}, want ${expected}" >&2
        failures=$((failures + 1))
    fi
done

# setup.sh checks the move again right before it repoints a workload, since
# phases 1 to 6 run between preflight and phase 7f. A Temporal restarted in
# that window stops setup instead of moving onto the stale copy.
write_values temporal true
recheck_cases=(
    "Deployments unchanged since the migration|uid-t/4|0"
    "Temporal restarted after preflight|uid-t/6|1"
)
for row in "${recheck_cases[@]}"; do
    IFS='|' read -r name deployment_state expected_status <<< "${row}"
    status=0
    (
        export FAKE_TEMPORAL_HOST=postgres.postgres.svc.cluster.local. FAKE_KEYCLOAK_URL="" \
            FAKE_STANDALONE=statefulset FAKE_RECORD="${temporal_receipt}" \
            FAKE_DEPLOYMENT_STATE="${deployment_state}" FAKE_READS_FAIL=false \
            PATH="${TEST_TMP_DIR}/bin:${PATH}"
        _recheck_db_cutover temporal true
    ) > /dev/null 2>&1 || status=$?
    if [[ "${status}" != "${expected_status}" ]]; then
        echo "FAIL recheck: ${name}: exit ${status}, want ${expected_status}" >&2
        failures=$((failures + 1))
    fi
done

# Each recheck sits directly before the command that repoints its workload.
# Columns: component|line that repoints it.
# shellcheck disable=SC2016 # The lines are matched as written in setup.sh.
recheck_wiring=(
    'keycloak|"${SCRIPT_DIR}/keycloak/setup.sh"'
    'temporal|"${TEMPORAL_CMD[@]}"'
)
for row in "${recheck_wiring[@]}"; do
    IFS='|' read -r component repoint <<< "${row}"
    next_line="$(awk -v call="_recheck_db_cutover ${component} " '
        found { sub(/^[[:space:]]+/, ""); print; exit }
        index($0, call) { found = 1 }
    ' "${SETUP_SH}")"
    if [[ "${next_line}" != "${repoint}" ]]; then
        echo "FAIL recheck: setup.sh must call _recheck_db_cutover ${component} right before ${repoint}, not '${next_line}'" >&2
        failures=$((failures + 1))
    fi
done

# ESO syncs Keycloak's credentials only into keycloak.namespace (kc-ns here),
# so a different KEYCLOAK_NS would wait for them in the wrong namespace.
write_values keycloak true
BLOCKING_ERRORS=()
KEYCLOAK_NS=other-ns _check_keycloak_ns
check_errors "KEYCLOAK_NS mismatch" "${BLOCKING_ERRORS[*]:-}" \
    "KEYCLOAK_NS is other-ns, but keycloak.namespace is kc-ns"

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
