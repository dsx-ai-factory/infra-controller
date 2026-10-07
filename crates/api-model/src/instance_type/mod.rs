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
use std::collections::HashMap;

use carbide_uuid::instance_type::InstanceTypeId;
use carbide_uuid::machine::MachineId;
use chrono::prelude::*;
use config_version::ConfigVersion;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::postgres::PgRow;

use super::machine::capabilities::{
    self as machine_caps, MachineCapabilitiesSet, MachineCapabilityDeviceType,
    MachineCapabilityType,
};
use crate::metadata::Metadata;

/* **************************************** */
/*      InstanceTypeAssociationDetails      */
/* **************************************** */

/// InstanceTypeAssociationDetails holds the counts and ids
/// of machines and and counts of instances associated with
/// an InstanceType.
#[derive(Debug, Clone)]
pub struct InstanceTypeAssociationDetails {
    pub instance_type_id: InstanceTypeId,
    pub total_machines: u32,
    pub machine_ids: Vec<MachineId>,
    pub total_instances: u32,
}

/* **************************************** */
/*    InstanceTypeMachineCapabilityFilter   */
/* **************************************** */

/// InstanceTypeMachineCapabilityFilter holds the details of a
/// single desired capability of a machine.  This could technically
/// represent more than one physical component, such as a server
/// with multiple CPUs of the exact same type.
///
/// For example, type=cpu, name=xeon, count=2
/// could represent a single CPU capability for a machine.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct InstanceTypeMachineCapabilityFilter {
    pub capability_type: MachineCapabilityType,
    pub name: Option<String>,
    pub frequency: Option<String>,
    pub capacity: Option<String>,
    pub vendor: Option<String>,
    pub count: Option<u32>,
    pub hardware_revision: Option<String>,
    pub cores: Option<u32>,
    pub threads: Option<u32>,
    pub inactive_devices: Option<Vec<u32>>,
    pub device_type: Option<MachineCapabilityDeviceType>,
}

impl InstanceTypeMachineCapabilityFilter {
    fn matches_machine_cpu_capability(&self, mac_cap: &machine_caps::MachineCapabilityCpu) -> bool {
        (match (&self.name, &mac_cap.name) {
            (None, _) => true,
            (Some(c), mc) => c == mc,
        }) && (match (self.cores, mac_cap.cores) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        }) && (match (self.threads, mac_cap.threads) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        }) && (match (&self.vendor, &mac_cap.vendor) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        })
    }

    fn matches_machine_gpu_capability(&self, mac_cap: &machine_caps::MachineCapabilityGpu) -> bool {
        (match (&self.name, &mac_cap.name) {
            (None, _) => true,
            (Some(c), mc) => c == mc,
        }) && (match (self.cores, mac_cap.cores) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        }) && (match (self.threads, mac_cap.threads) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        }) && (match (&self.vendor, &mac_cap.vendor) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        }) && (match (&self.frequency, &mac_cap.frequency) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        }) && (match (&self.capacity, &mac_cap.memory_capacity) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        })
    }

    fn matches_machine_memory_capability(
        &self,
        mac_cap: &machine_caps::MachineCapabilityMemory,
    ) -> bool {
        (match (&self.name, &mac_cap.name) {
            (None, _) => true,
            (Some(c), mc) => c == mc,
        }) && (match (&self.vendor, &mac_cap.vendor) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        }) && (match (&self.capacity, &mac_cap.capacity) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        })
    }

    fn matches_machine_storage_capability(
        &self,
        mac_cap: &machine_caps::MachineCapabilityStorage,
    ) -> bool {
        (match (&self.name, &mac_cap.name) {
            (None, _) => true,
            (Some(c), mc) => c == mc,
        }) && (match (&self.vendor, &mac_cap.vendor) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        }) && (match (&self.capacity, &mac_cap.capacity) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        })
    }

    fn matches_machine_network_capability(
        &self,
        mac_cap: &machine_caps::MachineCapabilityNetwork,
    ) -> bool {
        (match (&self.name, &mac_cap.name) {
            (None, _) => true,
            (Some(c), mc) => c == mc,
        }) && (match (&self.vendor, &mac_cap.vendor) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        }) && (match (&self.device_type, &mac_cap.device_type) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        })
    }

    fn matches_machine_infiniband_capability(
        &self,
        mac_cap: &machine_caps::MachineCapabilityInfiniband,
    ) -> bool {
        (match (&self.name, &mac_cap.name) {
            (None, _) => true,
            (Some(c), mc) => c == mc,
        }) && (match (&self.vendor, &mac_cap.vendor) {
            (None, _) => true,
            (Some(c), mc) => c == mc,
        }) && (match (&self.inactive_devices, &mac_cap.inactive_devices) {
            (None, _) => true,
            (Some(c), mc) => c == mc,
        })
    }

    fn matches_machine_dpu_capability(&self, mac_cap: &machine_caps::MachineCapabilityDpu) -> bool {
        (match (&self.name, &mac_cap.name) {
            (None, _) => true,
            (Some(c), mc) => c == mc,
        }) && (match (&self.hardware_revision, &mac_cap.hardware_revision) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(mc)) => c == mc,
        })
    }
}

