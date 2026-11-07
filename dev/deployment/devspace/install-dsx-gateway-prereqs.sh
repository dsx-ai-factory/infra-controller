#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
AGENTGATEWAY_VERSION="v1.4.1"

GATEWAY_API_MANIFEST="$("${SCRIPT_DIR}/prepare-dsx-exchange.sh" --gateway-api-manifest)"

kubectl apply --server-side -f "${GATEWAY_API_MANIFEST}" >/dev/null
kubectl wait --for=condition=Established --timeout=120s \
  crd/gateways.gateway.networking.k8s.io >/dev/null

helm upgrade --install agentgateway-crds \
  oci://cr.agentgateway.dev/charts/agentgateway-crds \
  --namespace agentgateway-system \
  --create-namespace \
  --version "${AGENTGATEWAY_VERSION}" \
  --wait \
  --timeout 5m >/dev/null

kubectl wait --for=condition=Established --timeout=120s \
  crd/agentgatewaybackends.agentgateway.dev \
  crd/agentgatewayparameters.agentgateway.dev \
  crd/agentgatewaypolicies.agentgateway.dev >/dev/null
