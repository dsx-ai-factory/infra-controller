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
use std::sync::Arc;

use carbide_api_core::cfg::file::ComputeAllocationEnforcement;
use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_test_harness::TestNetworkSegment;
use carbide_test_harness::dns::TestDomain;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use carbide_uuid::instance_type::InstanceTypeId;
use carbide_uuid::machine::HostMachineId;
use carbide_uuid::network::NetworkSegmentId;
use model::instance_type::InstanceTypeMachineCapabilityFilter;
use model::machine::capabilities::MachineCapabilityType;
use model::metadata::Metadata as DbMetadata;
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_server::Forge;
use rpc::forge::instance_interface_config::NetworkDetails;
use rpc::forge::{
    ComputeAllocationAttributes, CreateComputeAllocationRequest, DeleteComputeAllocationRequest,
    UpdateComputeAllocationRequest,
};
use tonic::{Code, Request};
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
    async fn create_vpc_and_tenant_segment(&self) -> NetworkSegmentId {
        let network_controller = self.harness.network_controller();
        let vpc_id = network_controller.create_vpc("test vpc 1").await;
        network_controller
            .create_tenant_segment(&self.domain, vpc_id)
            .await
            .id
    }
}

#[derive(Default)]
struct TestEnvOverrides {
    compute_allocation_enforcement: Option<ComputeAllocationEnforcement>,
}

impl TestEnvOverrides {
    fn with_compute_allocation_enforcement(
        self,
        enforcement: ComputeAllocationEnforcement,
    ) -> Self {
        Self {
            compute_allocation_enforcement: Some(enforcement),
        }
    }
}

struct TestManagedHost {
    id: HostMachineId,
}

async fn create_test_env(pool: PgPool) -> TestEnv {
    create_test_env_with_overrides(pool, TestEnvOverrides::default()).await
}

async fn create_test_env_with_overrides(pool: PgPool, overrides: TestEnvOverrides) -> TestEnv {
    let mut runtime_config = carbide_test_harness::test_support::default_config::get();
    if let Some(enforcement) = overrides.compute_allocation_enforcement {
        runtime_config.compute_allocation_enforcement = enforcement;
    }
    let runtime_config = Arc::new(runtime_config);
    let resource_pools = ResourcePoolBuilder::default()
        .with_vlan_ids(1, 5)
        .with_vnis(10_001, 10_005)
        .build();
    let harness = TestHarness::builder(pool)
        .with_resource_pools(resource_pools)
        .with_api_builder_fn(move |builder| builder.with_runtime_config(runtime_config))
        .build()
        .await;

    let mut txn = harness.db_txn().await;
    for _ in 0..3 {
        let uid = Uuid::new_v4();
        let desired_capabilities = vec![InstanceTypeMachineCapabilityFilter {
            capability_type: MachineCapabilityType::Cpu,
            ..Default::default()
        }];
        let metadata = DbMetadata {
            name: format!("the best type {uid}"),
            description: String::new(),
            labels: HashMap::new(),
        };
        db::instance_type::create(
            &mut txn,
            &InstanceTypeId::from(uid),
            &metadata,
            &desired_capabilities,
        )
        .await
        .unwrap();
    }
    txn.commit().await.unwrap();

    let domain = harness.test_domain().await;
    let network_controller = harness.network_controller();
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    let site_explorer = harness.test_site_explorer(SiteExplorerConfig::default());
    let api = harness.api_arc();

    TestEnv {
        api,
        harness,
        domain,
        admin_segment,
        underlay_segment,
        site_explorer,
    }
}

