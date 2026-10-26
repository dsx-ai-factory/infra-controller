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

//! The nico-api reads of the state readers, behind a trait so readers are
//! tested against an in-memory fake site. Every method is one read-only RPC;
//! the readers pass each call through the nico-api gate.

pub(crate) mod grpc;
pub(crate) mod host_read;

#[cfg(test)]
pub(crate) mod fake;

use std::sync::Arc;

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::MachineId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use rpc::forge::host_reprovisioning_list_response::HostReprovisioningListItem;
use rpc::{forge, site_explorer};

use crate::gate::CallError;
use crate::nico::host_read::HostDetail;

#[async_trait::async_trait]
pub(crate) trait NicoApi: Send + Sync {
    /// Hosts and DPUs, without predicted hosts.
    async fn machine_ids(&self) -> Result<Vec<MachineId>, CallError>;
    async fn hosts_by_ids(&self, ids: &[MachineId]) -> Result<Vec<HostDetail>, CallError>;
    async fn switch_ids(&self) -> Result<Vec<SwitchId>, CallError>;
    async fn switches_by_ids(&self, ids: &[SwitchId]) -> Result<Vec<forge::Switch>, CallError>;
    async fn power_shelf_ids(&self) -> Result<Vec<PowerShelfId>, CallError>;
    async fn power_shelves_by_ids(
        &self,
        ids: &[PowerShelfId],
    ) -> Result<Vec<forge::PowerShelf>, CallError>;
    async fn rack_ids(&self) -> Result<Vec<RackId>, CallError>;
    async fn racks_by_ids(&self, ids: &[RackId]) -> Result<Vec<forge::Rack>, CallError>;
    async fn instance_ids(&self) -> Result<Vec<InstanceId>, CallError>;
    async fn instances_by_ids(&self, ids: &[InstanceId])
    -> Result<Vec<forge::Instance>, CallError>;
    async fn expected_machines_linked(
        &self,
    ) -> Result<Vec<forge::LinkedExpectedMachine>, CallError>;
    async fn expected_switches_linked(&self)
    -> Result<Vec<forge::LinkedExpectedSwitch>, CallError>;
    async fn expected_power_shelves_linked(
        &self,
    ) -> Result<Vec<forge::LinkedExpectedPowerShelf>, CallError>;
    async fn expected_racks(&self) -> Result<Vec<forge::ExpectedRack>, CallError>;
    async fn expected_rack_groups(&self) -> Result<Vec<forge::ExpectedRackGroup>, CallError>;
    async fn hosts_waiting_for_reprovisioning(
        &self,
    ) -> Result<Vec<HostReprovisioningListItem>, CallError>;
    async fn explored_endpoints(
        &self,
        addresses: &[String],
    ) -> Result<Vec<site_explorer::ExploredEndpoint>, CallError>;
}

/// Opens one nico-api session per sweep, so certificate rotation needs no
/// restart and no call within a sweep pays a TLS dial.
#[async_trait::async_trait]
pub(crate) trait NicoConnector: Send + Sync {
    async fn connect(&self) -> Result<Arc<dyn NicoApi>, CallError>;
}
