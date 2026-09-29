#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Renew a Site Agent's Temporal client certificate by hand when the automated
# rotation failed or the certificate already expired. Rolls the Site's OTP in
# Site Manager, writes the new OTP to the registration Secret, and restarts the
# Site Agent so it downloads a new certificate. Uses the current kubeconfig context.
#
#   renew-site-agent-temporal-cert.sh [--dry-run] [--yes]
#
# --dry-run reports the current state without changing anything. Set REST_NS
# when more than one namespace runs a Site Agent.
set -euo pipefail

CONFIGMAP="nico-rest-site-agent-config"
POD="nico-rest-site-agent-0"
REGISTRATION_SECRET="site-registration"
SITE_MANAGER_SVC="nico-rest-site-manager"
LOCAL_PORT="${LOCAL_PORT:-18100}"

usage="Usage: $0 [--dry-run] [--yes]"
dry_run=false
assume_yes=false
for arg in "$@"; do
    case "${arg}" in
        --dry-run) dry_run=true ;;
        --yes) assume_yes=true ;;
        -h | --help) echo "${usage}"; exit 0 ;;
        *) echo "${usage}" >&2; exit 2 ;;
    esac
done

log() { echo "$(date -u +%H:%M:%S) $*" >&2; }
die() { echo "ERROR: $*" >&2; exit 1; }
k() { kubectl -n "${REST_NS}" "$@"; }
json_field() { python3 -c 'import json, sys; print(json.load(sys.stdin).get(sys.argv[1], ""))' "$1"; }
cert() { k get secret "${temporal_secret}" -o jsonpath='{.data.certificate}'; }
expiry() {
    base64 -d <<<"$1" | python3 -c 'import ssl; print(ssl._ssl._test_decode_cert("/dev/stdin")["notAfter"])' \
        2>/dev/null || echo unknown
}
can_i() {
    [[ "$(kubectl auth can-i -n "${REST_NS}" "$1" "$2" 2>/dev/null)" == yes ]] ||
        die "the current context cannot $1 $2"
}

# The StatefulSet recreates the pod under the same name, so wait for a new UID.
restart_site_agent() {
    local old uid
    old="$(k get pod "${POD}" -o jsonpath='{.metadata.uid}')"
    log "Restarting ${POD}"
    k delete pod "${POD}" --wait=false >/dev/null
    for _ in $(seq 60); do
        uid="$(k get pod "${POD}" -o jsonpath='{.metadata.uid}' 2>/dev/null || true)"
        [[ -n "${uid}" && "${uid}" != "${old}" ]] && break
        sleep 5
    done
    k wait --for=condition=Ready "pod/${POD}" --timeout=5m >/dev/null || die "${POD} did not become Ready"
}

REST_NS="${REST_NS:-$(kubectl get configmap -A --field-selector "metadata.name=${CONFIGMAP}" \
    -o jsonpath='{.items[*].metadata.namespace}' 2>/dev/null || true)}"
[[ -n "${REST_NS}" && "${REST_NS}" != *" "* ]] ||
    die "cannot tell which namespace runs the Site Agent (found '${REST_NS}'), set REST_NS"

site_id="$(k get configmap "${CONFIGMAP}" -o jsonpath='{.data.CLUSTER_ID}')"
registered_id="$(k get secret "${REGISTRATION_SECRET}" -o jsonpath='{.data.site-uuid}' | base64 -d)"
[[ -n "${site_id}" && "${site_id}" == "${registered_id}" ]] ||
    die "${CONFIGMAP} has CLUSTER_ID '${site_id}', but ${REGISTRATION_SECRET} is for '${registered_id}'"
[[ "$(k get configmap "${CONFIGMAP}" -o jsonpath='{.data.DISABLE_BOOTSTRAP}' | tr '[:upper:]' '[:lower:]')" != true ]] ||
    die "DISABLE_BOOTSTRAP is true, so the Site Agent would not download a new certificate"
temporal_secret="$(k get configmap "${CONFIGMAP}" -o jsonpath='{.data.TEMPORAL_CERT}')"
temporal_secret="${temporal_secret:-temporal-client-site-agent-certs}"
can_i patch "secret/${REGISTRATION_SECRET}"
can_i delete "pod/${POD}"
can_i create pods/portforward

old_cert="$(cert)"
log "Site ${site_id}, namespace ${REST_NS}, context $(kubectl config current-context)"
log "Temporal certificate expires $(expiry "${old_cert}")"

kubectl -n "${REST_NS}" port-forward "svc/${SITE_MANAGER_SVC}" "${LOCAL_PORT}:8100" >/dev/null 2>&1 &
pf_pid=$!
trap 'kill "${pf_pid}" 2>/dev/null || true' EXIT
site_manager="https://127.0.0.1:${LOCAL_PORT}/v1/site"
for _ in $(seq 20); do
    curl -sk -o /dev/null "${site_manager}" && break
    sleep 0.5
done
state="$(curl -sfk "${site_manager}/${site_id}" | json_field bootstrapstate)" ||
    die "cannot read Site ${site_id} from Site Manager"
log "Site Manager bootstrap state is ${state}"

if "${dry_run}"; then
    log "Dry run, nothing changed"
    exit 0
fi
if ! "${assume_yes}"; then
    read -r -p "Renew the Temporal certificate of Site ${site_id}? [y/N] " answer
    [[ "${answer}" == [yY] ]] || die "aborted"
fi

# Keep the OTP out of shell tracing and command lines. kubectl reads the patch from stdin.
restore_xtrace=false
if [[ $- == *x* ]]; then
    set +x
    restore_xtrace=true
fi
curl -sfk -X POST "${site_manager}/roll/${site_id}" >/dev/null || die "Site Manager did not roll the OTP"
site="$(curl -sfk "${site_manager}/${site_id}")"
otp="$(json_field otp <<<"${site}")"
[[ -n "${otp}" && "$(json_field bootstrapstate <<<"${site}")" == AwaitHandshake ]] ||
    die "Site Manager did not issue a new OTP"
printf '{"stringData":{"otp":"%s"}}' "${otp}" |
    k patch secret "${REGISTRATION_SECRET}" --type merge --patch-file /dev/stdin >/dev/null
unset site otp
if "${restore_xtrace}"; then
    set -x
fi
log "Rolled the OTP and wrote it to ${REGISTRATION_SECRET}"

restart_site_agent
for _ in $(seq 36); do
    new_cert="$(cert)"
    [[ "${new_cert}" != "${old_cert}" ]] && break
    sleep 5
done
[[ "${new_cert}" != "${old_cert}" ]] || die "no new certificate after 3m, check: kubectl -n ${REST_NS} logs ${POD}"
log "New Temporal certificate expires $(expiry "${new_cert}")"

# The Site Agent loaded the old certificate files at startup. A new pod mounts the
# updated Secret, so restart again instead of waiting for a file watcher reload.
restart_site_agent
for _ in $(seq 24); do
    logs="$(k logs "${POD}")"
    if grep -qE 'expired certificate|Failed to create Temporal client' <<<"${logs}"; then
        die "the Site Agent cannot connect to Temporal, check: kubectl -n ${REST_NS} logs ${POD}"
    fi
    if grep -q 'Started Worker' <<<"${logs}"; then
        log "Done, the Site Agent is connected to Temporal"
        exit 0
    fi
    sleep 5
done
die "the Site Agent did not start its Temporal worker within 2m, check: kubectl -n ${REST_NS} logs ${POD}"