async fn get_instance_type_fixture_id(env: &TestEnv) -> String {
    let existing_instance_type_ids = env
        .api
        .find_instance_type_ids(Request::new(rpc::forge::FindInstanceTypeIdsRequest {}))
        .await
        .unwrap()
        .into_inner()
        .instance_type_ids;

    env.api
        .find_instance_types_by_ids(Request::new(rpc::forge::FindInstanceTypesByIdsRequest {
            instance_type_ids: existing_instance_type_ids,
            include_allocation_stats: false,
            tenant_organization_id: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .instance_types
        .pop()
        .unwrap()
        .id
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

fn metadata(name: impl Into<String>) -> rpc::forge::Metadata {
    rpc::forge::Metadata {
        name: name.into(),
        description: String::new(),
        labels: vec![],
    }
}

async fn create_compute_allocation(
    env: &TestEnv,
    instance_type_id: &str,
    count: u32,
    name: &str,
) -> rpc::forge::ComputeAllocation {
    // Create allocation for this scenario.
    // Expect success: tenant and type are valid.
    env.api
        .create_compute_allocation(
            CreateComputeAllocationRequest::builder(TENANT_ORG)
                .created_by("tests")
                .metadata(metadata(name))
                .attributes(ComputeAllocationAttributes::builder(instance_type_id, count).rpc())
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .allocation
        .unwrap()
}

async fn allocate_instance(
    env: &TestEnv,
    host: &TestManagedHost,
    instance_type_id: Option<&str>,
    segment_id: NetworkSegmentId,
) -> Result<tonic::Response<rpc::forge::Instance>, tonic::Status> {
    // Attempt instance allocation for this case.
    // Caller asserts expected success/failure.
    env.api
        .allocate_instance(Request::new(rpc::forge::InstanceAllocationRequest {
            instance_id: None,
            machine_id: Some(host.id.into()),
            instance_type_id: instance_type_id.map(str::to_string),
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
}

async fn update_compute_allocation(
    env: &TestEnv,
    allocation: &rpc::forge::ComputeAllocation,
    count: u32,
    name: &str,
) -> Result<tonic::Response<rpc::forge::UpdateComputeAllocationResponse>, tonic::Status> {
    // Attempt allocation update for this case.
    // Caller asserts expected success/failure.
    env.api
        .update_compute_allocation(
            UpdateComputeAllocationRequest::builder(TENANT_ORG)
                .id(allocation.id.unwrap())
                .metadata(metadata(name))
                .attributes(
                    ComputeAllocationAttributes::builder(
                        &allocation.attributes.as_ref().unwrap().instance_type_id,
                        count,
                    )
                    .rpc(),
                )
                .updated_by("tests")
                .tonic_request(),
        )
        .await
}

#[sqlx_test]
async fn test_compute_allocation_basic_actions(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let host = create_managed_host(&env).await;
    // Bind host to this instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host.id.to_string()],
            },
        ))
        .await
        .unwrap();

    let allocation_name = format!("alloc-basic-{}", Uuid::new_v4());
    // Make one allocation for basic CRUD checks.
    // Expect success with valid tenant/type.
    let allocation = create_compute_allocation(&env, &instance_type_id, 1, &allocation_name).await;

    // Query IDs for the created allocation.
    // Expect one ID due to exact filters.
    let found_ids = env
        .api
        .find_compute_allocation_ids(Request::new(rpc::forge::FindComputeAllocationIdsRequest {
            name: Some(allocation_name.clone()),
            tenant_organization_id: Some(TENANT_ORG.to_string()),
            instance_type_id: Some(instance_type_id.clone()),
        }))
        .await
        .unwrap()
        .into_inner()
        .ids;
    assert_eq!(found_ids, vec![allocation.id.unwrap()]);

    // Fetch allocation by known unique ID.
    // Expect one active record.
    let found_allocations = env
        .api
        .find_compute_allocations_by_ids(Request::new(
            rpc::forge::FindComputeAllocationsByIdsRequest {
                ids: vec![allocation.id.unwrap()],
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .allocations;
    assert_eq!(found_allocations.len(), 1);
    assert_eq!(found_allocations[0].id, allocation.id);

    let updated_name = format!("alloc-basic-updated-{}", Uuid::new_v4());
    // Update metadata on existing allocation.
    // Expect success for owned record.
    let updated = update_compute_allocation(&env, &allocation, 1, &updated_name)
        .await
        .unwrap()
        .into_inner()
        .allocation
        .unwrap();
    assert_eq!(updated.metadata.unwrap().name, updated_name);

    // Delete the allocation owned by tenant.
    // Expect success: record exists.
    env.api
        .delete_compute_allocation(
            DeleteComputeAllocationRequest::builder(TENANT_ORG)
                .id(updated.id.unwrap())
                .tonic_request(),
        )
        .await
        .unwrap();

    // Re-query IDs after delete.
    // Expect none: deleted rows are filtered.
    let post_delete_ids = env
        .api
        .find_compute_allocation_ids(Request::new(rpc::forge::FindComputeAllocationIdsRequest {
            name: Some(updated_name),
            tenant_organization_id: Some(TENANT_ORG.to_string()),
            instance_type_id: Some(instance_type_id),
        }))
        .await
        .unwrap()
        .into_inner()
        .ids;
    assert!(post_delete_ids.is_empty());

    Ok(())
}

async fn test_create_instance_no_allocations(
    pool: PgPool,
    enforcement: ComputeAllocationEnforcement,
    should_pass: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Build env with selected enforcement mode.
    // Expect success with valid test config.
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            ..Default::default()
        }
        .with_compute_allocation_enforcement(enforcement),
    )
    .await;

    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let host = create_managed_host(&env).await;
    // Bind host to this instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host.id.to_string()],
            },
        ))
        .await
        .unwrap();
    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Try allocation with no existing limits.
    // Expected pass/fail depends on mode.
    let result = allocate_instance(&env, &host, Some(instance_type_id.as_str()), segment_id).await;
    if should_pass {
        result.unwrap();
    } else {
        let err = result.unwrap_err();
        assert_eq!(err.code(), Code::FailedPrecondition);
    }

    Ok(())
}

#[sqlx_test]
async fn test_create_instance_no_allocations_warn_only(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_create_instance_no_allocations(pool, ComputeAllocationEnforcement::WarnOnly, true).await
}

#[sqlx_test]
async fn test_create_instance_no_allocations_enforce_if_present(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_create_instance_no_allocations(pool, ComputeAllocationEnforcement::EnforceIfPresent, true)
        .await
}

#[sqlx_test]
async fn test_create_instance_no_allocations_always(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_create_instance_no_allocations(pool, ComputeAllocationEnforcement::Always, false).await
}

async fn test_create_instance_without_instance_type_id_no_allocations(
    pool: PgPool,
    enforcement: ComputeAllocationEnforcement,
) -> Result<(), Box<dyn std::error::Error>> {
    // Build env with selected enforcement mode.
    // Expect success because omitted instance type IDs skip allocation enforcement.
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            ..Default::default()
        }
        .with_compute_allocation_enforcement(enforcement),
    )
    .await;

    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let host = create_managed_host(&env).await;
    // Bind host to this instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host.id.to_string()],
            },
        ))
        .await
        .unwrap();
    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Allocate without sending instance_type_id.
    // Expect success even in enforcing modes.
    let instance = allocate_instance(&env, &host, None, segment_id)
        .await
        .unwrap()
        .into_inner();

    // Verify the immediate response.
    // Expect no explicit instance type on the created instance.
    assert!(instance.instance_type_id.is_none());

    // Read the instance back from the API.
    // Expect no explicit instance type to be persisted.
    let persisted = env
        .api
        .find_instances_by_ids(Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![instance.id.unwrap()],
        }))
        .await
        .unwrap()
        .into_inner()
        .instances
        .pop()
        .unwrap();
    assert!(persisted.instance_type_id.is_none());

    Ok(())
}

