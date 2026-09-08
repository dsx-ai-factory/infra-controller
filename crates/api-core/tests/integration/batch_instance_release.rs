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

use std::sync::Arc;

use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_test_harness::TestNetworkSegment;
use carbide_test_harness::dns::TestDomain;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::HostMachineId;
use carbide_uuid::network::NetworkSegmentId;
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_server::Forge;
use rpc::forge::instance_interface_config::NetworkDetails;
use tonic::Request;
use uuid::Uuid;

const TENANT_ORG: &str = "2829bbe3-c169-4cd9-8b2a-19a8b1618a93";

struct TestEnv {
    api: Arc<Api>,
    harness: TestHarness,
    domain: TestDomain,
    admin_segment: TestNetworkSegment,
    underlay_segment: TestNetworkSegment,
    site_explorer: TestSiteExplorer,
}

impl TestEnv {
    /// DPU-managed hosts (this file always builds those, via
    /// `with_dpu_network_status_reported`) can only allocate an instance into
    /// a network segment that's attached to a VPC -- the raw `underlay_segment`
    /// doesn't qualify. Mirrors `compute_allocation.rs`'s identical helper.
    async fn create_vpc_and_tenant_segment(&self) -> NetworkSegmentId {
        let network_controller = self.harness.network_controller();
        let vpc_id = network_controller.create_vpc("test vpc 1").await;
        network_controller
            .create_tenant_segment(&self.domain, vpc_id)
            .await
            .id
    }
}

struct TestManagedHost {
    id: HostMachineId,
}

