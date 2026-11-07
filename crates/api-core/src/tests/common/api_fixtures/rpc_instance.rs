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

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::StableHostMachineId;
use config_version::ConfigVersion;

// Represents Instance returned via RPC call.
// Adds some widely used helpers.
#[derive(Debug, PartialEq)]
pub(in crate::tests) struct RpcInstance(rpc::forge::Instance);

impl RpcInstance {
    pub(in crate::tests) fn new(v: rpc::forge::Instance) -> Self {
        Self(v)
    }

    pub(in crate::tests) fn id(&self) -> InstanceId {
        self.0.id.unwrap()
    }

    pub(in crate::tests) fn machine_id(&self) -> StableHostMachineId {
        self.0.machine_id.unwrap()
    }

    pub(in crate::tests) fn rpc_id(&self) -> Option<InstanceId> {
        self.0.id
    }

    pub(in crate::tests) fn inner(&self) -> &rpc::forge::Instance {
        &self.0
    }

    pub(in crate::tests) fn into_inner(self) -> rpc::forge::Instance {
        self.0
    }

    pub(in crate::tests) fn status(&self) -> RpcInstanceStatus<'_> {
        RpcInstanceStatus::new(self.0.status.as_ref().unwrap())
    }

    pub(in crate::tests) fn config(&self) -> RpcInstanceConfig<'_> {
        RpcInstanceConfig::new(self.0.config.as_ref().unwrap())
    }

    pub(in crate::tests) fn config_version(&self) -> ConfigVersion {
        self.0.config_version.parse::<ConfigVersion>().unwrap()
    }

    #[allow(dead_code)]
    pub(in crate::tests) fn ib_config_version(&self) -> ConfigVersion {
        self.0.ib_config_version.parse::<ConfigVersion>().unwrap()
    }

    pub(in crate::tests) fn network_config_version(&self) -> ConfigVersion {
        self.0
            .network_config_version
            .parse::<ConfigVersion>()
            .unwrap()
    }

    pub(in crate::tests) fn metadata(&self) -> &rpc::Metadata {
        self.0.metadata.as_ref().unwrap()
    }
}

pub(in crate::tests) struct RpcInstanceStatus<'a>(&'a rpc::InstanceStatus);

impl<'a> RpcInstanceStatus<'a> {
    pub(in crate::tests) fn new(v: &'a rpc::forge::InstanceStatus) -> Self {
        Self(v)
    }

    pub(in crate::tests) fn inner(&self) -> &rpc::forge::InstanceStatus {
        self.0
    }

    pub(in crate::tests) fn tenant(&self) -> rpc::forge::TenantState {
        self.0.tenant.as_ref().unwrap().state()
    }

    pub(in crate::tests) fn network(&self) -> &'a rpc::forge::InstanceNetworkStatus {
        self.0.network.as_ref().unwrap()
    }

    pub(in crate::tests) fn infiniband(&self) -> &'a rpc::forge::InstanceInfinibandStatus {
        self.0.infiniband.as_ref().unwrap()
    }

    pub(in crate::tests) fn configs_synced(&self) -> rpc::SyncState {
        self.0.configs_synced()
    }
}

pub(in crate::tests) struct RpcInstanceConfig<'a>(&'a rpc::InstanceConfig);

impl<'a> RpcInstanceConfig<'a> {
    pub(in crate::tests) fn new(v: &'a rpc::forge::InstanceConfig) -> Self {
        Self(v)
    }

    pub(in crate::tests) fn inner(&self) -> &'a rpc::forge::InstanceConfig {
        self.0
    }

    pub(in crate::tests) fn tenant(&self) -> &'a rpc::forge::TenantConfig {
        self.0.tenant.as_ref().unwrap()
    }

    pub(in crate::tests) fn os(&self) -> &'a rpc::forge::InstanceOperatingSystemConfig {
        self.0.os.as_ref().unwrap()
    }

    pub(in crate::tests) fn network(&self) -> &'a rpc::forge::InstanceNetworkConfig {
        self.0.network.as_ref().unwrap()
    }

    pub(in crate::tests) fn infiniband(&self) -> &'a rpc::forge::InstanceInfinibandConfig {
        self.0.infiniband.as_ref().unwrap()
    }
}
