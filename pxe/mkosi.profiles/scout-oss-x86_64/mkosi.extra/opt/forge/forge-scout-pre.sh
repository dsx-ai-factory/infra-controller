#!/usr/bin/env sh
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
for i in `cat /proc/cmdline`
do
        line=`echo $i|grep machine_id`
        if [ ! -z "$line" ] ;
        then
                machine_id=`echo $line|cut -d'=' -f2`
        fi
        line=`echo $i|grep server_uri`
        if [ ! -z "$line" ] ;
        then
                server_uri=`echo $line|cut -d'=' -f2`
        fi
        line=`echo $i|grep pxe_uri`
        if [ ! -z "$line" ] ;
        then
                pxe_uri=`echo $line|cut -d'=' -f2`
        fi
        line=`echo $i|grep cli_cmd`
        if [ ! -z "$line" ] ;
        then
                cli_cmd=`echo $line|cut -d'=' -f2`
        fi
done

rm "/opt/forge/forge-scout.env"
cat "/opt/forge/forge-scout.env.template" > "/opt/forge/forge-scout.env"
echo server_uri=$server_uri >> "/opt/forge/forge-scout.env"
echo machine_id=$machine_id >> "/opt/forge/forge-scout.env"
echo cli_cmd=$cli_cmd >> "/opt/forge/forge-scout.env"

# Use the PXE URL from the kernel command line if available (supports
# external hosts that can't resolve internal hostnames), otherwise fall
# back to the default internal hostname.
pxe_base_url=${pxe_uri:-http://carbide-pxe.forge}
curl --retry 5 --retry-all-errors -v -o /opt/forge/forge_root.pem ${pxe_base_url}/api/v0/tls/root_ca

mkdir -p ~/.ssh

if [ ! -f ~/.ssh/privatekey.pem ]; then
	ssh-keygen -t rsa -b 4096 -f ~/.ssh/privatekey.pem -q -N ""
	ssh-keyscan -p "22" 127.0.0.1 > ~/.ssh/known_hosts
	cat ~/.ssh/privatekey.pem.pub >> ~/.ssh/authorized_keys
fi

rmmod mlx5_ib || true
udevadm settle
modprobe mlx5_ib || true
udevadm settle
chown nvidia-dcgm:nvidia-dcgm /var/log/nvidia-dcgm
/opt/forge/load_modules.sh || true
udevadm settle
/opt/forge/forge-scout-network.sh || echo "Scout NIC filtering failed; continuing startup" >&2
