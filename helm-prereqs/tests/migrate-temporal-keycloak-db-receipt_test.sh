#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MIGRATE_SH="${SCRIPT_DIR}/../scripts/migrate-temporal-keycloak-db.sh"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
STATE="${TEST_TMP_DIR}/state"
export FAKE_STATE="${STATE}"

# A stateful kubectl: Deployment replicas and generations, the receipt
# ConfigMap, and a log of the calls whose order the cases assert. Scaling to
# the current replica count is a no-op, so it keeps the generation, as on a
# real cluster.
mkdir -p "${TEST_TMP_DIR}/bin"
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
set -euo pipefail
S="${FAKE_STATE}"
args="$*"
dep="${args#* deploy }"
dep="${dep%% *}"
case "${args}" in
    'get pods -n postgres -l app=postgres -o jsonpath={.items[0].metadata.name}')
        echo postgres-0
        ;;
    'get pods -n postgres -l cluster-name=nico-pg-cluster,spilo-role=master -o jsonpath={.items[0].metadata.name}')
        echo nico-pg-cluster-0
        ;;
    'exec -n postgres nico-pg-cluster-0 -- psql -U postgres -tAc SELECT 1 FROM pg_database WHERE datname='*)
        echo 1
        ;;
    'get configmap nico-workload-databases -n postgres --ignore-not-found -o name')
        [[ "${FAKE_CONFIGMAP_READ_FAILS:-false}" != "true" ]]
        if [[ -f "${S}/configmap.json" ]]; then
            echo configmap/nico-workload-databases
        fi
        ;;
    'create configmap nico-workload-databases -n postgres')
        echo '{"data":{}}' > "${S}/configmap.json"
        ;;
    'get configmap temporal-'*'-config -n temporal -o jsonpath='*)
        host="${FAKE_TEMPORAL_HOST:-postgres.postgres.svc.cluster.local.}"
        printf 'persistence:\n  datastores:\n    default:\n      sql:\n        connectAddr: "%s:5432"\n    visibility:\n      sql:\n        connectAddr: "%s:5432"\n' \
            "${host}" "${host}"
        ;;
    'get deployment keycloak -n '*' -o jsonpath='*KC_DB_URL*)
        echo "${FAKE_KEYCLOAK_URL:-jdbc:postgresql://postgres.postgres:5432/keycloak?sslmode=disable}"
        ;;
    'patch configmap nico-workload-databases -n postgres --type merge -p '*)
        if [[ "${FAKE_RECEIPT_WRITE_FAILS:-false}" == "true" && "${!#}" == *'-migrated":"'* ]]; then
            exit 1
        fi
        jq --argjson p "${!#}" \
            '.data = (((.data // {}) + $p.data) | with_entries(select(.value != null)))' \
            "${S}/configmap.json" > "${S}/configmap.tmp"
        mv "${S}/configmap.tmp" "${S}/configmap.json"
        echo "patch ${!#}" >> "${S}/log"
        ;;
    'get deploy '*' -n temporal -o jsonpath={.spec.replicas}')
        cat "${S}/replicas-${dep}"
        ;;
    'scale deploy '*' -n temporal --replicas='*)
        if [[ "$(cat "${S}/replicas-${dep}")" != "${args##*--replicas=}" ]]; then
            echo "${args##*--replicas=}" > "${S}/replicas-${dep}"
            echo $(( $(cat "${S}/gen-${dep}") + 1 )) > "${S}/gen-${dep}"
        fi
        ;;
    'rollout status deploy/'*)
        ;;
    'get deploy '*' -n temporal -o jsonpath={.metadata.uid}/{.metadata.generation}')
        [[ "${FAKE_STATE_READ_FAILS:-false}" != "true" ]]
        printf 'uid-%s/%s' "${dep}" "$(cat "${S}/gen-${dep}")"
        ;;
    'exec -n postgres postgres-0 -- pg_dump '*)
        echo dump
        ;;
    'exec -i -n postgres nico-pg-cluster-0 -- pg_restore -U postgres -d '*)
        db="${args#*-d }"
        db="${db%% *}"
        cat > /dev/null
        echo "pg_restore ${db}" >> "${S}/log"
        [[ "${FAKE_RESTORE_FAILS:-}" != "${db}" ]]
        ;;
    *)
        echo "unexpected: kubectl ${args}" >&2
        exit 2
        ;;
