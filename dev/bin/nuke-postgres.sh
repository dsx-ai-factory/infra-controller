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
###this script is intended to be used by developers against minikube to delete and recreate the forge database.
###It assumes that the API server is already down, usually accomplished by bringing down skaffold prior to running the script.

MAX_RETRY=10
i=0
while [[ $i -lt $MAX_RETRY ]]; do
  echo "Attempting to delete forge DB."


  #something is holding the DB connection -- so murder it, i don't care this is local get out of the way.
  kubectl exec -ti forge-pg-cluster-0  -n postgres -- /usr/bin/psql -U postgres -c "SELECT pg_terminate_backend(pg_stat_activity.pid)
FROM pg_stat_activity
WHERE datname = 'forge_system_carbide'
  AND pid <> pg_backend_pid();"

  kubectl exec -ti forge-pg-cluster-0  -n postgres -- /usr/bin/psql -U postgres -c "DROP DATABASE forge_system_carbide;"
  if [ $? -eq 0 ]; then
      echo "forge DB successfully deleted"
      break
  else
      echo "DB still has connections, waiting to retry."
      sleep 2
  fi

  i=$((i+1))
done

echo "Recreating forge db"
kubectl exec -ti forge-pg-cluster-0  -n postgres -- /usr/bin/psql -U postgres -c 'CREATE DATABASE forge_system_carbide with owner "forge-system.carbide";'