async fn create_test_env(pool: PgPool) -> TestEnv {
    let resource_pools = ResourcePoolBuilder::default()
        .with_vlan_ids(1, 5)
        .with_vnis(10_001, 10_005)
        .build();
    let harness = TestHarness::builder(pool)
        .with_resource_pools(resource_pools)
        .build()
        .await;

    let domain = harness.test_domain().await;
    let network_controller = harness.network_controller();
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    let site_explorer = harness.test_site_explorer(SiteExplorerConfig::default());
    let api = harness.api_arc();

    let env = TestEnv {
        api,
        harness,
        domain,
        admin_segment,
        underlay_segment,
        site_explorer,
    };

    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: "batch-release-test-tenant".to_string(),
                description: String::new(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();

    env
}

async fn create_managed_host(env: &TestEnv) -> TestManagedHost {
    let mut mh = env
        .harness
        .managed_host_builder(&env.site_explorer, env.underlay_segment)
        .with_config(ManagedHostConfig::default())
        .with_dpu_network_status_reported()
        .build()
        .await
        .0;
    mh.host.discover_primary_iface(env.admin_segment).await;
    mh.advance_to_converged_ready().await;
    TestManagedHost { id: mh.host.id }
}

fn single_interface_network_config(
    segment_id: NetworkSegmentId,
) -> rpc::forge::InstanceNetworkConfig {
    rpc::forge::InstanceNetworkConfig {
        interfaces: vec![rpc::forge::InstanceInterfaceConfig {
            function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
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

fn default_os_config() -> rpc::forge::InstanceOperatingSystemConfig {
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

async fn allocate_instance(
    env: &TestEnv,
    host: &TestManagedHost,
    segment_id: NetworkSegmentId,
) -> rpc::forge::Instance {
    env.api
        .allocate_instance(Request::new(rpc::forge::InstanceAllocationRequest {
            instance_id: None,
            machine_id: Some(host.id.into()),
            instance_type_id: None,
            config: Some(rpc::forge::InstanceConfig {
                tenant: Some(rpc::forge::TenantConfig {
                    tenant_organization_id: TENANT_ORG.to_string(),
                    tenant_keyset_ids: vec![],
                    hostname: None,
                }),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: None,
                nvlink: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                spxconfig: None,
                power_profile: None,
            }),
            metadata: None,
            allow_unhealthy_machine: false,
        }))
        .await
        .unwrap()
        .into_inner()
}

fn release_request(instance_id: InstanceId) -> rpc::forge::InstanceReleaseRequest {
    rpc::forge::InstanceReleaseRequest {
        id: Some(instance_id),
        issue: None,
        is_repair_tenant: None,
        delete_attribution: None,
    }
}

/// A batch of otherwise-valid instances all release successfully in one call.
#[sqlx_test]
async fn test_batch_release_all_succeed(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let mut release_requests = Vec::new();
    let mut expected_ids = Vec::new();
    for _ in 0..3 {
        let host = create_managed_host(&env).await;
        let instance = allocate_instance(&env, &host, segment_id).await;
        let instance_id = instance.id.unwrap();
        expected_ids.push(instance_id);
        release_requests.push(release_request(instance_id));
    }

    let response = env
        .api
        .release_instances(Request::new(rpc::forge::BatchInstanceReleaseRequest {
            release_requests,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(response.results.len(), expected_ids.len());
    for result in &response.results {
        assert_eq!(
            result.status,
            rpc::forge::InstanceReleaseStatusCode::Success as i32,
            "{:?}",
            result
        );
        assert!(expected_ids.contains(&result.id.unwrap()));
    }

    Ok(())
}

/// One bad instance ID in the batch is reported as a per-instance failure and
/// does not prevent the other, valid instances in the same batch from being
/// released -- this is the best-effort semantics the RPC is designed around.
#[sqlx_test]
async fn test_batch_release_continues_past_a_failed_instance(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let host_a = create_managed_host(&env).await;
    let instance_a = allocate_instance(&env, &host_a, segment_id).await;
    let instance_a_id = instance_a.id.unwrap();

    let host_b = create_managed_host(&env).await;
    let instance_b = allocate_instance(&env, &host_b, segment_id).await;
    let instance_b_id = instance_b.id.unwrap();

    let nonexistent_id: InstanceId = Uuid::new_v4().into();

    let response = env
        .api
        .release_instances(Request::new(rpc::forge::BatchInstanceReleaseRequest {
            release_requests: vec![
                release_request(instance_a_id),
                release_request(nonexistent_id),
                release_request(instance_b_id),
            ],
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(response.results.len(), 3);

    let succeeded: Vec<_> = response
        .results
        .iter()
        .filter(|r| r.status == rpc::forge::InstanceReleaseStatusCode::Success as i32)
        .filter_map(|r| r.id)
        .collect();
    assert_eq!(succeeded.len(), 2);
    assert!(succeeded.contains(&instance_a_id));
    assert!(succeeded.contains(&instance_b_id));

    let failed: Vec<_> = response
        .results
        .iter()
        .filter(|r| r.status != rpc::forge::InstanceReleaseStatusCode::Success as i32)
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].id, Some(nonexistent_id));
    assert_eq!(
        failed[0].status,
        rpc::forge::InstanceReleaseStatusCode::NotFound as i32
    );

    Ok(())
}

/// An empty batch is not an error (unlike `AllocateInstances`, which rejects
/// an empty batch) -- there is simply nothing to release, so the response
/// comes back with an empty `results`.
#[sqlx_test]
async fn test_batch_release_empty_request_is_a_no_op(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let response = env
        .api
        .release_instances(Request::new(rpc::forge::BatchInstanceReleaseRequest {
            release_requests: vec![],
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(response.results.is_empty());

    Ok(())
}

/// A batch entry with no `id` is reported as a failure with `id` unset,
/// rather than silently dropped -- so response counts always reconcile
/// against the request count -- and does not prevent the rest of the batch
/// from being released.
#[sqlx_test]
async fn test_batch_release_entry_with_no_id_is_reported_as_a_failure(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let host = create_managed_host(&env).await;
    let instance = allocate_instance(&env, &host, segment_id).await;
    let instance_id = instance.id.unwrap();

    let no_id_request = rpc::forge::InstanceReleaseRequest {
        id: None,
        issue: None,
        is_repair_tenant: None,
        delete_attribution: None,
    };

    // The no-id entry is placed FIRST so this test actually proves the batch
    // loop continues past it, rather than merely proving a no-id entry after
    // the last real entry gets recorded (which would pass even if the loop
    // stopped processing entirely after hitting a no-id entry).
    let response = env
        .api
        .release_instances(Request::new(rpc::forge::BatchInstanceReleaseRequest {
            release_requests: vec![no_id_request, release_request(instance_id)],
        }))
        .await
        .unwrap()
        .into_inner();

    // Results are ordered 1:1 with the request, so index 0 is the no-id entry
    // and index 1 is the real instance -- no searching required.
    assert_eq!(response.results.len(), 2);
    assert_eq!(response.results[0].id, None);
    assert_eq!(
        response.results[0].status,
        rpc::forge::InstanceReleaseStatusCode::InvalidArgument as i32
    );
    assert_eq!(response.results[1].id, Some(instance_id));
    assert_eq!(
        response.results[1].status,
        rpc::forge::InstanceReleaseStatusCode::Success as i32
    );

    Ok(())
}
