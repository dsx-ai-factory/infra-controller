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

use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use carbide_uuid::network::NetworkSegmentId;
use model::test_support::ManagedHostConfig;
use rpc::forge::instance_interface_config::NetworkDetails;

pub(super) struct InstanceFixture {
    pub(super) harness: TestHarness,
    pub(super) mh: TestManagedHost,
    pub(super) segment_id: NetworkSegmentId,
}

impl InstanceFixture {
    pub(super) async fn new(pool: PgPool) -> Self {
        let harness = TestHarness::builder(pool)
            .with_resource_pools(
                ResourcePoolBuilder::default()
                    .with_vlan_ids(1, 5)
                    .with_vnis(10_001, 10_005)
                    .build(),
            )
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
        let mut mh = harness
            .managed_host_builder(&site_explorer, underlay_segment)
            .with_config(ManagedHostConfig::default())
            .with_dpu_network_status_reported()
            .build()
            .await
            .0;
        mh.host.discover_primary_iface(admin_segment).await;
        mh.advance_to_converged_ready().await;
        Self {
            harness,
            mh,
            segment_id,
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