/* ********************************** */
/*            InstanceType            */
/* ********************************** */

/// InstanceType represents a collection of _desired_
/// machine capabilities.
/// An InstanceType is used to create pools of "allocatable"
/// machines based on their capabilities.
///
/// A provider would define an InstanceType and then define
/// an allocation constraint with that InstanceType to define
/// how many instances of a given InstanceType a tenant can
/// create/allocate.
///
/// When an instance allocation is requested, the InstanceType
/// is then used to filter machines to select an available
/// machine that matches the set of desired capabilities.
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceType {
    pub id: InstanceTypeId,
    pub desired_capabilities: Vec<InstanceTypeMachineCapabilityFilter>,
    pub version: ConfigVersion,
    pub created: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
    pub metadata: Metadata,
}

impl<'r> sqlx::FromRow<'r, PgRow> for InstanceTypeAssociationDetails {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let machine_ids: sqlx::types::Json<Vec<MachineId>> = row.try_get("machine_ids")?;

        let total_instances: i32 = row.try_get("total_instances")?;
        let total_machines: i32 = row.try_get("total_machines")?;

        Ok(InstanceTypeAssociationDetails {
            instance_type_id: row.try_get("instance_type_id")?,
            total_machines: total_machines
                .try_into()
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            machine_ids: machine_ids.0,
            total_instances: total_instances
                .try_into()
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        })
    }
}

impl<'r> sqlx::FromRow<'r, PgRow> for InstanceType {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("labels")?;

        let metadata = Metadata {
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            labels: labels.0,
        };

        let desired_capabilities: sqlx::types::Json<Vec<InstanceTypeMachineCapabilityFilter>> =
            row.try_get("desired_capabilities")?;

        Ok(InstanceType {
            id: row.try_get("id")?,
            version: row.try_get("version")?,
            created: row.try_get("created")?,
            deleted: row.try_get("deleted")?,
            metadata,
            desired_capabilities: desired_capabilities.0,
        })
    }
}

