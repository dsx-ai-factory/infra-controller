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

mod controller;

use std::sync::Arc;

use carbide_api_core::test_support::{default_config, ib_fabric_test_manager};
use carbide_nvlink_manager::nvlink::test_support::NmxcSimClient;
use carbide_redfish::libredfish::test_support::RedfishSim;
use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, Credentials};
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_test_harness::dns::TestDomain;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use carbide_test_harness::{CarbideConfig, TestNetworkSegment};
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use controller::{ControllerClients, InstanceControllers};
use ipnetwork::IpNetwork;
use model::resource_pool;
use model::test_support::ManagedHostConfig;
use rpc::forge::instance_interface_config::NetworkDetails;
use tokio::sync::OnceCell;
use tonic::IntoRequest;

pub(super) struct InstanceEnv {
    pub(super) harness: TestHarness,
    pub(super) segment_id: NetworkSegmentId,
    pub(super) domain: TestDomain,
    admin_segment: TestNetworkSegment,
    underlay_segment: TestNetworkSegment,
    site_explorer: TestSiteExplorer,
    instance_controllers: OnceCell<InstanceControllers>,
    controller_clients: ControllerClients,
}

impl InstanceEnv {
    pub(super) async fn new(pool: PgPool, config: CarbideConfig) -> Self {
        let mut pools = ResourcePoolBuilder::default()
            .with_vlan_ids(1, 5)
            .with_vnis(10_001, 10_005)
            .with_integer_pool(resource_pool::common::DPA_VNI, 40_001, 40_005);
        if config.fnn.is_some() {
            pools = pools
                .with_integer_pool(resource_pool::common::EXTERNAL_VPC_VNI, 50_001, 50_005)
                .with_integer_pool(resource_pool::common::FNN_ASN, 30_001, 30_035);
        }
        let credentials = Arc::new(TestCredentialManager::default());
        let ib = ib_fabric_test_manager(&config, credentials.clone());
        let redfish = Arc::new(RedfishSim::default());
        let nmxc = Arc::new(NmxcSimClient::default());
        let controller_clients = ControllerClients {
            redfish: redfish.clone(),
            ib: ib.clone(),
            nmxc: nmxc.clone(),
        };
        let harness = TestHarness::builder(pool)
            .with_resource_pools(pools.build())
            .with_api_builder_fn(move |builder| {
                builder
                    .with_runtime_config(Arc::new(config))
                    .with_credential_manager(credentials)
                    .with_ib_fabric_manager(ib)
                    .with_redfish_pool(redfish)
                    .with_nmxc_client_pool(nmxc)
            })
            .build()
            .await;
        let domain = harness.create_test_domain("dwrt1.com").await;
        let network_controller = harness.network_controller();
        let admin_segment = network_controller.create_admin_segment(&domain).await;
        let underlay_segment = network_controller.create_underlay_segment(&domain).await;
        let vpc_id = network_controller.create_vpc("test vpc 1").await;
        let segment_id = network_controller
            .create_tenant_segment(&domain, vpc_id)
            .await
            .id;
        let site_explorer = harness.default_test_site_explorer();
        Self {
            harness,
            segment_id,
            domain,
            admin_segment,
            underlay_segment,
            site_explorer,
            instance_controllers: OnceCell::new(),
            controller_clients,
        }
    }

    pub(super) async fn create_tenant_segment(
        &self,
        vpc_id: VpcId,
        network: IpNetwork,
        name: &str,
    ) -> NetworkSegmentId {
        let segment = self
            .harness
            .api()
            .create_network_segment(
                rpc::forge::NetworkSegmentCreationRequest {
                    id: None,
                    mtu: Some(1500),
                    name: name.to_string(),
                    prefixes: vec![rpc::forge::NetworkPrefix {
                        id: None,
                        prefix: ipnetwork::IpNetwork::new(network.network(), network.prefix())
                            .unwrap()
                            .to_string(),
                        gateway: Some(network.ip().to_string()),
                        reserve_first: 3,
                        free_ip_count: 0,
                        svi_ip: None,
                        free_ip_count_v2: None,
                        free_ip_count_saturated: false,
                        dhcpv6_link_address: None,
                    }],
                    subdomain_id: Some(self.domain.id),
                    vpc_id: Some(vpc_id),
                    segment_type: rpc::forge::NetworkSegmentType::Tenant as i32,
                    infer_slaac_eui64_addresses: false,
                }
                .into_request(),
            )
            .await
            .unwrap()
            .into_inner();
        // Get the tenant segment into ready state, as in the shared network fixtures.
        let network_controller = self.harness.network_controller();
        network_controller.run_single_iteration().await;
        network_controller.run_single_iteration().await;
        segment.id.unwrap()
    }