#[sqlx_test]
async fn test_create_instance_no_allocations_without_instance_type_id_enforce_if_present(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_create_instance_without_instance_type_id_no_allocations(
        pool,
        ComputeAllocationEnforcement::EnforceIfPresent,
    )
    .await
}

#[sqlx_test]
async fn test_create_instance_no_allocations_without_instance_type_id_always(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_create_instance_without_instance_type_id_no_allocations(
        pool,
        ComputeAllocationEnforcement::Always,
    )
    .await
}

async fn test_create_instance_with_enough_allocations(
    pool: PgPool,
    enforcement: ComputeAllocationEnforcement,
) -> Result<(), Box<dyn std::error::Error>> {
    // Build env with selected enforcement mode.
    // Expect success with valid test config.
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            ..Default::default()
        }
        .with_compute_allocation_enforcement(enforcement),
    )
    .await;

    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let host = create_managed_host(&env).await;
    // Bind host to this instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host.id.to_string()],
            },
        ))
        .await
        .unwrap();
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let alloc_name = format!("alloc-enough-{}", Uuid::new_v4());
    // Seed one allocation for this tenant/type.
    // Expect success with valid tenant/type.
    let _allocation = create_compute_allocation(&env, &instance_type_id, 1, &alloc_name).await;

    // Allocate one instance against limit 1.
    // Expect success in all enforcement modes.
    allocate_instance(&env, &host, Some(instance_type_id.as_str()), segment_id)
        .await
        .unwrap();

    // Nothing in the suite had ever asked for allocation stats -- every test passed
    // `include_allocation_stats: false`, and the only caller that passes `true` is
    // production, over in `api-web/src/instance_type.rs`. So this is the first check on
    // the numbers the web UI actually reads back: one allocation of one, fully consumed.
    let instance_types = env
        .api
        .find_instance_types_by_ids(Request::new(rpc::forge::FindInstanceTypesByIdsRequest {
            instance_type_ids: vec![instance_type_id.to_string()],
            tenant_organization_id: Some(TENANT_ORG.to_string()),
            include_allocation_stats: true,
        }))
        .await
        .unwrap()
        .into_inner()
        .instance_types;

    assert_eq!(instance_types.len(), 1);
    let stats = instance_types[0]
        .allocation_stats
        .as_ref()
        .expect("allocation stats should come back when include_allocation_stats is set");
    assert_eq!(stats.max_allocatable, 1, "one allocation, sized one");
    assert_eq!(stats.used, 1, "the instance we just allocated");
    assert_eq!(stats.unused, 0, "which leaves nothing spare");
    assert_eq!(
        stats.unused_usable, 0,
        "and nothing spare on a healthy host either -- this one is computed separately \
         from `unused`, by filtering out unused machines in bad states"
    );

    Ok(())
}

