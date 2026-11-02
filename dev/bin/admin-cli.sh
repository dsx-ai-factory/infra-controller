#!/usr/bin/env bash
#
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

CLI_ARGS="$@"

if [ "$FORGE_BOOTSTRAP_KIND" == "kube" ]; then
  kubectl exec --context minikube --namespace forge-system -it deploy/carbide-api -- bash -c \
      "/opt/carbide/forge-admin-cli --forge-root-ca-path=/var/run/secrets/spiffe.io/ca.crt --client-cert-path=/var/run/secrets/spiffe.io/tls.crt --client-key-path=/var/run/secrets/spiffe.io/tls.key -c https://carbide-api.forge-system.svc.cluster.local:\${CARBIDE_API_SERVICE_PORT} $CLI_ARGS"
else
  # docker-compose case
  source "$(dirname "${BASH_SOURCE[0]}")/host_port.sh" || exit $?
  API_URL="https://$(host_port "$API_SERVER_HOST" "$API_SERVER_PORT")"

  API_CONTAINER=$(docker ps | grep carbide-api | awk -F" " '{print $NF}')

  echo docker exec -ti ${API_CONTAINER} /opt/forge-admin-cli/debug/forge-admin-cli -c "$API_URL" --client-cert-path=/opt/forge/server_identity.pem --client-key-path=/opt/forge/server_identity.key $CLI_ARGS
  docker exec -ti ${API_CONTAINER} /opt/forge-admin-cli/debug/forge-admin-cli -c "$API_URL" --client-cert-path=/opt/forge/server_identity.pem --client-key-path=/opt/forge/server_identity.key $CLI_ARGS
fi
