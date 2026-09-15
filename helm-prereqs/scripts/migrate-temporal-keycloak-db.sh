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
#
# =============================================================================
# migrate-temporal-keycloak-db.sh — one-time cutover of Temporal and/or
# Keycloak from the legacy standalone postgres.postgres StatefulSet onto the
# shared nico-pg-cluster. See "Consolidating Temporal/Keycloak onto
# nico-pg-cluster" in README.md for the full transition story and
# prerequisites (temporal.useHaPostgres/keycloak.useHaPostgres, helmfile sync).
#
# This is a stop-the-world dump/restore: Temporal/Keycloak are scaled to zero
# for the duration and STAY at zero on success, until the follow-up setup.sh
# run repoints them at nico-pg-cluster and scales them back up — restoring
# them here instead would resume writes against the about-to-be-abandoned
# postgres.postgres, and those writes would be silently lost at cutover. A
# failed migration is the exception: the EXIT trap restores the original
# replica count immediately, since nothing was cut over.
#
# Usage:
#   ./migrate-temporal-keycloak-db.sh [--db temporal|keycloak|both] [--dry-run]
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREREQS_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

DB_TARGET="both"
DRY_RUN=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --db)
            if [[ $# -lt 2 ]]; then
                echo "ERROR: --db requires a value (temporal, keycloak, or both)" >&2
                exit 1
            fi
            DB_TARGET="$2"
            shift 2
            ;;
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        -h|--help)
            grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

if [[ "${DB_TARGET}" != "temporal" && "${DB_TARGET}" != "keycloak" && "${DB_TARGET}" != "both" ]]; then
    echo "ERROR: --db must be temporal, keycloak, or both (got: ${DB_TARGET})" >&2
    exit 1
fi

_run() {
    if "${DRY_RUN}"; then
        echo "  [dry-run] $*"
    else
        "$@"
    fi
}

# Reads a scalar field nested directly under a YAML key, at any indentation
# depth. Scans until the next line at the same or shallower indentation as
# the matched key, rather than a fixed line count, so it doesn't break if a
# comment block above the field grows. Mirrors preflight.sh's
# _yaml_toplevel_value (duplicated here since this script runs standalone).
_yaml_toplevel_value() {
    local _file="$1" _key="$2" _field="$3"
    awk -v key="${_key}" -v field="${_field}" '
        {
            indent = match($0, /[^ ]/) - 1
            trimmed = $0
            sub(/^[[:space:]]*/, "", trimmed)
        }
        !in_block && trimmed == key ":" { in_block = 1; key_indent = indent; next }
        in_block && trimmed != "" && trimmed !~ /^#/ && indent <= key_indent { exit }
        in_block && trimmed ~ "^" field ":[[:space:]]*" {
            sub("^" field ":[[:space:]]*", "", trimmed)
            sub(/[[:space:]]+#.*/, "", trimmed)
            gsub(/"/, "", trimmed)
            print trimmed
            exit
        }
    ' "${_file}"
}

_DRY_RUN_LABEL=""
if "${DRY_RUN}"; then
    _DRY_RUN_LABEL=", dry-run"
fi
echo "=== migrate-temporal-keycloak-db.sh (--db ${DB_TARGET}${_DRY_RUN_LABEL}) ==="

# ---------------------------------------------------------------------------
# Preflight: legacy postgres pod
# ---------------------------------------------------------------------------
LEGACY_PG_POD="$(kubectl get pods -n postgres -l app=postgres \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)"
if [[ -z "${LEGACY_PG_POD}" ]]; then
    echo "ERROR: no legacy postgres pod found (app=postgres in the postgres namespace) — nothing to migrate from." >&2
    exit 1
fi
echo "Legacy postgres pod: ${LEGACY_PG_POD}"

# ---------------------------------------------------------------------------
# Preflight: nico-pg-cluster master pod
# ---------------------------------------------------------------------------
NICO_PG_POD="$(kubectl get pods -n postgres \
    -l cluster-name=nico-pg-cluster,spilo-role=master \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)"
if [[ -z "${NICO_PG_POD}" ]]; then
    echo "ERROR: no nico-pg-cluster master pod found (cluster-name=nico-pg-cluster,spilo-role=master in the postgres namespace)." >&2
    echo "  Ensure postgresql.enabled=true in helm-prereqs/values.yaml and the nico-prereqs release has synced." >&2
    exit 1
fi
echo "nico-pg-cluster master pod: ${NICO_PG_POD}"

# ---------------------------------------------------------------------------
# Preflight: target database/user already provisioned by the postgres
# operator (i.e. the corresponding useHaPostgres toggle was applied before this
# script ran). Checked for every database up front, before any Deployment is
# scaled down, so a missing target aborts cleanly with nothing stopped.
# ---------------------------------------------------------------------------
_require_target_db() {
    local _db="$1"
    if ! kubectl exec -n postgres "${NICO_PG_POD}" -- \
        psql -U postgres -tAc "SELECT 1 FROM pg_database WHERE datname='${_db}'" 2>/dev/null | grep -q 1; then
        echo "ERROR: database '${_db}' does not exist on nico-pg-cluster yet." >&2
        echo "  Set the matching useHaPostgres toggle in ${PREREQS_DIR}/values.yaml and run 'helmfile sync' (or setup.sh) first." >&2
        exit 1
    fi
}

if [[ "${DB_TARGET}" == "temporal" || "${DB_TARGET}" == "both" ]]; then
    _require_target_db "temporal"
    _require_target_db "temporal_visibility"
fi
if [[ "${DB_TARGET}" == "keycloak" || "${DB_TARGET}" == "both" ]]; then
    _require_target_db "keycloak"
fi

# ---------------------------------------------------------------------------
# Scale a set of Deployments to zero (or back to their prior replica count).
# Replica counts are cached in the global _SAVED_REPLICAS_* vars, keyed by
# "<ns>/<deployment>", so a caller can scale a group of Deployments down once,
# dump/restore multiple databases while they're stopped, then scale the same
# group back up exactly once — otherwise workloads that read/write more than
# one database (Temporal writes both `temporal` and `temporal_visibility`)
# would resume against the legacy DB mid-migration, between two dump/restore
# calls, defeating the stop-the-world guarantee.
#
# _SCALED_GROUPS records every group currently scaled down, cleared by
# _scale_up once it restores that group. An EXIT trap scales up anything
# still in this set — including on a mid-migration failure (e.g. pg_dump/
# pg_restore itself failing) — so a script error never leaves a workload
# stuck at zero replicas.
# ---------------------------------------------------------------------------
declare -A _SAVED_REPLICAS
declare -A _SCALED_GROUPS

_cleanup_scaled_groups() {
    local _group _ns _deployments
    for _group in "${!_SCALED_GROUPS[@]}"; do
        echo "Cleanup: restoring replica counts for ${_group} after early exit..." >&2
        read -r _ns _deployments <<< "${_SCALED_GROUPS[${_group}]}"
        _scale_up "${_ns}" ${_deployments}
    done
}
trap _cleanup_scaled_groups EXIT

_scale_down() {
    local _ns="$1"
    shift
    local _deployments=("$@")
    local _dep _cur

    # Read every replica count BEFORE registering the group or scaling
    # anything down. No `|| echo 0` fallback: a transient kubectl failure
    # must not get cached as "was 0 replicas" — if a later step fails, the
    # EXIT trap would "restore" a genuinely-running workload to zero, a real
    # outage the trap exists to prevent, not cause. Reading everything first
    # (instead of read-then-scale per deployment) also means a failure here
    # never leaves _SCALED_GROUPS registered with entries _scale_up can't
    # find a saved count for.
    local -A _pending_replicas=()
    for _dep in "${_deployments[@]}"; do
        if ! _cur="$(kubectl get deploy "${_dep}" -n "${_ns}" -o jsonpath='{.spec.replicas}' 2>/dev/null)" \
            || [[ ! "${_cur}" =~ ^[0-9]+$ ]]; then
            echo "ERROR: could not read the current replica count for ${_ns}/${_dep} — refusing to guess it was 0" >&2
            exit 1
        fi
        _pending_replicas["${_dep}"]="${_cur}"
    done

    _SCALED_GROUPS["${_ns}"]="${_ns} ${_deployments[*]}"
    for _dep in "${_deployments[@]}"; do
        _cur="${_pending_replicas["${_dep}"]}"
        _SAVED_REPLICAS["${_ns}/${_dep}"]="${_cur}"
        echo "Scaling down ${_ns}/${_dep} (was ${_cur} replicas)..."
        _run kubectl scale deploy "${_dep}" -n "${_ns}" --replicas=0
    done
    if ! "${DRY_RUN}"; then
        for _dep in "${_deployments[@]}"; do
            # No `|| true`: a stuck rollout means a pod may still be writing
            # to postgres.postgres, so let this abort the migration — the
            # EXIT trap then restores the original replica count.
            kubectl rollout status deploy/"${_dep}" -n "${_ns}" --timeout=120s
        done
    fi
}

_scale_up() {
    local _ns="$1"
    shift
    local _deployments=("$@")
    local _dep
    echo "Restoring replica counts for ${_ns}..."
    for _dep in "${_deployments[@]}"; do
        _run kubectl scale deploy "${_dep}" -n "${_ns}" --replicas="${_SAVED_REPLICAS["${_ns}/${_dep}"]}"
    done
    unset "_SCALED_GROUPS[${_ns}]"
}

# Mark a group's migration as successfully complete: stop tracking it (so the
# EXIT trap won't touch it) WITHOUT scaling it back up — see "Why workloads
# stay stopped on success" above.
_disarm_group() {
    unset "_SCALED_GROUPS[$1]"
}

# ---------------------------------------------------------------------------
# Dump one database from the legacy pod and restore it into nico-pg-cluster,
# fixing ownership on the restored objects. Does NOT touch consumer replicas
# — callers scale down/up around one or more _dump_restore_db calls.
# ---------------------------------------------------------------------------
_dump_restore_db() {
    local _db="$1" _owner="$2"

    echo ""
    echo "--- Migrating database: ${_db} (owner: ${_owner}) ---"

    echo "Dumping ${_db} from ${LEGACY_PG_POD} and restoring into ${NICO_PG_POD} as ${_owner}..."
    # --role: pg_restore issues SET ROLE "<owner>" (permitted since postgres is
    # superuser) before creating each object, so everything is owned by
    # <owner> from the start. Restoring as plain -U postgres with --no-owner
    # instead leaves objects owned by postgres, and a later REASSIGN OWNED BY
    # postgres fails — postgres here is the actual initdb bootstrap superuser
    # (OID 10), and Postgres refuses to reassign objects owned by that role
    # ("required by the database system"), even scoped to one database.
    if "${DRY_RUN}"; then
        echo "  [dry-run] kubectl exec -n postgres ${LEGACY_PG_POD} -- pg_dump -U postgres -Fc --no-owner --no-acl ${_db} |" \
             "kubectl exec -i -n postgres ${NICO_PG_POD} -- pg_restore -U postgres -d ${_db} --no-owner --role=${_owner} --clean --if-exists"
    else
        kubectl exec -n postgres "${LEGACY_PG_POD}" -- \
            pg_dump -U postgres -Fc --no-owner --no-acl "${_db}" | \
            kubectl exec -i -n postgres "${NICO_PG_POD}" -- \
                pg_restore -U postgres -d "${_db}" --no-owner --role="${_owner}" --clean --if-exists
    fi

    echo "--- ${_db} migration complete ---"
}

if [[ "${DB_TARGET}" == "temporal" || "${DB_TARGET}" == "both" ]]; then
    _TEMPORAL_DEPLOYMENTS=(temporal-frontend temporal-history temporal-matching temporal-worker)
    _scale_down temporal "${_TEMPORAL_DEPLOYMENTS[@]}"
    _dump_restore_db "temporal" "temporal.nico"
    _dump_restore_db "temporal_visibility" "temporal.nico"
fi

if [[ "${DB_TARGET}" == "keycloak" || "${DB_TARGET}" == "both" ]]; then
    # values.yaml::keycloak.namespace is the canonical source (matches what
    # setup.sh and the ESO ClusterExternalSecret target); KEYCLOAK_NS lets an
    # operator override it explicitly, same as keycloak/setup.sh honors.
    _KEYCLOAK_NS="${KEYCLOAK_NS:-$(_yaml_toplevel_value "${PREREQS_DIR}/values.yaml" keycloak namespace)}"
    _KEYCLOAK_NS="${_KEYCLOAK_NS:-nico-rest}"
    _scale_down "${_KEYCLOAK_NS}" keycloak
    _dump_restore_db "keycloak" "keycloak.nico"
fi

# Disarm only once every requested migration above has succeeded (set -e
# means we never reach here if any of them failed) — with --db both, a
# Keycloak failure after Temporal's dump/restore must still leave Temporal
# tracked by the EXIT trap, or it would be stuck at zero replicas with no
# setup.sh run to bring it back (Temporal itself was never cut over).
if [[ "${DB_TARGET}" == "temporal" || "${DB_TARGET}" == "both" ]]; then
    _disarm_group temporal
fi
if [[ "${DB_TARGET}" == "keycloak" || "${DB_TARGET}" == "both" ]]; then
    _disarm_group "${_KEYCLOAK_NS}"
fi

echo ""
echo "=== Migration complete ==="
echo "Migrated workloads are left at zero replicas — they stay stopped until"
echo "setup.sh repoints them at nico-pg-cluster, so nothing writes to the"
echo "already-migrated legacy database in the meantime."
echo "Next steps:"
if [[ "${DB_TARGET}" == "temporal" || "${DB_TARGET}" == "both" ]]; then
    echo "  - Confirm temporal.useHaPostgres: true in ${PREREQS_DIR}/values.yaml"
fi
if [[ "${DB_TARGET}" == "keycloak" || "${DB_TARGET}" == "both" ]]; then
    echo "  - Confirm keycloak.useHaPostgres: true in ${PREREQS_DIR}/values.yaml"
fi
echo "  - Re-run setup.sh so phases 7d/7f point Temporal/Keycloak at nico-pg-cluster and scale workloads back up"
echo "  - Once verified, the legacy temporal/temporal_visibility/keycloak databases on postgres.postgres can be dropped"