esac
FAKE_KUBECTL
chmod +x "${TEST_TMP_DIR}/bin/kubectl"

# Temporal's ConfigMaps above name the StatefulSet unless FAKE_TEMPORAL_HOST
# says otherwise. Helm reports its latest revision, and FAKE_HELM_TEMPORAL_HOST
# stands for one whose pre-upgrade hook failed before the ConfigMaps changed.
cat > "${TEST_TMP_DIR}/bin/helm" <<'FAKE_HELM'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" != 'get values temporal -n temporal -o json' ]]; then
    echo "unexpected: helm $*" >&2
    exit 2
fi
host="${FAKE_HELM_TEMPORAL_HOST:-${FAKE_TEMPORAL_HOST:-}}"
if [[ -n "${host}" ]]; then
    jq -n --arg h "${host}" '{server: {config: {persistence: {default: {sql: {host: $h}}}}}}'
else
    echo '{}'
fi
FAKE_HELM
chmod +x "${TEST_TMP_DIR}/bin/helm"

DEPLOYMENTS=(temporal-frontend temporal-history temporal-matching temporal-worker)
failures=0

reset_state() {
    local replicas="$1" generation="$2" dep
    rm -rf "${STATE}"
    mkdir -p "${STATE}"
    : > "${STATE}/log"
    for dep in "${DEPLOYMENTS[@]}"; do
        echo "${replicas}" > "${STATE}/replicas-${dep}"
        echo "${generation}" > "${STATE}/gen-${dep}"
    done
}

receipt_for_generation() {
    local generation="$1" dep
    local -a entries=()
    for dep in "${DEPLOYMENTS[@]}"; do
        entries+=("${dep}=uid-${dep}/${generation}")
    done
    printf '%s\n' "${entries[*]}"
}

run_migration() {
    PATH="${TEST_TMP_DIR}/bin:${PATH}" bash "${MIGRATE_SH}" --db "${1:-temporal}" \
        > "${STATE}/output" 2>&1
}

replicas() {
    local dep
    for dep in "${DEPLOYMENTS[@]}"; do
        cat "${STATE}/replicas-${dep}"
    done | sort -u | tr '\n' ' '
}

receipt() {
    if [[ -f "${STATE}/configmap.json" ]]; then
        jq -r '.data["temporal-migrated"] // ""' "${STATE}/configmap.json"
    fi
}

fail() {
    echo "FAIL $1" >&2
    failures=$((failures + 1))
}

# A first migration records each Deployment as it was once stopped, and only
# after the last restore.
reset_state 1 3
if ! run_migration; then
    fail "first migration exited non-zero: $(tail -3 "${STATE}/output")"
fi
if [[ "$(receipt)" != "$(receipt_for_generation 4)" ]]; then
    fail "first migration wrote receipt '$(receipt)', want '$(receipt_for_generation 4)'"
fi
if [[ "$(tail -1 "${STATE}/log")" != 'patch {"data":{"temporal-migrated":"'* ]]; then
    fail "first migration did not write the receipt after its last restore: $(tr '\n' ';' < "${STATE}/log")"
fi

# A retry on top of a valid receipt clears it before restoring anything, so a
# failed restore leaves no receipt behind. Its scale-down is a no-op, so the
# old receipt would still match every Deployment.
reset_state 0 4
jq -n --arg r "$(receipt_for_generation 4)" '{data: {temporal: "standalone", "temporal-migrated": $r}}' \
    > "${STATE}/configmap.json"
if FAKE_RESTORE_FAILS=temporal_visibility run_migration; then
    fail "retry with a failing restore exited zero"
fi
if [[ -n "$(receipt)" ]]; then
    fail "retry with a failing restore kept receipt '$(receipt)'"
fi
if [[ "$(jq -r '.data.temporal' "${STATE}/configmap.json")" != "standalone" ]]; then
    fail "clearing the receipt dropped the recorded location"
fi
if [[ "$(head -1 "${STATE}/log")" != 'patch {"data":{"temporal-migrated":null}}' ]]; then
    fail "retry did not clear the receipt before its first restore: $(tr '\n' ';' < "${STATE}/log")"
