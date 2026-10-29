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
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use carbide_site_explorer::MachineCreator;
use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_test_harness::network::segment::TestNetworkSegment;
use carbide_test_harness::prelude::*;
use carbide_uuid::machine::DpuMachineId;
use component_manager::TestMachineInfoProvider;
use mac_address::MacAddress;
use model::expected_machine::ExpectedMachine;
use model::rack_type::{RackProfile, RackProfileConfig};
use model::site_explorer::{EndpointExplorationReport, ExploredDpu, ExploredManagedHost};
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_server::Forge;

pub(super) struct ExploredHostFixture {
    pub(super) host: ExploredManagedHost,
    pub(super) host_report: EndpointExplorationReport,
    pub(super) dpu_machine_ids: HashMap<u8, DpuMachineId>,
}

pub(super) struct Env {
    pub(super) pool: PgPool,
    pub(super) underlay_segment: TestNetworkSegment,
    pub(super) test_harness: TestHarness,
}

pub(super) const TEST_MACHINE_INFO_RACK_PROFILE_ID: &str = "NVL72";

impl Env {
    pub(super) async fn new(pool: PgPool) -> Self {
        let test_harness = TestHarness::builder(pool.clone()).build().await;
        let domain = test_harness.test_domain().await;
        let network_controller = test_harness.network_controller();
        let underlay_segment = network_controller.create_underlay_segment(&domain).await;
        network_controller.create_admin_segment(&domain).await;
        Self {
            pool,
            underlay_segment,
            test_harness,
        }
    }

    pub(super) fn api(&self) -> &Api {
        self.test_harness.api()
    }
}

pub(super) fn machine_creator_config() -> SiteExplorerConfig {
    SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        explorations_per_run: 2,
        concurrent_explorations: 1,
        run_interval: Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        create_switches: Arc::new(true.into()),
        switches_created_per_run: 1,
        ..Default::default()
    }
}

pub(super) fn machine_creator(env: &Env, config: SiteExplorerConfig) -> MachineCreator {
    machine_creator_with_dpf(env, config, false)
}

pub(super) fn machine_creator_with_dpf(
    env: &Env,
    config: SiteExplorerConfig,
    dpf_enabled_at_site: bool,
) -> MachineCreator {
    MachineCreator::new(
        env.pool.clone(),
        (&config).into(),
        env.api().common_pools().clone(),
        Arc::new(env.api().runtime_config.rack_profiles.clone()),
        None,
        dpf_enabled_at_site,
    )
}

pub(super) fn machine_creator_with_info_provider(
    env: &Env,
    provider: Arc<TestMachineInfoProvider>,
) -> MachineCreator {
    MachineCreator::new(
        env.pool.clone(),
        (&machine_creator_config()).into(),
        env.api().common_pools().clone(),
        Arc::new(machine_info_rack_profiles()),
        Some(provider),
        false,
    )
}

pub(super) fn machine_info_rack_profiles() -> RackProfileConfig {
    RackProfileConfig {
        rack_profiles: [(
            TEST_MACHINE_INFO_RACK_PROFILE_ID.to_string(),
            RackProfile::default(),
        )]
        .into_iter()
        .collect(),
    }
}

pub(super) fn expected_machine(managed_host: &ManagedHostConfig) -> ExpectedMachine {
    ExpectedMachine {
        id: Some(uuid::Uuid::new_v4()),
        bmc_mac_address: managed_host.bmc_mac_address,
        data: managed_host
            .expected_machine_data
            .clone()
            .unwrap_or_default(),
    }
}

pub(super) async fn discover_bmc(
    api: &Api,
    segment: TestNetworkSegment,
    mac_address: MacAddress,
    vendor_string: &str,
) -> IpAddr {
    api.discover_dhcp(
        rpc::forge::DhcpDiscovery::builder(mac_address, segment.relay_address)
            .vendor_string(vendor_string)
            .tonic_request(),
    )
    .await
    .expect("BMC DHCP discovery should succeed")
    .into_inner()
    .address
    .parse()
    .expect("DHCP response address should be an IP address")
}

pub(super) async fn explored_host_fixture(
    env: &Env,
    managed_host: &ManagedHostConfig,
) -> ExploredHostFixture {
    let host_bmc_ip = discover_bmc(
        env.api(),
        env.underlay_segment,
        managed_host.bmc_mac_address,
        "NVIDIA/OOB",
    )
    .await;

    let mut dpu_bmc_ips = Vec::new();
    for (dpu_index, dpu) in managed_host.dpus.iter().enumerate() {
        let dpu_index = dpu_index.try_into().expect("DPU index should fit into u8");
        let dpu_bmc_ip = discover_bmc(
            env.api(),
            env.underlay_segment,
            dpu.bmc_mac_address,
            "NVIDIA/BF/BMC",
        )
        .await;
        dpu_bmc_ips.push((dpu_index, dpu_bmc_ip));
    }

    let results = managed_host
        .exploration_results(Some(host_bmc_ip), &dpu_bmc_ips)
        .expect("managed host exploration results should be generated");
    let dpu_machine_ids = results.dpu_machine_ids();
    let (_, host_report) = results
        .host_report
        .expect("host exploration report should exist");
    let dpus = results
        .dpu_reports
        .into_iter()
        .map(|dpu| ExploredDpu {
            bmc_ip: dpu.bmc_ip,
            host_pf_mac_address: managed_host
                .dpus
                .get(dpu.dpu_index as usize)
                .map(|config| config.host_mac_address),
            host_chassis_id: None,
            report: Arc::new(dpu.report),
        })
        .collect();

    ExploredHostFixture {
        host: ExploredManagedHost { host_bmc_ip, dpus },
        host_report,
        dpu_machine_ids,
    }
}
