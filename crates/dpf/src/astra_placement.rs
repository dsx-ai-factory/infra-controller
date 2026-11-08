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

//! Validated per-device Astra placement values for DPF template rendering.

use model::expected_machine::{ExpectedInterface, ExpectedMachine, validate_astra_placements};
use serde::Serialize;

use crate::DpfError;

/// Validated per-device rendering values.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct PlacedPort {
    pub(crate) group_id: String,
    pub(crate) hw_plane: u32,
    pub(crate) pci_address: String,
    pub(crate) phys_port_name: String,
    pub(crate) cerebro_ifname: String,
    pub(crate) logical_ifname: String,
}

pub(crate) fn ports_for_dpu(
    machine: &ExpectedMachine,
    serial: &str,
) -> Result<Option<Vec<PlacedPort>>, DpfError> {
    validate_astra_placements(&machine.data.interfaces)
        .map_err(|message| DpfError::ConfigError(message.to_string()))?;
    let mut ports = Vec::new();
    for interface in &machine.data.interfaces {
        let Some(p) = &interface.astra_placement else {
            continue;
        };
        if p.dpu_serial_number != serial {
            continue;
        }
        ports.push(PlacedPort {
            group_id: p.group_id.clone(),
            hw_plane: p.hw_plane,
            pci_address: p.pci_address.clone(),
            phys_port_name: p.phys_port_name.clone(),
            cerebro_ifname: interface.physical_ifname.clone().unwrap(),
            logical_ifname: interface.logical_ifname.clone().unwrap(),
        });
    }
    if ports.is_empty() {
        return Ok(None);
    }
    ports.sort_by(|a, b| (&a.group_id, a.hw_plane).cmp(&(&b.group_id, b.hw_plane)));
    Ok(Some(ports))
}

/// Explicit placement selects this DPU's complete inventory; other DPUs retain unplaced declarations.
pub(crate) fn interfaces_for_dpu<'a>(
    machine: &'a ExpectedMachine,
    serial: &str,
) -> Vec<&'a ExpectedInterface> {
    let explicit = machine.data.interfaces.iter().any(|nic| {
        nic.astra_placement
            .as_ref()
            .is_some_and(|p| p.dpu_serial_number == serial)
    });
    machine
        .data
        .interfaces
        .iter()
        .filter(|nic| {
            nic.nic_type.as_deref() == Some("CX9")
                && match &nic.astra_placement {
                    Some(placement) => placement.dpu_serial_number == serial,
                    None => !explicit,
                }
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn test_machine(serial: &str, bus_offset: u8) -> ExpectedMachine {
    use model::expected_machine::{AstraPortPlacement, ExpectedMachineData};
    let groups = ["A53", "A56", "A43", "A46", "A13", "A16", "A3", "A6"];
    let mut interfaces = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        for plane in 0..4 {
            interfaces.push(ExpectedInterface {
                physical_ifname: Some(format!(
                    "C{}-{}-L{}",
                    index / 2 * 2 + 1,
                    index % 2 + 1,
                    plane + 1
                )),
                logical_ifname: Some(format!("eth_r{}_p{}", index / 2, index % 2 * 4 + plane)),
                nic_type: Some("CX9".to_string()),
                astra_placement: Some(AstraPortPlacement {
                    dpu_serial_number: serial.to_string(),
                    group_id: group.to_string(),
                    hw_plane: plane as u32,
                    pci_address: format!("0000:{:02x}:00.{plane}", bus_offset as usize + index),
                    phys_port_name: format!("p{plane}"),
                }),
                ..Default::default()
            });
        }
    }
    ExpectedMachine {
        id: None,
        bmc_mac_address: "02:00:00:00:00:01".parse().unwrap(),
        data: ExpectedMachineData {
            interfaces,
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_validation_and_dpu_scoping() {
        for scenario in [
            "valid",
            "missing plane",
            "duplicate BDF",
            "duplicate identity",
            "wrong rail",
            "unsafe port",
            "invalid BDF",
            "unknown group",
            "invalid plane",
            "nonzero domain",
        ] {
            let mut machine = test_machine("dpu-one", 3);
            let first = machine.data.interfaces[0].clone();
            let port = machine.data.interfaces[0].astra_placement.as_mut().unwrap();
            match scenario {
                "unsafe port" => port.phys_port_name = "../p0".to_string(),
                "invalid BDF" => port.pci_address = "0000:03:20.0".to_string(),
                "unknown group" => port.group_id = "OTHER".to_string(),
                "invalid plane" => port.hw_plane = 4,
                "nonzero domain" => port.pci_address = "0005:03:00.0".to_string(),
                "missing plane" => {
                    machine.data.interfaces.pop();
                }
                "duplicate BDF" => {
                    machine.data.interfaces[1]
                        .astra_placement
                        .as_mut()
                        .unwrap()
                        .pci_address = first.astra_placement.unwrap().pci_address;
                }
                "duplicate identity" => {
                    machine.data.interfaces[1].physical_ifname = first.physical_ifname
                }
                "wrong rail" => {
                    machine.data.interfaces[0].logical_ifname = Some("eth_r3_p0".to_string())
                }
                _ => (),
            }
            assert_eq!(
                validate_astra_placements(&machine.data.interfaces).is_ok(),
                matches!(scenario, "valid" | "nonzero domain"),
                "{scenario}"
            );
        }
    }

    #[test]
    fn placement_values_preserve_order_and_dpu_scoping() {
        let first = test_machine("dpu-one", 3);
        let mut second = test_machine("dpu-two", 3);
        second.data.server_name = Some("another-host".to_string());
        second.data.interfaces.reverse();
        second.data.interfaces[0].fixed_ip = Some("100.96.0.10".parse().unwrap());
        let ports = |machine: &ExpectedMachine, serial| {
            serde_json::to_value(ports_for_dpu(machine, serial).unwrap().unwrap()).unwrap()
        };
        assert_eq!(ports(&first, "dpu-one"), ports(&second, "dpu-two"));
        assert_ne!(
            ports(&first, "dpu-one"),
            ports(&test_machine("dpu-one", 20), "dpu-one")
        );
        let mut both = first;
        both.data.interfaces.extend(second.data.interfaces);
        assert_eq!(interfaces_for_dpu(&both, "dpu-one").len(), 32);
        assert_eq!(interfaces_for_dpu(&both, "dpu-two").len(), 32);
        assert!(interfaces_for_dpu(&both, "unplaced").is_empty());
        assert!(ports_for_dpu(&both, "unplaced").unwrap().is_none());
        both.data.interfaces.push(ExpectedInterface {
            physical_ifname: Some("legacy-port".to_string()),
            nic_type: Some("CX9".to_string()),
            ..Default::default()
        });
        assert_eq!(interfaces_for_dpu(&both, "dpu-one").len(), 32);
        assert_eq!(interfaces_for_dpu(&both, "unplaced").len(), 1);
    }
}
