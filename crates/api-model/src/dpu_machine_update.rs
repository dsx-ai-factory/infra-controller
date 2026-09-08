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

use carbide_uuid::machine::{DpuMachineId, HostMachineId};
use sqlx::FromRow;

use crate::errors::ModelError;
use crate::machine::{ManagedHostState, ManagedHostStateSnapshot};

#[derive(Debug, FromRow)]
pub struct DpuMachineUpdate {
    pub host_machine_id: HostMachineId,
    pub dpu_machine_id: DpuMachineId,
    pub firmware_version: String,
    /// Whether the owning host is ingested through DPF. DPF decides staleness
    /// from the DPUDeployment's expected BFB, not from
    /// `dpu_nic_firmware_update_versions`, so `firmware_version` on a
    /// DPF-managed update is traceability only and must not be compared
    /// against that config list. Defaults to `false` when the value is loaded
    /// straight from a query that does not select it.
    #[sqlx(default)]
    pub dpf_managed: bool,
}

/// A DPU identified via DPF whose installed BFB or BlueFieldSoftware no longer
/// matches the expected one. Produced by the DPF query layer and joined to host
/// snapshots by [`DpuMachineUpdate::find_outdated_dpus_dpf`].
#[derive(Debug, Clone)]
pub struct OutdatedDpfDpu {
    pub dpu_machine_id: DpuMachineId,
    /// Expected provisioning source: a BFB filename (e.g.
    /// `dpf-operator-system-bf-bundle-<hash>.bfb`) or a BlueFieldSoftware CR
    /// name, depending on which one the owning DPUDeployment declares. Used as
    /// the `firmware_version` field for traceability when this DPU is turned
    /// into a [`DpuMachineUpdate`].
    pub target_source: String,
}

impl DpuMachineUpdate {
    /// Find DPUs and the corresponding host that needs to have its firmware updated.
    /// DPUs can be updated when:
    /// 1. the installed firmware does not match the expected firmware
    /// 2. the DPU is not marked for reprovisioning
    /// 3. the DPU is not marked for maintenance.
    /// 4. the Host is healthy (no pending health alert)
    /// 5. If all DPUs need upgrade, put all in queue. State machine supports upgrading multiple
    ///    DPUs of a managedhost.
    /// 6. If some of the DPUs for a managed host need upgrade, put them in queue.
    ///    6.1. Make sure none of the DPU is under reprovisioning while queuing a new DPU for a
    ///    managedhost. This is done by confirming that Host is not marked for updates
    pub fn find_available_outdated_dpus(
        limit: Option<i32>,
        dpu_nic_firmware_update_versions: &[String],
        snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
        dpf_outdated: &[OutdatedDpfDpu],
    ) -> Result<Vec<DpuMachineUpdate>, ModelError> {
        if limit.is_some_and(|l| l <= 0) {
            return Ok(vec![]);
        }

        let mut outdated_dpus =
            Self::find_outdated_dpus(dpu_nic_firmware_update_versions, snapshots)?;
        outdated_dpus.extend(Self::find_outdated_dpus_dpf(dpf_outdated, snapshots)?);

        let mut scheduled_host_updates = 0;
        let available_outdated_dpus: Vec<DpuMachineUpdate> = outdated_dpus
            .into_iter()
            .filter_map(|outdated_host| {
                // If the limit on scheduled host updates is reached, skip creating more
                if let Some(limit) = limit
                    && scheduled_host_updates >= limit
                {
                    return None;
                }
                if !outdated_host.is_available_for_updates() {
                    return None;
                }
                scheduled_host_updates += 1;
                Some(outdated_host.outdated_dpus)
            })
            .flatten()
            .collect();

        Ok(available_outdated_dpus)
    }

