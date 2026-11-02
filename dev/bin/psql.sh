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

SQL_QUERY=$1

if [ "$FORGE_BOOTSTRAP_KIND" == "kube" ]; then
  # shellcheck disable=SC2016 # Expand the database settings inside the container.
  kubectl exec --context minikube --namespace forge-system -it deploy/carbide-api -- bash -c '
    # PGHOST takes a bare address, without the brackets used in PostgreSQL URLs.
    datastore_host="$DATASTORE_HOST"
    case "$datastore_host" in
      \[*\]) datastore_host=${datastore_host#\[}; datastore_host=${datastore_host%\]} ;;
    esac
    PGHOST="$datastore_host" PGPORT="$DATASTORE_PORT" \
      PGUSER="$DATASTORE_USER" PGPASSWORD="$DATASTORE_PASSWORD" \
      PGDATABASE="$DATASTORE_NAME" psql -P pager=off -t -c "$1"
  ' psql-query "$SQL_QUERY"
else
  psql -t --quiet -P pager=off -c "${SQL_QUERY}"
fi