#[sqlx_test]
async fn test_create_instance_enough_allocations_warn_only(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_create_instance_with_enough_allocations(pool, ComputeAllocationEnforcement::WarnOnly).await
}

#[sqlx_test]
async fn test_create_instance_enough_allocations_enforce_if_present(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_create_instance_with_enough_allocations(
        pool,
        ComputeAllocationEnforcement::EnforceIfPresent,
    )
    .await
}

#[sqlx_test]
async fn test_create_instance_enough_allocations_always(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_create_instance_with_enough_allocations(pool, ComputeAllocationEnforcement::Always).await
}

async fn test_create_instance_with_insufficient_allocations(
    pool: PgPool,
    enforcement: ComputeAllocationEnforcement,
    second_should_pass: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Build env with selected enforcement mode.
    // Expect success with valid test config.
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            ..Default::default()
        }
        .with_compute_allocation_enforcement(enforcement),
    )
    .await;

    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let host_1 = create_managed_host(&env).await;
    // Bind first host to the instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host_1.id.to_string()],
            },
        ))
        .await
        .unwrap();
    let host_2 = create_managed_host(&env).await;
    // Bind second host to the instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host_2.id.to_string()],
            },
        ))
        .await
        .unwrap();
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let alloc_name = format!("alloc-insufficient-{}", Uuid::new_v4());
    // Seed one allocation for this tenant/type.
    // Expect success with valid tenant/type.
    let _allocation = create_compute_allocation(&env, &instance_type_id, 1, &alloc_name).await;

    // First allocation consumes full limit.
    // Expect success.
    allocate_instance(&env, &host_1, Some(instance_type_id.as_str()), segment_id)
        .await
        .unwrap();

    // Second allocation exceeds limit=1.
    // Outcome depends on enforcement mode.
    let second =
        allocate_instance(&env, &host_2, Some(instance_type_id.as_str()), segment_id).await;
    if second_should_pass {
        second.unwrap();
    } else {
        let err = second.unwrap_err();
        assert_eq!(err.code(), Code::FailedPrecondition);
    }

    Ok(())
}

#[sqlx_test]
async fn test_create_instance_insufficient_allocations_warn_only(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_create_instance_with_insufficient_allocations(
        pool,
        ComputeAllocationEnforcement::WarnOnly,
        true,
    )
    .await
}

#[sqlx_test]
async fn test_create_instance_insufficient_allocations_enforce_if_present(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_create_instance_with_insufficient_allocations(
        pool,
        ComputeAllocationEnforcement::EnforceIfPresent,
        false,
    )
    .await
}

#[sqlx_test]
async fn test_create_instance_insufficient_allocations_always(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_create_instance_with_insufficient_allocations(
        pool,
        ComputeAllocationEnforcement::Always,
        false,
    )
    .await
}