    pub(super) async fn advance_instance_to_ready(
        &self,
        mh: &TestManagedHost,
        instance: &TestInstance,
    ) {
        let controllers = self
            .instance_controllers
            .get_or_init(|| async {
                InstanceControllers::new(&self.harness, &self.controller_clients)
            })
            .await;
        controllers
            .advance_instance_to_ready(&self.harness, mh, instance)
            .await;
    }

    pub(super) async fn create_managed_host(&self, config: ManagedHostConfig) -> TestManagedHost {
        let mut mh = self
            .harness
            .managed_host_builder(&self.site_explorer, self.underlay_segment)
            .with_config(config)
            .with_dpu_network_status_reported()
            .build()
            .await
            .0;
        mh.host.discover_primary_iface(self.admin_segment).await;
        mh.advance_to_converged_ready().await;
        // Completed ingestion records BIOS setup; idle Ready hosts must remain stable when
        // the instance fixture drives the shared machine controller for a sibling host.
        let mut txn = self.harness.db_txn().await;
        db::machine::update_bios_password_set_time(&mh.host.id, txn.as_mut())
            .await
            .unwrap();
        txn.commit().await.unwrap();
        for bmc_mac_address in
            std::iter::once(mh.host.bmc_mac).chain(mh.dpus.iter().map(|dpu| dpu.bmc_mac))
        {
            self.harness
                .api()
                .credential_manager()
                .set_credentials(
                    &CredentialKey::BmcCredentials {
                        credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
                    },
                    &Credentials::UsernamePassword {
                        username: "root".to_string(),
                        password: "notforprod".to_string(),
                    },
                )
                .await
                .unwrap();
        }
        for dpu in &mh.dpus {
            let status = dpu.network_status().dpu_agent_version("test").build().await;
            self.harness
                .api()
                .record_dpu_network_status(status.into_request())
                .await
                .unwrap();
        }
        mh
    }
}

pub(super) struct InstanceFixture {
    pub(super) harness: TestHarness,
    pub(super) mh: TestManagedHost,
    pub(super) segment_id: NetworkSegmentId,
}

impl InstanceFixture {
    pub(super) async fn new(pool: PgPool) -> Self {
        let env = InstanceEnv::new(pool, default_config::get()).await;
        let mh = env.create_managed_host(ManagedHostConfig::default()).await;
        Self {
            harness: env.harness,
            mh,
            segment_id: env.segment_id,
        }
    }
}

pub(super) fn default_os_config() -> rpc::forge::InstanceOperatingSystemConfig {
    rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: Some("SomeRandomData".to_string()),
        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
            rpc::forge::InlineIpxe {
                ipxe_script: "SomeRandomiPxe".to_string(),
            },
        )),
    }
}

pub(super) fn default_tenant_config() -> rpc::TenantConfig {
    rpc::TenantConfig {
        tenant_organization_id: "Tenant1".to_string(),
        tenant_keyset_ids: vec![],
        hostname: None,
    }
}

pub(super) fn single_interface_network_config(
    segment_id: NetworkSegmentId,
) -> rpc::InstanceNetworkConfig {
    rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: Some(segment_id),
            network_details: Some(NetworkDetails::SegmentId(segment_id)),
            device: None,
            device_instance: 0,
            virtual_function_id: None,
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        }],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    }
}

pub(super) fn single_interface_network_config_with_vfs(
    segment_ids: Vec<NetworkSegmentId>,
) -> rpc::InstanceNetworkConfig {
    let mut segment_iter = segment_ids.into_iter().enumerate();
    let (_function_id, segment_id) = segment_iter.next().unwrap();
    let mut interfaces = vec![rpc::InstanceInterfaceConfig {
        function_type: rpc::InterfaceFunctionType::Physical as i32,
        network_segment_id: Some(segment_id),
        network_details: Some(NetworkDetails::SegmentId(segment_id)),
        device: None,
        device_instance: 0,
        virtual_function_id: None,
        ip_address: None,
        ipv6_interface_config: None,
        routing_profile: None,
    }];

    interfaces.extend(
        segment_iter.map(|(function_id, segment_id)| rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Virtual as i32,
            network_segment_id: Some(segment_id),
            network_details: Some(NetworkDetails::SegmentId(segment_id)),
            device: None,
            device_instance: 0,
            virtual_function_id: Some(function_id as u32),
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        }),
    );

    rpc::InstanceNetworkConfig {
        interfaces,
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    }
}
