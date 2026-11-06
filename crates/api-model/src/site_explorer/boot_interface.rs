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

//! Selects host boot interfaces from registered host-PF facts and Redfish PCI evidence.

use carbide_uuid::machine::DpuMachineId;
use mac_address::MacAddress;

#[cfg(test)]
#[path = "boot_interface_tests.rs"]
mod tests;

use super::{
    EndpointExplorationReport, ExploredDpu, HostPrimaryInterfaceSelection, chassis_part_number,
    is_dpu_product_chassis_id,
};
use crate::errors::{ModelError, ModelResult};
use crate::hardware_info::{DpuData, DpuHostPfError};
use crate::machine_boot_interface::BootInterfaceSelectionSource;

/// Keeps one paired DPU's registered inventory beside its provisional Redfish MAC for shared selection.
#[derive(Clone, Debug)]
pub struct HostBootInterfaceDpu {
    /// Paired DPU whose exact host link must be associated.
    pub dpu_machine_id: DpuMachineId,
    /// Registered DPU data, with the observed platform classification.
    pub hardware: DpuData,
    /// Redfish-derived candidate used only for BF2/BF3.
    pub provisional_mac_address: Option<MacAddress>,
}

impl HostBootInterfaceDpu {
    /// Resolves the configured BF4 host PF while retaining the existing BF2/BF3 candidate.
    pub fn host_mac_address(&self, controller: u8) -> Result<Option<MacAddress>, DpuHostPfError> {
        match self.hardware.selected_host_pf(controller)? {
            Some(host_pf) => Ok(Some(host_pf.mac_address)),
            None => Ok(self.provisional_mac_address),
        }
    }
}

/// Retains the specific missing or contradictory fact so callers can wait without choosing a provisional BF4 MAC.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HostPrimaryInterfaceError {
    /// Automatic selection has no host ComputerSystem report.
    #[error("host redfish system is missing")]
    MissingHostSystem,
    /// No automatic DPU candidate matches a host interface.
    #[error("no DPU host-PF candidate matches host redfish interfaces")]
    NoCandidates,
    /// A paired BF4 DPU has invalid, missing, or contradictory registered inventory.
    #[error("DPU {dpu_machine_id}: {source}")]
    DpuInventory {
        /// DPU whose required mapping could not be used.
        dpu_machine_id: DpuMachineId,
        /// Exact mapping failure.
        #[source]
        source: DpuHostPfError,
    },
    /// A required BF4 target MAC has no host Redfish EthernetInterface.
    #[error("host redfish interface is missing for MAC {0}")]
    MissingHostInterface(MacAddress),
    /// A required BF4 target does not resolve to a Redfish interface ID.
    #[error("host redfish interface ID is missing for MAC {0}")]
    MissingInterfaceId(MacAddress),
    /// An automatic candidate lacks a UEFI PCI path.
    #[error("UEFI PCI path is missing or invalid for MAC {0}")]
    MissingUefiPath(MacAddress),
    /// Multiple paired DPUs claim the same boot MAC.
    #[error("multiple paired DPUs claim host-PF MAC {0}")]
    DuplicateCandidate(MacAddress),
}

impl ExploredDpu {
    /// Combines registered inventory with observed BF4 classification without treating its base MAC as a boot mapping.
    pub fn boot_interface_dpu(
        &self,
        registered: Option<&DpuData>,
    ) -> ModelResult<HostBootInterfaceDpu> {
        let dpu_machine_id = self
            .report
            .machine_id
            .ok_or(ModelError::MissingArgument("Missing DPU machine ID"))?
            .try_into()?;
        let mut hardware = registered.cloned().unwrap_or_default();
        if let Some(part_number) = self
            .report
            .chassis
            .iter()
            .filter(|chassis| is_dpu_product_chassis_id(&chassis.id))
            .find_map(chassis_part_number)
            && (hardware.part_number.is_empty()
                || super::is_bf4_dpu_part_number(part_number.trim()))
        {
            // A Redfish BF4 classification must require agent inventory even before registration.
            hardware.part_number = part_number.to_string();
        }
        Ok(HostBootInterfaceDpu {
            dpu_machine_id,
            hardware,
            provisional_mac_address: self.host_pf_mac_address,
        })
    }
}

