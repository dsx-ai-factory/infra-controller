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

use std::ops::Deref;
use std::sync::Arc;

pub use carbide_api_core::cfg::file::CarbideConfig;
use carbide_api_core::test_support::rpc::forge::forge_server::Forge;
pub use carbide_api_core::test_support::{self, Api, rpc};
use carbide_site_explorer::config::SiteExplorerConfig;
pub use carbide_site_explorer::test_support::{MockEndpointExplorer, TestSiteExplorer};
use carbide_site_explorer::{EndpointExplorationService, SiteExplorer};
use carbide_utils::test_support::test_meter::TestMeter;
use carbide_uuid::machine::MachineId;
use sqlx::{PgPool, PgTransaction};
use tokio::task::JoinSet;
use tokio_util::sync::{CancellationToken, DropGuard};
use tonic::Request;

use crate::asset::{TestExpectedSwitch, TestPowerShelf, TestRack, TestSwitch};
use crate::builder::TestHarnessBuilder;
use crate::dns::TestDomain;
use crate::network::controller::TestNetworkController;
pub use crate::network::segment::TestNetworkSegment;

pub mod asset;
pub mod builder;
pub mod db_machine;
pub mod dns;
pub mod machine;
pub mod machine_dpu;
pub mod machine_host;
pub mod managed_host;
pub mod network;
pub mod prelude;
pub mod resource_pool;

pub use db_machine::DbMachineExt;
pub use machine::TestMachine;
pub use machine_dpu::TestDpuMachine;
pub use machine_host::TestHostMachine;
pub use managed_host::{TestManagedHost, TestManagedHostBuildData, TestManagedHostBuilder};

pub struct TestHarness {
    api: Arc<ApiHandle>,
    pub test_meter: TestMeter,
    processor_id: String,
}

impl TestHarness {
    pub fn builder(db_pool: PgPool) -> TestHarnessBuilder {
        builder::TestHarnessBuilder {
            db_pool,
            api_builder_fn: None,
            test_meter: None,
            pools: None,
        }
    }

    pub fn api(&self) -> &Api {
        self.api.deref()
    }

    pub fn api_arc(&self) -> Arc<Api> {
        self.api.api.clone()
    }

    pub async fn db_txn(&self) -> PgTransaction<'static> {
        self.api
            .database_connection
            .begin()
            .await
            .expect("database transaction should start")
    }

    pub async fn test_domain(&self) -> TestDomain {
        self.create_test_domain("testharness.example.com").await
    }

    pub async fn create_test_domain(&self, name: impl Into<String>) -> TestDomain {
        let name = name.into();
        let id = self
            .api
            .create_domain(Request::new(rpc::protos::dns::CreateDomainRequest {
                name: name.clone(),
            }))
            .await
            .unwrap()
            .into_inner()
            .id
            .map(::carbide_uuid::domain::DomainId::try_from)
            .unwrap()
            .unwrap();
        TestDomain { id, name }
    }

    pub fn network_controller(&self) -> TestNetworkController {
        TestNetworkController::new(
            self.api.clone(),
            self.processor_id.clone(),
            &self.test_meter,
        )
    }

    pub fn managed_host_builder<'a>(
        &'a self,
        site_explorer: &'a TestSiteExplorer,
        segment: TestNetworkSegment,
    ) -> TestManagedHostBuilder<'a> {
        TestManagedHostBuilder::new(self, site_explorer, segment)
    }

    pub fn test_site_explorer(&self, config: SiteExplorerConfig) -> TestSiteExplorer {
        let endpoint_explorer = Arc::new(MockEndpointExplorer::default());
        let api = self.api();
        let endpoint_exploration_service = Arc::new(EndpointExplorationService::new(
            api.database_connection.clone(),
            endpoint_explorer.clone(),
            Arc::new(api.runtime_config.get_firmware_config()),
        ));
        let site_explorer = SiteExplorer::new(
            api.database_connection.clone(),
            config,
            self.test_meter.meter(),
            endpoint_exploration_service,
            endpoint_explorer.clone(),
            api.common_pools().clone(),
            api.work_lock_manager_handle(),
            api.runtime_config.rack_profiles.clone(),
            None,
            api.credential_manager().clone(),
        );
        TestSiteExplorer::new(site_explorer, endpoint_explorer)
    }

    pub fn default_test_site_explorer(&self) -> TestSiteExplorer {
        self.test_site_explorer(SiteExplorerConfig::default())
    }

    pub async fn find_machine(&self, id: MachineId) -> Vec<rpc::forge::Machine> {
        self.api
            .find_machines_by_ids(Request::new(rpc::forge::MachinesByIdsRequest {
                machine_ids: vec![id],
                include_history: true,
            }))
            .await
            .expect("machine lookup by id should succeed")
            .into_inner()
            .machines
    }

    /// Creates a randomly identified rack with the supplied profile and default config.
    pub async fn create_rack(
        &self,
        rack_profile_id: carbide_uuid::rack::RackProfileId,
    ) -> TestRack {
        TestRack::create(self, rack_profile_id).await
    }

    pub async fn create_switch(&self, slot_number: i32, tray_index: i32) -> TestSwitch {
        TestSwitch::create(self, slot_number, tray_index).await
    }

    /// Creates an expected-switch fixture through the Forge API.
    pub async fn create_expected_switch(
        &self,
        expected_switch: rpc::forge::ExpectedSwitch,
    ) -> TestExpectedSwitch {
        TestExpectedSwitch::create(self, expected_switch).await
    }

    /// Creates a power shelf directly in the database with the supplied config.
    ///
    /// The ID is derived from `config.name`, so repeated names collide in the
    /// same database. BMC MAC, metadata, and rack association are left unset.
    pub async fn create_power_shelf(
        &self,
        config: model::power_shelf::PowerShelfConfig,
    ) -> TestPowerShelf {
        TestPowerShelf::create(self, config).await
    }
}

pub(crate) struct ApiHandle {
    api: Arc<Api>,
    cancel_token: CancellationToken,
    _drop_guard: DropGuard,
    _js: JoinSet<()>,
}

impl Deref for ApiHandle {
    type Target = Api;
    fn deref(&self) -> &Self::Target {
        self.api.as_ref()
    }
}

#[ctor::ctor(unsafe)]
fn setup_test_logging() {
    carbide_api_core::test_support::setup_test_logging()
}
