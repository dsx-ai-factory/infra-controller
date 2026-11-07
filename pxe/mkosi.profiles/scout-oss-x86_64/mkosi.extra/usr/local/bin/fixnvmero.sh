#!/bin/bash
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

devices=$(lsblk -n | grep " 1 disk" | awk '{print $1}')

for dev in ${devices}
do
	basedev=$(echo $dev | sed 's/n[0-9][0-9]*$//')
	ns=$(echo $dev | sed 's/.*n\([0-9][0-9]*\)$/\1/')
	bse=$(nvme id-ns /dev/${dev} | grep "in use" | awk '{print $5}' | cut -d: -f2)
	bs=$((2 ** $bse))
	cap=$(nvme id-ns /dev/${dev} | grep -i nvmcap | awk '{print $NF}')
	capb=$(($cap / $bs))
	ctrl=$(nvme id-ctrl /dev/${dev} | grep "^cntlid" | awk '{print $NF}')
	echo "Device ${dev} capacity ${cap} blocksize ${bs} blocks ${capb} controller ${ctrl}"

	nvme delete-ns /dev/${basedev} -n ${ns}
	nvme create-ns /dev/${basedev} --nsze=${capb} --ncap=${capb} --flbas=0 -dps=0
	nvme attach-ns /dev/${basedev} --namespace-id=${ns} -controllers=${ctrl}
	nvme ns-rescan /dev/${basedev}
done

