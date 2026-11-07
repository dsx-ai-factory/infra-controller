#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALLER="${SCRIPT_DIR}/../observability/install-observability.sh"
summary="$(sed -n '/^echo "=== Observability install complete ==="/,/^echo "  Docs:/p' "${INSTALLER}")"
if [[ -z "${summary}" ]]; then
    echo "could not extract the observability install summary" >&2
    exit 1
fi

while IFS='|' read -r GRAFANA_VIP expected; do
    output="$(GRAFANA_VIP="${GRAFANA_VIP}" WITH_TEMPO=false WITH_DPU=false bash -euo pipefail -c "${summary}")"
    if [[ "${output}" != *"  Grafana:    ${expected}"* ]]; then
        printf 'VIP %s: expected Grafana output %s, got:\n%s\n' "${GRAFANA_VIP:-<unset>}" "${expected}" "${output}" >&2
        exit 1
    fi
done <<'CASES'
2001:db8::10|http://[2001:db8::10] (VIP)
192.0.2.10|http://192.0.2.10 (VIP)
|kubectl -n monitoring port-forward svc/obs-grafana 3000:80  ->  http://localhost:3000
CASES

echo "install-observability summary tests passed"
