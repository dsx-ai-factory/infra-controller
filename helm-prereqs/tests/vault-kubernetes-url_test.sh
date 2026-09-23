#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
configure_script="$(
    helm template nico-prereqs "${SCRIPT_DIR}/.." \
        --show-only templates/vault-config-job.yaml |
        yq -er 'select(.kind == "Job") | .spec.template.spec.containers[] |
            select(.name == "configure") | .command[2]'
)"

# Run the rendered command without reading a service-account token or contacting
# Kubernetes or Vault. Stop after the auth configuration, before AppRole writes.
stubs='cat() { printf "test-token\n"; }
sed() { :; }
sleep() { exit 1; }
wget() {
    for arg; do :; done
    printf "wget-url=%s\n" "$arg"
}
vault() {
    case "$1 $2" in
        "write auth/kubernetes/config")
            shift 2
            printf "vault-arg=%s\n" "$@"
            exit 0
            ;;
        "secrets "*|"auth enable"|"write nicoca/"*) ;;
        *) echo "Unexpected Vault command: $*" >&2; exit 1 ;;
    esac
}'

while read -r label host port expected_url; do
    if ! actual="$(
        KUBERNETES_SERVICE_HOST="${host}" KUBERNETES_SERVICE_PORT="${port}" \
            /bin/sh -c "${stubs}
${configure_script}" |
            grep -E '^(wget-url|vault-arg)='
    )"; then
        printf '%s: configure command failed or produced no Kubernetes URLs\n%s\n' \
            "${label}" "${actual}" >&2
        exit 1
    fi
    expected="$(printf 'wget-url=%s/api/v1/namespaces/cert-manager/secrets/site-root\nvault-arg=kubernetes_host=%s' \
        "${expected_url}" "${expected_url}")"
    if [[ "${actual}" != "${expected}" ]]; then
        printf '%s: unexpected Kubernetes URLs\nExpected:\n%s\nActual:\n%s\n' \
            "${label}" "${expected}" "${actual}" >&2
        exit 1
    fi
    printf '%s Kubernetes URLs passed\n' "${label}"
done <<'CASES'
IPv4 10.96.0.1 443 https://10.96.0.1:443
IPv6 fd00:1234::1 6443 https://[fd00:1234::1]:6443
CASES
