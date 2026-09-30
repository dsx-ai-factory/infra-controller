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

# =============================================================================
# ingestion-rate-report.sh — exact ingestion curves from the DB's own clocks
#
# Reads the timestamps the pipeline itself recorded, which makes runs exactly
# comparable for the #3738 knob campaign:
#   * machines.created            → machine-creation curve (per-minute + p50/p90 gap)
#   * machine_interfaces.created  → interface registration curve
# Output is plain text; pass --csv for machine-readable per-minute buckets.
#
# Environment:
#   KUBECONFIG, POSTGRES_NS (postgres), NICO_DB (nico_system_nico)
# =============================================================================
set -euo pipefail

POSTGRES_NS="${POSTGRES_NS:-postgres}"
NICO_DB="${NICO_DB:-nico_system_nico}"
CSV=false
[[ "${1:-}" == "--csv" ]] && CSV=true

PG="$(kubectl get pods -n "$POSTGRES_NS" -l application=spilo \
    -o jsonpath='{range .items[*]}{.metadata.name} {.metadata.labels.spilo-role}{"\n"}{end}' 2>/dev/null \
    | awk '$2=="master"{print $1}' | head -1)"
[[ -n "$PG" ]] || { echo "ERROR: no Patroni primary in $POSTGRES_NS" >&2; exit 1; }
# Surface query failures rather than returning an empty string that later
# parses as a zero row.
q() {
    local out rc
    out="$(kubectl exec -n "$POSTGRES_NS" "$PG" -- su postgres -c "psql -d $NICO_DB -v ON_ERROR_STOP=1 -tAc \"$1\"" 2>&1)"; rc=$?
    if (( rc != 0 )); then
        echo "ERROR: query failed (rc=$rc): ${out}" >&2
        return "$rc"
    fi
    printf '%s' "$out"
}

report_curve() {   # $1 = table  $2 = label
    local tbl="$1" label="$2"
    local stats
    stats="$(q "SELECT count(*) || '|' || min(created) || '|' || max(created) || '|' ||
        round(extract(epoch FROM (max(created) - min(created)))) || '|' ||
        round(extract(epoch FROM (percentile_cont(0.5) WITHIN GROUP (ORDER BY created) - min(created)))) || '|' ||
        round(extract(epoch FROM (percentile_cont(0.9) WITHIN GROUP (ORDER BY created) - min(created))))
        FROM ${tbl};")"
    [[ -n "$stats" ]] || { echo "${label}: no rows"; return; }
    IFS='|' read -r n first last span p50 p90 <<< "$stats"
    echo "${label}: ${n} rows over ${span}s (first ${first}, last ${last})"
    echo "  p50 at +${p50}s, p90 at +${p90}s, avg $(awk -v n="$n" -v s="$span" 'BEGIN{ if (s>0) printf "%.1f", n*60/s; else printf "n/a" }')/min"
    if $CSV; then
        echo "  per-minute buckets (${label}):"
        q "SELECT date_trunc('minute', created) || ',' || count(*)
           FROM ${tbl} GROUP BY 1 ORDER BY 1;" | sed 's/^/    /'
    fi
}

report_curve machines "machines.created"
report_curve machine_interfaces "machine_interfaces.created"

# Terminal-state distribution for context (a knob that destabilizes the
# pipeline shows up as machines wedged mid-state, not just as a slower rate).
echo "state distribution:"
q "SELECT '  ' || coalesce(controller_state->>'state','?') || ': ' || count(*)
   FROM machines GROUP BY 1 ORDER BY 2 DESC;"