async fn test_create_instance_without_instance_type_id_skips_insufficient_allocations(
    pool: PgPool,
    enforcement: ComputeAllocationEnforcement,
) -> Result<(), Box<dyn std::error::Error>> {
    // Build env with selected enforcement mode.
    // Expect omitted instance type IDs to bypass allocation enforcement.
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            ..Default::default()
        }
        .with_compute_allocation_enforcement(enforcement),
    )
    .await;

    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let host_1 = create_managed_host(&env).await;
    let host_2 = create_managed_host(&env).await;
    // Bind both hosts to the same instance type.
    // Expect success for fresh hosts.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host_1.id.to_string(), host_2.id.to_string()],
            },
        ))
        .await
        .unwrap();
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let alloc_name = format!("alloc-insufficient-omit-type-{}", Uuid::new_v4());
    // Seed one allocation for this tenant/type.
    // Expect success with valid tenant/type.
    let _allocation = create_compute_allocation(&env, &instance_type_id, 1, &alloc_name).await;

    // Consume the single allocation using an explicit instance type ID.
    // Expect success.
    allocate_instance(&env, &host_1, Some(instance_type_id.as_str()), segment_id)
        .await
        .unwrap();

    // Allocate without sending instance_type_id after the limit is exhausted.
    // Expect success because omitted instance type IDs skip enforcement.
    let instance = allocate_instance(&env, &host_2, None, segment_id)
        .await
        .unwrap()
        .into_inner();

    // Verify the immediate response.
    // Expect no explicit instance type on the created instance.
    assert!(instance.instance_type_id.is_none());

    // Read the instance back from the API.
    // Expect no explicit instance type to be persisted.
    let persisted = env
        .api
        .find_instances_by_ids(Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![instance.id.unwrap()],
        }))
        .await
        .unwrap()
        .into_inner()
        .instances
        .pop()
        .unwrap();
    assert!(persisted.instance_type_id.is_none());

    Ok(())
}

#[sqlx_test]
async fn test_create_instance_insufficient_allocations_without_instance_type_id_enforce_if_present(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_create_instance_without_instance_type_id_skips_insufficient_allocations(
        pool,
        ComputeAllocationEnforcement::EnforceIfPresent,
    )
    .await
}

#[sqlx_test]
async fn test_create_instance_insufficient_allocations_without_instance_type_id_always(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_create_instance_without_instance_type_id_skips_insufficient_allocations(
        pool,
        ComputeAllocationEnforcement::Always,
    )
    .await
}

#[sqlx_test]
async fn test_delete_allocation_when_instances_not_present_passes(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let host = create_managed_host(&env).await;
    // Bind host to this instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host.id.to_string()],
            },
        ))
        .await
        .unwrap();

    let alloc_name = format!("alloc-delete-no-instances-{}", Uuid::new_v4());
    // Seed allocation before delete scenario.
    // Expect success with valid tenant/type.
    let allocation = create_compute_allocation(&env, &instance_type_id, 1, &alloc_name).await;

    // Delete allocation with zero instances.
    // Expect success: no lower-bound conflict.
    env.api
        .delete_compute_allocation(
            DeleteComputeAllocationRequest::builder(TENANT_ORG)
                .id(allocation.id.unwrap())
                .tonic_request(),
        )
        .await
        .unwrap();

    // The delete returning `Ok` only says the call didn't error -- check the row is
    // actually gone, or this passes just as happily against a delete that does nothing.
    let remaining = env
        .api
        .find_compute_allocation_ids(Request::new(rpc::forge::FindComputeAllocationIdsRequest {
            name: Some(alloc_name),
            tenant_organization_id: Some(TENANT_ORG.to_string()),
            instance_type_id: Some(instance_type_id),
        }))
        .await
        .unwrap()
        .into_inner()
        .ids;
    assert!(remaining.is_empty(), "the allocation should be gone");

    Ok(())
}

