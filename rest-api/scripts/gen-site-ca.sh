#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# -------------------------------------------------------------------
# gen-site-ca.sh — Generate a root CA and create the ca-signing-secret
#                  required by nico-rest-cert-manager and the
#                  nico-rest-ca-issuer ClusterIssuer.
#
# Usage:
#   ./scripts/gen-site-ca.sh [OPTIONS]
#
# Options:
#   --namespace <ns>   Namespace for nico-rest workloads (default: nico-rest)
#   --output-dir <dir> Write ca.crt and ca.key to this directory instead of
#                      applying directly to the cluster. Does not run kubectl.
#   --cn <string>      Common Name for the CA (default: "NICo Local Dev CA")
#   --org <string>     Organization for the CA (default: "NVIDIA")
#   --days <n>         Validity period in days (default: 3650)
#   --dry-run          Print the kubectl commands that would be run, do not apply
#   -h, --help         Show this help
#
# What it creates:
#   Secret "ca-signing-secret" (type: kubernetes.io/tls) in:
#     - <namespace>     (used by nico-rest-cert-manager and cert-manager.io ClusterIssuer)
#     - cert-manager    (required if cert-manager reads the secret from its own namespace)
#
# Examples:
#   # Apply directly to the cluster
#   ./scripts/gen-site-ca.sh
#
#   # Write cert files to disk, apply manually later
#   ./scripts/gen-site-ca.sh --output-dir /tmp/nico-ca
#
#   # Custom CN, apply to a non-default namespace
#   ./scripts/gen-site-ca.sh --cn "My Corp CA" --namespace my-nico-ns
# -------------------------------------------------------------------

set -eEuo pipefail

die()  { echo "❌  $*" >&2; exit 1; }
info() { echo "ℹ️   $*"; }
ok()   { echo "✅  $*"; }
warn() { echo "⚠️   $*"; }

# ---- Defaults -------------------------------------------------------
NAMESPACE="nico-rest"
OUTPUT_DIR=""
CA_CN="NICo Local Dev CA"
CA_ORG="NVIDIA"
CA_DAYS=3650
DRY_RUN=false

# ---- Parse args -----------------------------------------------------
while [[ $# -gt 0 ]]; do
  case "$1" in
    --namespace)   NAMESPACE="${2:?--namespace requires a value}";  shift 2 ;;
    --output-dir)  OUTPUT_DIR="${2:?--output-dir requires a value}"; shift 2 ;;
    --cn)          CA_CN="${2:?--cn requires a value}";             shift 2 ;;
    --org)         CA_ORG="${2:?--org requires a value}";           shift 2 ;;
    --days)        CA_DAYS="${2:?--days requires a value}";         shift 2 ;;
    --dry-run)     DRY_RUN=true;                                    shift   ;;
    -h|--help)
      sed -n '/^# Usage:/,/^# ---/p' "$0" | sed 's/^# \{0,3\}//'
      exit 0
      ;;
    *) die "Unknown option: $1" ;;
  esac
done

command -v openssl >/dev/null 2>&1 || die "'openssl' not found in PATH"
if [[ -z "$OUTPUT_DIR" ]] && [[ "$DRY_RUN" == "false" ]]; then
  command -v kubectl >/dev/null 2>&1 || die "'kubectl' not found in PATH"
fi

# ---- Generate CA in a temp dir -------------------------------------
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

info "Generating RSA 4096 root CA (validity: ${CA_DAYS} days)…"

cat > "$TMP_DIR/ca.cnf" <<EOF
[req]
distinguished_name = req_distinguished_name
x509_extensions    = v3_ca
prompt             = no

[req_distinguished_name]
C  = US
ST = CA
L  = Local
O  = ${CA_ORG}
CN = ${CA_CN}

[v3_ca]
basicConstraints       = critical,CA:TRUE
keyUsage               = critical,keyCertSign,cRLSign,digitalSignature
subjectKeyIdentifier   = hash
authorityKeyIdentifier = keyid:always,issuer:always
EOF

openssl req -x509 -sha256 -nodes -newkey rsa:4096 \
  -keyout "$TMP_DIR/ca.key" \
  -out    "$TMP_DIR/ca.crt" \
  -days   "$CA_DAYS" \
  -config "$TMP_DIR/ca.cnf" \
  -extensions v3_ca \
  2>/dev/null

ok "CA generated (CN: ${CA_CN}, O: ${CA_ORG})"

# ---- Output-dir mode: just write files, no kubectl -----------------
if [[ -n "$OUTPUT_DIR" ]]; then
  mkdir -p "$OUTPUT_DIR"
  cp "$TMP_DIR/ca.crt" "$OUTPUT_DIR/ca.crt"
  cp "$TMP_DIR/ca.key" "$OUTPUT_DIR/ca.key"
  ok "Written to: ${OUTPUT_DIR}/ca.crt and ${OUTPUT_DIR}/ca.key"
  echo
  info "To create the secret manually:"
  echo "  kubectl create secret tls ca-signing-secret \\"
  echo "    --cert=${OUTPUT_DIR}/ca.crt \\"
  echo "    --key=${OUTPUT_DIR}/ca.key \\"
  echo "    -n ${NAMESPACE} --dry-run=client -o yaml | kubectl apply -f -"
  echo
  echo "  kubectl create secret tls ca-signing-secret \\"
  echo "    --cert=${OUTPUT_DIR}/ca.crt \\"
  echo "    --key=${OUTPUT_DIR}/ca.key \\"
  echo "    -n cert-manager --dry-run=client -o yaml | kubectl apply -f -"
  exit 0
fi

# ---- Build kubectl commands ----------------------------------------
APPLY_NS_CMD="kubectl create secret tls ca-signing-secret \
  --cert=$TMP_DIR/ca.crt \
  --key=$TMP_DIR/ca.key \
  -n $NAMESPACE \
  --dry-run=client -o yaml | kubectl apply -f -"

APPLY_CM_CMD="kubectl create secret tls ca-signing-secret \
  --cert=$TMP_DIR/ca.crt \
  --key=$TMP_DIR/ca.key \
  -n cert-manager \
  --dry-run=client -o yaml | kubectl apply -f -"

if [[ "$DRY_RUN" == "true" ]]; then
  warn "Dry-run mode — commands that would be run:"
  echo "  $APPLY_NS_CMD"
  echo "  $APPLY_CM_CMD"
  exit 0
fi

# ---- Apply CA secrets to cluster ----------------------------------
info "Creating ca-signing-secret in namespace '${NAMESPACE}'…"
eval "$APPLY_NS_CMD"
ok "ca-signing-secret created in '${NAMESPACE}'"

info "Creating ca-signing-secret in namespace 'cert-manager'…"
if kubectl get namespace cert-manager >/dev/null 2>&1; then
  eval "$APPLY_CM_CMD"
  ok "ca-signing-secret created in 'cert-manager'"
else
  warn "Namespace 'cert-manager' not found — skipping. Create it after cert-manager is installed:"
  echo "  $APPLY_CM_CMD"
fi

echo
ok "Done. Next step: apply the ClusterIssuer:"
echo "  kubectl apply -k deploy/kustomize/base/cert-manager-io"