impl InstanceType {
    /// Check whether a set of capabilities satisfies the
    /// requirements of an InstanceType
    ///
    /// * `machine_caps` - A reference to a MachineCapabilitiesSet struct with the capabilities to check
    pub fn matches_capability_set(&self, machine_caps: &MachineCapabilitiesSet) -> bool {
        for cap in self.desired_capabilities.iter() {
            match cap.capability_type {
                MachineCapabilityType::Cpu => {
                    if let Some(desired_cnt) = cap.count {
                        match machine_caps.cpu.iter().try_fold(0, |found_cnt: u32, c| {
                            if !cap.matches_machine_cpu_capability(c) {
                                return Some(found_cnt);
                            }

                            // Update the found count.
                            match found_cnt.overflowing_add(c.count) {
                                (_, true) => None, // overflow
                                (found_cnt, _) if found_cnt > desired_cnt => None,
                                (found_cnt, _) => Some(found_cnt),
                            }
                        }) {
                            Some(found_cnt) if found_cnt == desired_cnt => {} // Do nothing.
                            _ => return false, // Desired count was exceeded or count mismatch
                        }
                    } else if !machine_caps
                        .cpu
                        .iter()
                        .any(|c| cap.matches_machine_cpu_capability(c))
                    {
                        return false; // We just needed to find at least one match, but there were zero.
                    }
                }
                MachineCapabilityType::Gpu => {
                    if let Some(desired_cnt) = cap.count {
                        match machine_caps.gpu.iter().try_fold(0, |found_cnt: u32, c| {
                            if !cap.matches_machine_gpu_capability(c) {
                                return Some(found_cnt);
                            }

                            // Update the found count.
                            match found_cnt.overflowing_add(c.count) {
                                (_, true) => None, // overflow
                                (found_cnt, _) if found_cnt > desired_cnt => None,
                                (found_cnt, _) => Some(found_cnt),
                            }
                        }) {
                            Some(found_cnt) if found_cnt == desired_cnt => {} // Do nothing.
                            _ => return false, // Desired count was exceeded or count mismatch.
                        }
                    } else if !machine_caps
                        .gpu
                        .iter()
                        .any(|c| cap.matches_machine_gpu_capability(c))
                    {
                        return false; // We just needed to find at least one match, but there were zero.
                    }
                }
                MachineCapabilityType::Memory => {
                    if let Some(desired_cnt) = cap.count {
                        match machine_caps.memory.iter().try_fold(0, |found_cnt: u32, c| {
                            if !cap.matches_machine_memory_capability(c) {
                                return Some(found_cnt);
                            }

                            // Update the found count.
                            match found_cnt.overflowing_add(c.count) {
                                (_, true) => None, // overflow
                                (found_cnt, _) if found_cnt > desired_cnt => None,
                                (found_cnt, _) => Some(found_cnt),
                            }
                        }) {
                            Some(found_cnt) if found_cnt == desired_cnt => {} // Do nothing.
                            _ => return false, // Desired count was exceeded or count mismatch
                        }
                    } else if !machine_caps
                        .memory
                        .iter()
                        .any(|c| cap.matches_machine_memory_capability(c))
                    {
                        return false; // We just needed to find at least one match, but there were zero.
                    }
                }
                MachineCapabilityType::Storage => {
                    if let Some(desired_cnt) = cap.count {
                        match machine_caps
                            .storage
                            .iter()
                            .try_fold(0, |found_cnt: u32, c| {
                                if !cap.matches_machine_storage_capability(c) {
                                    return Some(found_cnt);
                                }

                                // Update the found count.
                                match found_cnt.overflowing_add(c.count) {
                                    (_, true) => None, // overflow
                                    (found_cnt, _) if found_cnt > desired_cnt => None,
                                    (found_cnt, _) => Some(found_cnt),
                                }
                            }) {
                            Some(found_cnt) if found_cnt == desired_cnt => {} // Do nothing.
                            _ => return false, // Desired count was exceeded or count mismatch
                        }
                    } else if !machine_caps
                        .storage
                        .iter()
                        .any(|c| cap.matches_machine_storage_capability(c))
                    {
                        return false; // We just needed to find at least one match, but there were zero.
                    }
                }

                MachineCapabilityType::Network => {
                    if let Some(desired_cnt) = cap.count {
                        match machine_caps
                            .network
                            .iter()
                            .try_fold(0, |found_cnt: u32, c| {
                                if !cap.matches_machine_network_capability(c) {
                                    return Some(found_cnt);
                                }

                                // Update the found count.
                                match found_cnt.overflowing_add(c.count) {
                                    (_, true) => None, // overflow
                                    (found_cnt, _) if found_cnt > desired_cnt => None,
                                    (found_cnt, _) => Some(found_cnt),
                                }
                            }) {
                            Some(found_cnt) if found_cnt == desired_cnt => {} // Do nothing.
                            _ => return false, // Desired count was exceeded or count mismatch
                        }
                    } else if !machine_caps
                        .network
                        .iter()
                        .any(|c| cap.matches_machine_network_capability(c))
                    {
                        return false; // We just needed to find at least one match, but there were zero.
                    }
                }

                MachineCapabilityType::Infiniband => {
                    if let Some(desired_cnt) = cap.count {
                        match machine_caps
                            .infiniband
                            .iter()
                            .try_fold(0, |found_cnt: u32, c| {
                                if !cap.matches_machine_infiniband_capability(c) {
                                    return Some(found_cnt);
                                }

                                // Update the found count.
                                match found_cnt.overflowing_add(c.count) {
                                    (_, true) => None, // overflow
                                    (found_cnt, _) if found_cnt > desired_cnt => None,
                                    (found_cnt, _) => Some(found_cnt),
                                }
                            }) {
                            Some(found_cnt) if found_cnt == desired_cnt => {} // Do nothing.
                            _ => return false, // Desired count was exceeded or count mismatch
                        }
                    } else if !machine_caps
                        .infiniband
                        .iter()
                        .any(|c| cap.matches_machine_infiniband_capability(c))
                    {
                        return false; // We just needed to find at least one match, but there were zero.
                    }
                }

                MachineCapabilityType::Dpu => {
                    if let Some(desired_cnt) = cap.count {
                        match machine_caps.dpu.iter().try_fold(0, |found_cnt: u32, c| {
                            if !cap.matches_machine_dpu_capability(c) {
                                return Some(found_cnt);
                            }

                            // Update the found count.
                            match found_cnt.overflowing_add(c.count) {
                                (_, true) => None, // overflow
                                (found_cnt, _) if found_cnt > desired_cnt => None,
                                (found_cnt, _) => Some(found_cnt),
                            }
                        }) {
                            Some(found_cnt) if found_cnt == desired_cnt => {} // Do nothing.
                            _ => return false, // Desired count was exceeded or count mismatch
                        }
                    } else if !machine_caps
                        .dpu
                        .iter()
                        .any(|c| cap.matches_machine_dpu_capability(c))
                    {
                        return false; // We just needed to find at least one match, but there were zero.
                    }
                }
            }
        }

        true
    }
}

#[cfg(test)]
mod tests;