#[sqlx_test]
async fn test_delete_allocation_when_instances_present_and_sufficient_remain_passes(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let host_1 = create_managed_host(&env).await;
    // Bind first host to the instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host_1.id.to_string()],
            },
        ))
        .await
        .unwrap();
    let host_2 = create_managed_host(&env).await;
    // Bind second host to the instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host_2.id.to_string()],
            },
        ))
        .await
        .unwrap();
    let segment_id = env.create_vpc_and_tenant_segment().await;

    // First allocation for delete-cap test.
    // Expect success with valid tenant/type.
    let alloc_1 = create_compute_allocation(
        &env,
        &instance_type_id,
        1,
        &format!("alloc-delete-enough-1-{}", Uuid::new_v4()),
    )
    .await;
    // Second allocation keeps remaining cap >= use.
    // Expect success with valid tenant/type.
    let alloc_2 = create_compute_allocation(
        &env,
        &instance_type_id,
        1,
        &format!("alloc-delete-enough-2-{}", Uuid::new_v4()),
    )
    .await;

    // Create one active instance before delete.
    // Expect success with cap=2.
    allocate_instance(&env, &host_1, Some(instance_type_id.as_str()), segment_id)
        .await
        .unwrap();

    // Delete one allocation with spare capacity.
    // Expect success: remaining cap is enough.
    env.api
        .delete_compute_allocation(
            DeleteComputeAllocationRequest::builder(TENANT_ORG)
                .id(alloc_1.id.unwrap())
                .tonic_request(),
        )
        .await
        .unwrap();

    // "Sufficient remain" is the claim in this test's name and nothing here was measuring
    // it -- the delete returning `Ok` says only that the call didn't error. So check the
    // row is actually gone, then prove the remaining capacity is genuinely one by deleting
    // the *second* allocation and watching it get refused: with an instance still live,
    // dropping to zero is exactly what the guard exists to stop.
    let remaining = env
        .api
        .find_compute_allocation_ids(Request::new(rpc::forge::FindComputeAllocationIdsRequest {
            name: None,
            tenant_organization_id: Some(TENANT_ORG.to_string()),
            instance_type_id: Some(instance_type_id.clone()),
        }))
        .await
        .unwrap()
        .into_inner()
        .ids;
    assert_eq!(remaining, vec![alloc_2.id.unwrap()]);

    let status = env
        .api
        .delete_compute_allocation(
            DeleteComputeAllocationRequest::builder(TENANT_ORG)
                .id(alloc_2.id.unwrap())
                .tonic_request(),
        )
        .await
        .expect_err("deleting the last allocation under a live instance should be refused");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);

    // And the refusal has to have left the row alone -- `FailedPrecondition` on its own
    // would still come back from a handler that deleted the allocation and only then
    // errored, which is the failure this guard exists to prevent.
    let after_refusal = env
        .api
        .find_compute_allocation_ids(Request::new(rpc::forge::FindComputeAllocationIdsRequest {
            name: None,
            tenant_organization_id: Some(TENANT_ORG.to_string()),
            instance_type_id: Some(instance_type_id),
        }))
        .await
        .unwrap()
        .into_inner()
        .ids;
    assert_eq!(after_refusal, vec![alloc_2.id.unwrap()]);

    Ok(())
}

#[sqlx_test]
async fn test_delete_allocation_when_instances_present_and_insufficient_remain_fails(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let host = create_managed_host(&env).await;
    // Bind host to this instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host.id.to_string()],
            },
        ))
        .await
        .unwrap();
    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Seed single allocation for fail case.
    // Expect success with valid tenant/type.
    let allocation = create_compute_allocation(
        &env,
        &instance_type_id,
        1,
        &format!("alloc-delete-insufficient-{}", Uuid::new_v4()),
    )
    .await;

    // Create one active instance before delete.
    // Expect success with cap=1.
    allocate_instance(&env, &host, Some(instance_type_id.as_str()), segment_id)
        .await
        .unwrap();

    let err = env
        .api
        // Delete only allocation under active instance.
        // Expect fail: would drop below usage.
        .delete_compute_allocation(
            DeleteComputeAllocationRequest::builder(TENANT_ORG)
                .id(allocation.id.unwrap())
                .tonic_request(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);

    Ok(())
}

