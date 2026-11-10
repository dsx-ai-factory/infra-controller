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
use std::net::IpAddr;

use carbide_uuid::machine::{
    HostMachineId, MachineIdSubtype, PredictedHostMachineId, StableHostMachineId,
};
use db::DatabaseError;
use db::db_read::DbReader;
use mac_address::MacAddress;
use model::site_explorer::ExploredManagedHost;
use sqlx::PgConnection;

use crate::errors::SiteExplorerResult;

/// ManagedHost wraps an ExploredManagedHost along with a machine id.
/// This helper structure is used by the create_managed_host to create a managed host
/// using the explored managed host structure that the site explorer retrieves from the
/// explored_managed_host table.
/// The create_managed_host function creates a ManagedHost with the machine ID set to None initially.
/// It sets the machine_id when attaching the first DPU to a given host.
/// It will use the machine_id from this structure when attaching all other DPUs to a host.
#[derive(Debug, Clone)]
pub(super) struct ManagedHost<'a> {
    /// Retrieved from the explored_managed_host table
    pub(super) explored_host: &'a ExploredManagedHost,
    /// The site explorer uses the machine_id as the host's machine ID when attaching a DPU to a host.
    /// The site explorer sets this field as part of attaching the first DPU to a host in the create_managed_host function.
    pub(super) machine_id: Option<HostMachineId>,
}

impl<'a> ManagedHost<'a> {
    pub(super) fn init(explored_host: &'a ExploredManagedHost) -> Self {
        Self {
            explored_host,
            machine_id: None,
        }
    }
}

/// Checks if an ingested machine (host or DPU) exists with the given BMC IP address.
///
/// This checks BMC interface/address rows for a machine owner, rather than the
/// `explored_managed_hosts` staging table rebuilt on every site explorer update.
/// Switch and power-shelf ownership alone does not satisfy this predicate.
///
/// This prevents site explorer from triggering unintended actions (like power cycles
/// or re-ingestion) on machines that have already been ingested, even if the host's
/// BMC temporarily stops reporting DPUs in its PCIe device list.
pub async fn is_endpoint_in_managed_host(
    bmc_ip_address: IpAddr,
    txn: impl DbReader<'_>,
) -> Result<bool, DatabaseError> {
    let machine_id =
        db::machine_topology::find_machine_id_by_bmc_ip(txn, &bmc_ip_address.to_string()).await?;
    Ok(machine_id.is_some())
}

/// Host identity found through the zero-DPU host's interface inventory.
pub(super) enum ZeroDpuHostLookup {
    NotFound,
    Predicted(PredictedHostMachineId),
    Stable(StableHostMachineId),
    Conflict,
}

/// Resolves host identity from owned interfaces or pending predictions and rejects
/// persisted DPU attachments: a report can temporarily omit its DPU inventory.
/// Predicted interfaces must belong to the explored BMC before they can be refreshed.
pub(super) async fn find_zero_dpu_host(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
    mac_addresses: &[MacAddress],
) -> SiteExplorerResult<ZeroDpuHostLookup> {
    // Interface ownership survives migration from a predicted to a stable ID.
    let endpoint_machine_id = db::machine_topology::find_machine_id_by_bmc_mac::<HostMachineId>(
        &mut *txn,
        bmc_mac_address,
    )
    .await?;

    let machine_ids_by_mac =
        db::machine::find_machine_ids_by_mac_addresses(&mut *txn, mac_addresses).await?;
    let predicted_machine_ids_by_mac =
        db::predicted_machine_interface::find_machine_ids_by_mac_addresses(
            &mut *txn,
            mac_addresses,
        )
        .await?;
    let mut host = ZeroDpuHostLookup::NotFound;
    for mac_address in mac_addresses {
        let Some(machine_id) = machine_ids_by_mac
            .get(mac_address)
            .or_else(|| predicted_machine_ids_by_mac.get(mac_address))
            .copied()
        else {
            continue;
        };

        match machine_id.machine_id_subtype() {
            MachineIdSubtype::StableHost(machine_id) => {
                host = ZeroDpuHostLookup::Stable(machine_id);
                break;
            }
            MachineIdSubtype::PredictedHost(predicted_machine_id) => {
                if endpoint_machine_id.as_ref() != Some(predicted_machine_id.as_host_machine_id()) {
                    tracing::warn!(
                        %mac_address,
                        predicted_machine_id = %machine_id,
                        ?endpoint_machine_id,
                        %bmc_mac_address,
                        "Could not confirm that the predicted interface belongs to the explored BMC; skipping refresh",
                    );
                    return Ok(ZeroDpuHostLookup::Conflict);
                }
                host = ZeroDpuHostLookup::Predicted(predicted_machine_id);
            }
            MachineIdSubtype::Dpu(_) => {
                return Ok(ZeroDpuHostLookup::Conflict);
            }
        }
    }

    let machine_id: HostMachineId = match &host {
        ZeroDpuHostLookup::Predicted(machine_id) => (*machine_id).into(),
        ZeroDpuHostLookup::Stable(machine_id) => (*machine_id).into(),
        ZeroDpuHostLookup::NotFound | ZeroDpuHostLookup::Conflict => return Ok(host),
    };
    let interfaces = db::machine_interface::find_by_machine_ids(txn, &[machine_id]).await?;
    Ok(
        if interfaces
            .values()
            .flatten()
            .any(|interface| interface.attached_dpu_machine_id.is_some())
        {
            ZeroDpuHostLookup::Conflict
        } else {
            host
        },
    )
}
