/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Projects identified host cards into the MLX API view and joins devices to their BMCs by serial.

use std::collections::HashMap;

use carbide_hw_identification::host_dpu::{DpuProduct, HostDpu, HostDpuRules};
use model::site_explorer::{ExploredEndpoint, ExploredMlxDevice, MlxDeviceKind};

/// DPU endpoints supply BMC details for this report. Its device rows come from host endpoints.
pub(super) fn hosts_with_dpus<'hosts, 'rules>(
    endpoints: &'hosts [ExploredEndpoint],
    rules: &'rules HostDpuRules,
) -> impl Iterator<Item = (&'hosts ExploredEndpoint, Vec<HostDpu<'hosts, 'rules>>)> {
    endpoints
        .iter()
        .filter(|ep| !ep.report.is_dpu())
        .map(move |host| (host, rules.identify_dpus(&host.report)))
        .filter(|(_, cards)| !cards.is_empty())
}

pub(super) fn devices_for_hosts(
    hosts: &[ExploredEndpoint],
    rules: &HostDpuRules,
) -> Vec<ExploredMlxDevice> {
    hosts_with_dpus(hosts, rules)
        .flat_map(|(host, cards)| {
            cards.into_iter().map(|card| ExploredMlxDevice {
                host_bmc_ip: host.address,
                machine_id: host.report.machine_id,
                device_kind: match card.rule.product {
                    DpuProduct::BlueField2Dpu => MlxDeviceKind::Bf2Dpu,
                    DpuProduct::BlueField3Dpu => MlxDeviceKind::Bf3DpuMode,
                    DpuProduct::BlueField3SuperNic9D3B4 => MlxDeviceKind::Bf3NicMode,
                    DpuProduct::BlueField3SuperNic => MlxDeviceKind::Bf3SuperNic,
                    DpuProduct::BlueField4Dpu | DpuProduct::UnspecifiedBlueField => {
                        MlxDeviceKind::Unknown
                    }
                },
                pcie_id: card.pcie_device.and_then(|pcie| pcie.id.clone()),
                part_number: Some(card.part_number.to_owned()),
                serial_number: card.serial_number.map(str::to_owned),
                firmware_version: card.firmware_version.map(str::to_owned),
                description: card.pcie_device.and_then(|pcie| pcie.description.clone()),
                dpu_bmc_ip: None,
                nic_mode: None,
            })
        })
        .collect()
}

pub(super) fn attach_dpu_endpoints(
    devices: &mut [ExploredMlxDevice],
    endpoints: &[ExploredEndpoint],
) {
    let mut by_serial = HashMap::new();
    for endpoint in endpoints {
        if let Some(serial) = endpoint.report.dpu_pairing_serial_number() {
            // Multiple BMCs claiming the same serial make this association ambiguous.
            by_serial
                .entry(serial)
                .and_modify(|entry| *entry = None)
                .or_insert(Some(endpoint));
        }
    }
    for device in devices {
        if let Some(Some(endpoint)) = device
            .serial_number
            .as_deref()
            .map(str::trim)
            .and_then(|s| by_serial.get(s))
        {
            device.dpu_bmc_ip = Some(endpoint.address);
            device.nic_mode = endpoint.report.bluefield_operating_mode();
        }
    }
}

#[cfg(test)]
mod tests;