#[sqlx_test]
async fn test_update_allocation_reduce_when_sufficient_remains_passes(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let host_1 = create_managed_host(&env).await;
    // Bind first host to the instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host_1.id.to_string()],
            },
        ))
        .await
        .unwrap();
    let host_2 = create_managed_host(&env).await;
    // Bind second host to the instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host_2.id.to_string()],
            },
        ))
        .await
        .unwrap();
    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Seed allocation count=2 for reduce test.
    // Expect success with valid tenant/type.
    let allocation = create_compute_allocation(
        &env,
        &instance_type_id,
        2,
        &format!("alloc-update-reduce-pass-{}", Uuid::new_v4()),
    )
    .await;

    // Create one active instance first.
    // Expect success with cap=2.
    allocate_instance(&env, &host_1, Some(instance_type_id.as_str()), segment_id)
        .await
        .unwrap();

    // Reduce count from 2 to 1.
    // Expect success: still >= active instances.
    let updated = update_compute_allocation(
        &env,
        &allocation,
        1,
        &format!("alloc-update-reduce-pass-updated-{}", Uuid::new_v4()),
    )
    .await
    .unwrap()
    .into_inner()
    .allocation
    .unwrap();

    assert_eq!(updated.attributes.unwrap().count, 1);

    Ok(())
}

#[sqlx_test]
async fn test_update_allocation_reduce_when_insufficient_remains_fails(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let host = create_managed_host(&env).await;
    // Bind host to this instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host.id.to_string()],
            },
        ))
        .await
        .unwrap();
    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Seed allocation count=1 for fail case.
    // Expect success with valid tenant/type.
    let allocation = create_compute_allocation(
        &env,
        &instance_type_id,
        1,
        &format!("alloc-update-reduce-fail-{}", Uuid::new_v4()),
    )
    .await;

    // Create one active instance first.
    // Expect success with cap=1.
    allocate_instance(&env, &host, Some(instance_type_id.as_str()), segment_id)
        .await
        .unwrap();

    // Reduce count from 1 to 0.
    // Expect fail: would drop below usage.
    let err = update_compute_allocation(
        &env,
        &allocation,
        0,
        &format!("alloc-update-reduce-fail-updated-{}", Uuid::new_v4()),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code(), Code::FailedPrecondition);

    Ok(())
}

#[sqlx_test]
async fn test_update_allocation_increase_when_sufficient_machines_remain_passes(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let host_1 = create_managed_host(&env).await;
    // Bind first host to the instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host_1.id.to_string()],
            },
        ))
        .await
        .unwrap();
    let host_2 = create_managed_host(&env).await;
    // Bind second host to the instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host_2.id.to_string()],
            },
        ))
        .await
        .unwrap();

    // Seed allocation count=1 for increase test.
    // Expect success with valid tenant/type.
    let allocation = create_compute_allocation(
        &env,
        &instance_type_id,
        1,
        &format!("alloc-update-increase-pass-{}", Uuid::new_v4()),
    )
    .await;

    // Increase count from 1 to 2.
    // Expect success: two machines are present.
    let updated = update_compute_allocation(
        &env,
        &allocation,
        2,
        &format!("alloc-update-increase-pass-updated-{}", Uuid::new_v4()),
    )
    .await
    .unwrap()
    .into_inner()
    .allocation
    .unwrap();

    assert_eq!(updated.attributes.unwrap().count, 2);

    Ok(())
}

#[sqlx_test]
async fn test_update_allocation_increase_when_insufficient_machines_remain_fails(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let host = create_managed_host(&env).await;
    // Bind host to this instance type.
    // Expect success for a fresh host.
    env.api
        .associate_machines_with_instance_type(Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host.id.to_string()],
            },
        ))
        .await
        .unwrap();

    // Seed allocation count=1 for fail case.
    // Expect success with valid tenant/type.
    let allocation = create_compute_allocation(
        &env,
        &instance_type_id,
        1,
        &format!("alloc-update-increase-fail-{}", Uuid::new_v4()),
    )
    .await;

    // Increase count from 1 to 2.
    // Expect fail: only one machine is present.
    let err = update_compute_allocation(
        &env,
        &allocation,
        2,
        &format!("alloc-update-increase-fail-updated-{}", Uuid::new_v4()),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code(), Code::FailedPrecondition);

    Ok(())
}