    pub fn find_unavailable_outdated_dpus(
        dpu_nic_firmware_update_versions: &[String],
        snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> Result<Vec<DpuMachineUpdate>, ModelError> {
        let outdated_dpus = Self::find_outdated_dpus(dpu_nic_firmware_update_versions, snapshots)?;

        let unavailable_outdated_dpus: Vec<DpuMachineUpdate> = outdated_dpus
            .into_iter()
            .filter_map(|outdated_host| {
                if outdated_host.is_available_for_updates() {
                    return None;
                }
                Some(outdated_host.outdated_dpus)
            })
            .flatten()
            .collect();

        Ok(unavailable_outdated_dpus)
    }

    pub fn find_outdated_dpus<'a>(
        dpu_nic_firmware_update_versions: &[String],
        snapshots: &'a HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> Result<Vec<OutdatedHost<'a>>, ModelError> {
        snapshots
            .iter()
            .map(|(machine_id, managed_host)| {
                let outdated_dpus: Result<Vec<Option<DpuMachineUpdate>>, ModelError> = managed_host
                    .dpu_snapshots
                    .iter()
                    .map(|dpu| {
                        // TODO: implement the logic to find the outdated DPUs which are ingested
                        // using DPF.
                        if managed_host.host_snapshot.config.dpf.used_for_ingestion {
                            return Ok(None);
                        }
                        let firmware_version = dpu
                            .status
                            .hardware_info
                            .as_ref()
                            .and_then(|info| info.dpu_info.as_ref())
                            .map(|dpu_info| dpu_info.firmware_version.trim().to_owned());
                        let Some(firmware_version) = firmware_version else {
                            return Ok(None);
                        };

                        if dpu_nic_firmware_update_versions.contains(&firmware_version) {
                            return Ok(None);
                        }

                        Ok(Some(DpuMachineUpdate {
                            host_machine_id: *machine_id,
                            dpu_machine_id: dpu.dpu_machine_id().map_err(|error| {
                                ModelError::DatabaseTypeConversionError(error.to_string())
                            })?,
                            firmware_version,
                            dpf_managed: false,
                        }))
                    })
                    .collect();
                let outdated_dpus = outdated_dpus?.into_iter().flatten().collect::<Vec<_>>();

                if outdated_dpus.is_empty() {
                    return Ok(None);
                }

                Ok(Some(OutdatedHost {
                    managed_host,
                    outdated_dpus,
                }))
            })
            .collect::<Result<Vec<_>, ModelError>>()
            .map(|hosts| hosts.into_iter().flatten().collect())
    }

    /// Join DPF-identified outdated DPUs (by `MachineId`) to their owning host
    /// snapshots and produce one [`OutdatedHost`] per host. DPUs that do not
    /// appear in any snapshot are dropped silently — the upstream DPF query
    /// layer is responsible for logging that case.
    pub fn find_outdated_dpus_dpf<'a>(
        dpf_outdated: &[OutdatedDpfDpu],
        snapshots: &'a HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> Result<Vec<OutdatedHost<'a>>, ModelError> {
        if dpf_outdated.is_empty() {
            return Ok(vec![]);
        }

        let dpu_to_host: HashMap<DpuMachineId, HostMachineId> = snapshots
            .iter()
            .flat_map(|(host_id, snap)| {
                snap.dpu_snapshots.iter().map(move |d| {
                    d.dpu_machine_id()
                        .map(|id| (id, *host_id))
                        .map_err(|error| ModelError::DatabaseTypeConversionError(error.to_string()))
                })
            })
            .collect::<Result<_, _>>()?;

        let mut by_host: HashMap<HostMachineId, Vec<DpuMachineUpdate>> = HashMap::new();
        for outdated in dpf_outdated {
            let Some(&host_id) = dpu_to_host.get(&outdated.dpu_machine_id) else {
                continue;
            };
            by_host.entry(host_id).or_default().push(DpuMachineUpdate {
                host_machine_id: host_id,
                dpu_machine_id: outdated.dpu_machine_id,
                firmware_version: outdated.target_source.clone(),
                dpf_managed: true,
            });
        }

        Ok(by_host
            .into_iter()
            .filter_map(|(host_id, outdated_dpus)| {
                let managed_host = snapshots.get(&host_id)?;
                Some(OutdatedHost {
                    managed_host,
                    outdated_dpus,
                })
            })
            .collect())
    }
}

pub struct OutdatedHost<'a> {
    pub managed_host: &'a ManagedHostStateSnapshot,
    pub outdated_dpus: Vec<DpuMachineUpdate>,
}

impl OutdatedHost<'_> {
    pub fn is_available_for_updates(&self) -> bool {
        // Skip any machines that have pending health alerts
        if !self.managed_host.aggregate_health.alerts.is_empty() {
            return false;
        }
        // Skip any machines that are not Ready
        if !matches!(self.managed_host.managed_state, ManagedHostState::Ready) {
            return false;
        }

        // Check if all DPUs have the `reprovisioning_requested` flag cleared
        if self
            .managed_host
            .dpu_snapshots
            .iter()
            .any(|dpu| dpu.reprovision_requested.is_some())
        {
            return false;
        }

        true
    }
}