fi

# A retry whose Deployments are already at zero keeps their generations, so the
# old receipt would still match. A failed ConfigMap read must stop the retry
# before it restores anything, or a failed copy would hide behind that receipt.
reset_state 0 4
jq -n --arg r "$(receipt_for_generation 4)" '{data: {temporal: "standalone", "temporal-migrated": $r}}' \
    > "${STATE}/configmap.json"
if FAKE_CONFIGMAP_READ_FAILS=true FAKE_RESTORE_FAILS=temporal_visibility run_migration; then
    fail "retry with a failing ConfigMap read exited zero"
fi
if [[ -s "${STATE}/log" ]]; then
    fail "retry with a failing ConfigMap read changed the cluster: $(tr '\n' ';' < "${STATE}/log")"
fi

# A failed uid/generation read stops the migration before it writes a receipt
# preflight would reject, and brings Temporal back up on postgres.postgres.
reset_state 1 3
if FAKE_STATE_READ_FAILS=true run_migration; then
    fail "migration with a failing Deployment-state read exited zero"
fi
if [[ -n "$(receipt)" ]]; then
    fail "migration with a failing Deployment-state read wrote receipt '$(receipt)'"
fi
if [[ "$(replicas)" != "1 " ]]; then
    fail "migration with a failing Deployment-state read left replicas at '$(replicas)', want '1 '"
fi

# After a cutover whose pre-upgrade hook failed, Helm's latest revision names
# nico-pg-cluster while Temporal still runs on postgres.postgres. Refreshing the
# copy must still work, so the source comes from the ConfigMaps, not Helm.
reset_state 1 3
if ! FAKE_HELM_TEMPORAL_HOST=nico-pg-cluster.postgres.svc.cluster.local run_migration; then
    fail "migration after a failed pre-upgrade hook exited non-zero: $(tail -3 "${STATE}/output")"
fi
if [[ "$(receipt)" != "$(receipt_for_generation 4)" ]]; then
    fail "migration after a failed pre-upgrade hook wrote receipt '$(receipt)', want '$(receipt_for_generation 4)'"
fi

# A failed receipt write brings Temporal back up on postgres.postgres, since
# preflight would never let setup.sh move it without that receipt.
reset_state 1 3
if FAKE_RECEIPT_WRITE_FAILS=true run_migration; then
    fail "migration with a failing receipt write exited zero"
fi
if [[ "$(replicas)" != "1 " ]]; then
    fail "migration with a failing receipt write left replicas at '$(replicas)', want '1 '"
fi

# The script refuses a workload that already moved, before stopping or
# copying anything. Columns: name|--db|Temporal host|Keycloak KC_DB_URL|output.
refusal_cases=(
    "repeat run after the Temporal cutover|temporal|nico-pg-cluster.postgres.svc.cluster.local||ERROR: temporal already uses nico-pg-cluster"
    "--db both with only Keycloak moved|both||jdbc:postgresql://nico-pg-cluster.postgres.svc.cluster.local:5432/keycloak?sslmode=require|ERROR: keycloak already uses nico-pg-cluster"
    "Temporal on an unrecognized host|temporal|pg.example.com||ERROR: temporal uses pg.example.com, not postgres.postgres"
)
for row in "${refusal_cases[@]}"; do
    IFS='|' read -r name db temporal_host keycloak_url expected_output <<< "${row}"
    reset_state 1 3
    if FAKE_TEMPORAL_HOST="${temporal_host}" FAKE_KEYCLOAK_URL="${keycloak_url}" run_migration "${db}"; then
        fail "${name}: exited zero"
    fi
    if ! grep -qF -- "${expected_output}" "${STATE}/output"; then
        fail "${name}: output does not contain '${expected_output}': $(tail -3 "${STATE}/output")"
    fi
    if [[ -s "${STATE}/log" || "$(replicas)" != "1 " ]]; then
        fail "${name}: changed the cluster before refusing: $(tr '\n' ';' < "${STATE}/log") replicas $(replicas)"
    fi
done

if (( failures > 0 )); then
    exit 1
fi
echo "migrate-temporal-keycloak-db receipt tests passed"