async fn test_remove_machine_association(
    pool: PgPool,
    enforcement: ComputeAllocationEnforcement,
    associated_machine_count: usize,
    allocation_count: Option<u32>,
    should_pass: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            ..Default::default()
        }
        .with_compute_allocation_enforcement(enforcement),
    )
    .await;

    let instance_type_id = get_instance_type_fixture_id(&env).await;
    // Create tenant for allocation FK checks.
    // Expect success in isolated test DB.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: TENANT_ORG.to_string(),
            routing_profile_type: None,
            metadata: Some(metadata("compute-allocation-test-tenant")),
        }))
        .await
        .unwrap();

    let mut hosts = Vec::new();
    for _ in 0..associated_machine_count {
        let host = create_managed_host(&env).await;
        // Bind host to this instance type.
        // Expect success for a fresh host.
        env.api
            .associate_machines_with_instance_type(Request::new(
                rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                    instance_type_id: instance_type_id.clone(),
                    machine_ids: vec![host.id.to_string()],
                },
            ))
            .await
            .unwrap();
        hosts.push(host);
    }

    if let Some(count) = allocation_count {
        // Seed allocation for removal checks.
        // Expect success with valid tenant/type.
        let _allocation = create_compute_allocation(
            &env,
            &instance_type_id,
            count,
            &format!("alloc-remove-assoc-{}", Uuid::new_v4()),
        )
        .await;
    }

    let host_to_remove = hosts.first().unwrap();
    // Try removing host association.
    // Result depends on enforcement mode.
    let result = env
        .api
        .remove_machine_instance_type_association(Request::new(
            rpc::forge::RemoveMachineInstanceTypeAssociationRequest {
                machine_id: host_to_remove.id.to_string(),
            },
        ))
        .await;

    // Both branches used to stop at the status code, which left the association itself
    // unchecked either way -- a handler that returned `Ok` without unbinding, or one that
    // errored *after* committing the unbind, would both have gone unnoticed.
    let bound_instance_type = env
        .api
        .find_machines_by_ids(Request::new(rpc::forge::MachinesByIdsRequest {
            machine_ids: vec![host_to_remove.id.into()],
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .machines
        .remove(0)
        .config
        .expect("machine should have a config")
        .instance_type_id;

    if should_pass {
        result.unwrap();
        assert!(
            bound_instance_type.is_none(),
            "the association should be gone once removal succeeds"
        );
    } else {
        let err = result.unwrap_err();
        assert_eq!(err.code(), Code::FailedPrecondition);
        assert!(
            bound_instance_type.is_some(),
            "a refused removal should leave the association in place"
        );
    }

    Ok(())
}

#[sqlx_test]
async fn test_remove_machine_association_no_allocations_warn_only(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_remove_machine_association(pool, ComputeAllocationEnforcement::WarnOnly, 1, None, true)
        .await
}

#[sqlx_test]
async fn test_remove_machine_association_no_allocations_enforce_if_present(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_remove_machine_association(
        pool,
        ComputeAllocationEnforcement::EnforceIfPresent,
        1,
        None,
        true,
    )
    .await
}

#[sqlx_test]
async fn test_remove_machine_association_no_allocations_always(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_remove_machine_association(pool, ComputeAllocationEnforcement::Always, 2, None, true).await
}

#[sqlx_test]
async fn test_remove_machine_association_allocations_less_than_remaining_warn_only(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_remove_machine_association(
        pool,
        ComputeAllocationEnforcement::WarnOnly,
        3,
        Some(1),
        true,
    )
    .await
}

#[sqlx_test]
async fn test_remove_machine_association_allocations_less_than_remaining_enforce_if_present(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_remove_machine_association(
        pool,
        ComputeAllocationEnforcement::EnforceIfPresent,
        3,
        Some(1),
        true,
    )
    .await
}

#[sqlx_test]
async fn test_remove_machine_association_allocations_less_than_remaining_always(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_remove_machine_association(pool, ComputeAllocationEnforcement::Always, 3, Some(1), true)
        .await
}

#[sqlx_test]
async fn test_remove_machine_association_allocations_greater_than_remaining_warn_only(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_remove_machine_association(
        pool,
        ComputeAllocationEnforcement::WarnOnly,
        2,
        Some(2),
        true,
    )
    .await
}

#[sqlx_test]
async fn test_remove_machine_association_allocations_greater_than_remaining_enforce_if_present(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_remove_machine_association(
        pool,
        ComputeAllocationEnforcement::EnforceIfPresent,
        2,
        Some(2),
        false,
    )
    .await
}

#[sqlx_test]
async fn test_remove_machine_association_allocations_greater_than_remaining_always(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_remove_machine_association(
        pool,
        ComputeAllocationEnforcement::Always,
        2,
        Some(2),
        false,
    )
    .await
}
