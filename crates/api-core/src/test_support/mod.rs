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

pub mod builder;
pub mod default_config;
pub mod fixture_config;
pub mod health;
pub(crate) mod ib_fabric;
pub(crate) mod ib_guid_pool;
pub mod mac_address_pool;
pub mod metadata;
pub mod network;
pub mod network_segment;
pub mod redfish;

use std::sync::Arc;
use std::time::Duration;

use carbide_secrets::credentials::CredentialManager;
use db::work_lock_manager::WorkLockManagerHandle;
use model::resource_pool::common::CommonPools;
pub use rpc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub use crate::api::Api;
pub use crate::api::metrics::ApiMetricsEmitter;
pub use crate::logging::setup::dep_log_filter;

pub const MAX_BGP_PASSWORD_LENGTH: usize = crate::handlers::credential::MAX_BGP_PASSWORD_LENGTH;

pub fn default_credential_key(credential: &crate::api::DefaultCredential) -> &str {
    credential.key()
}

impl Api {
    pub fn work_lock_manager_handle(&self) -> WorkLockManagerHandle {
        self.work_lock_manager_handle.clone()
    }

    pub fn common_pools(&self) -> &Arc<CommonPools> {
        &self.common_pools
    }

    pub fn credential_manager(&self) -> &Arc<dyn CredentialManager> {
        &self.credential_manager
    }

    pub fn start_dynamic_settings_reset_task(
        &self,
        join_set: &mut JoinSet<()>,
        period: Duration,
        cancel_token: CancellationToken,
    ) {
        self.dynamic_settings
            .start_reset_task(join_set, period, cancel_token);
    }

    pub async fn process_scout_req_for_test(
        &self,
        machine_id: carbide_uuid::machine::HostMachineId,
    ) -> crate::CarbideResult<rpc::forge_agent_control_response::Action> {
        crate::handlers::process_scout_req_for_test(self, machine_id).await
    }
}

pub fn setup_test_logging() {
    carbide_test_support::setup_test_logging("carbide-api");
}
