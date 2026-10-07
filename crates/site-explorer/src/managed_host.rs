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

use carbide_uuid::machine::HostMachineId;
use db::DatabaseError;
use db::db_read::DbReader;
use model::site_explorer::ExploredManagedHost;

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
/// This queries the `machine_topologies` table which stores actual ingested machines,
/// rather than the `explored_managed_hosts` staging table which gets wiped and rebuilt
/// on every site explorer update.
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
