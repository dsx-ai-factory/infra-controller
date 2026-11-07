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
if [ $# -lt 2 ]
then
    echo "Usage: $0 <control-plane-node> <dpu machine id> [<ssh-args>...]"
    exit 1
fi

set -e

rawout=$(echo y | TERM=xterm ssh -tt $1 sudo kubectl exec -qti deploy/carbide-api -n forge-system -- bash -c "'"'/opt/carbide/forge-admin-cli -c https://${CARBIDE_API_SERVICE_HOST}:${CARBIDE_API_SERVICE_PORT} machine show --machine='$2' 2> /dev/null  && sleep 1 && /opt/carbide/forge-admin-cli -f json -c https://${CARBIDE_API_SERVICE_HOST}:${CARBIDE_API_SERVICE_PORT} machine dpu-ssh-credentials --query='$2' 2> /dev/null '"'" 2> /dev/null | col -b)
#echo ---${rawout}---

address=$(echo $rawout | sed -e 's/.*Addresses : \([0-9.]*\).*/\1/g')
user=$(echo $rawout | sed -e 's/.*"username" *: *"\([^ ]*\)".*/\1/g')
export SSHPASS=$(echo $rawout | sed -e 's/.*"password" *: *"\([^ ]*\)".*/\1/g')

shift 2
sshpass -e ssh $user@$address "$@"
#echo address: $address
#echo user: $user
#echo pass: $SSHPASS