impl EndpointExplorationReport {
    /// Selects a declared primary first, otherwise requires each BF4 mapping before comparing all candidate PCI paths.
    pub fn select_host_primary_interface_from_dpus(
        &self,
        dpus: &[HostBootInterfaceDpu],
        declared_primary: Option<MacAddress>,
        controller: u8,
    ) -> Result<HostPrimaryInterfaceSelection, HostPrimaryInterfaceError> {
        // A resolved declaration is independent of automatic DPU inventory requirements.
        if let Some(mac) = declared_primary
            && self.find_interface_id_for_mac(mac).is_some()
        {
            return self.select_host_primary_interface_from_macs(&[], Some(mac));
        }
        let system = self
            .systems
            .first()
            .ok_or(HostPrimaryInterfaceError::MissingHostSystem)?;
        let mut macs = Vec::with_capacity(dpus.len());
        for dpu in dpus {
            let Some(mac) = dpu.host_mac_address(controller).map_err(|source| {
                HostPrimaryInterfaceError::DpuInventory {
                    dpu_machine_id: dpu.dpu_machine_id,
                    source,
                }
            })?
            else {
                continue;
            };
            // BF4 requires a usable match for every paired DPU; legacy unmatched candidates remain ignored.
            if dpu.hardware.required_host_pf(controller).is_some() {
                let interface = system
                    .ethernet_interfaces
                    .iter()
                    .find(|interface| interface.mac_address == Some(mac))
                    .ok_or(HostPrimaryInterfaceError::MissingHostInterface(mac))?;
                if interface
                    .id
                    .as_deref()
                    .is_none_or(|id| id.trim().is_empty())
                {
                    return Err(HostPrimaryInterfaceError::MissingInterfaceId(mac));
                }
            }
            if macs.contains(&mac) {
                return Err(HostPrimaryInterfaceError::DuplicateCandidate(mac));
            }
            macs.push(mac);
        }
        self.select_host_primary_interface_from_macs(&macs, None)
    }

    /// Compares the existing UEFI PCI ordering keys so discovery and readiness apply one ordering rule.
    pub(super) fn select_host_primary_interface_from_macs(
        &self,
        macs: &[MacAddress],
        declared_primary: Option<MacAddress>,
    ) -> Result<HostPrimaryInterfaceSelection, HostPrimaryInterfaceError> {
        // A declared primary wins as long as the report has it as a full pair
        // (find_interface_id_for_mac scans every system ethernet interface,
        // integrated NICs included).
        if let Some(mac) = declared_primary
            && self.find_interface_id_for_mac(mac).is_some()
        {
            return Ok(HostPrimaryInterfaceSelection {
                mac_address: mac,
                source: BootInterfaceSelectionSource::ExpectedMachine,
            });
        }
        let system = self
            .systems
            .first()
            .ok_or(HostPrimaryInterfaceError::MissingHostSystem)?;
        // Filter PCI device names only for the interfaces which are mapped to DPU.
        // Host might have some integrated or embedded interfaces, which are not used by forge.
        // Need to ignore them.
        let candidates = system
            .ethernet_interfaces
            .iter()
            .filter(|interface| interface.mac_address.is_some_and(|mac| macs.contains(&mac)))
            .map(|interface| {
                let mac = interface
                    .mac_address
                    .ok_or(HostPrimaryInterfaceError::NoCandidates)?;
                let key = interface
                    .uefi_device_path
                    .as_ref()
                    .and_then(|path| path.ordering_key().ok())
                    .ok_or(HostPrimaryInterfaceError::MissingUefiPath(mac))?;
                Ok((mac, key))
            })
            .collect::<Result<Vec<_>, HostPrimaryInterfaceError>>()?;
        let (mac_address, _) = candidates
            .into_iter()
            .min_by(|(_, left), (_, right)| left.cmp(right))
            .ok_or(HostPrimaryInterfaceError::NoCandidates)?;
        Ok(HostPrimaryInterfaceSelection {
            mac_address,
            source: BootInterfaceSelectionSource::RedfishUefiPci,
        })
    }
}
