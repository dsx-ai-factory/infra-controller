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

//! Tests for batch instance allocation API

use ::rpc::forge::forge_server::Forge;
use carbide_uuid::machine::{MachineId, StableHostMachineId};
use carbide_uuid::network::NetworkSegmentId;
use common::api_fixtures::instance::{
    default_os_config, default_tenant_config, single_interface_network_config,
};
use common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_managed_host, create_test_env,
    create_test_env_with_overrides, get_instance_type_fixture_id, populate_network_security_groups,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::cfg::file::ComputeAllocationEnforcement;
use crate::tests::common;
use crate::tests::common::api_fixtures::TestManagedHost;

/// Allocate 3 instances in a single batch request.
/// Expect all 3 instances to be created with correct machine_id and network config.
#[crate::sqlx_test]
async fn test_batch_allocate_instances_success(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Create 3 managed hosts
    let mh1 = create_managed_host(&env).await;
    let mh2 = create_managed_host(&env).await;
    let mh3 = create_managed_host(&env).await;

    // Build batch allocation request
    let batch_request = rpc::forge::BatchInstanceAllocationRequest {
        instance_requests: vec![
            build_test_instance_allocation_request(&env, &mh1, segment_id),
            build_test_instance_allocation_request(&env, &mh2, segment_id),
            build_test_instance_allocation_request(&env, &mh3, segment_id),
        ],
    };

    // Call batch API
    let response = env
        .api
        .allocate_instances(tonic::Request::new(batch_request))
        .await
        .unwrap()
        .into_inner();

    // Verify response
    assert_eq!(response.instances.len(), 3);

    // Verify all instances are in the database
    let mut txn = env.db_txn().await;
    for instance in &response.instances {
        let machine_id = *instance.machine_id.as_ref().unwrap();
        let snapshot = db::managed_host::load_snapshot(
            txn.as_mut(),
            &machine_id,
            model::machine::LoadSnapshotOptions::default(),
        )
        .await
        .unwrap();

        assert!(snapshot.is_some());
        let snapshot = snapshot.unwrap();
        assert!(snapshot.instance.is_some());

        let instance_snapshot = snapshot.instance.unwrap();
        assert_eq!(instance_snapshot.machine_id, machine_id.into());
        assert!(!instance_snapshot.config.network.interfaces.is_empty());
    }
}

/// Include an invalid machine ID in a batch of 3 requests.
/// Expect the entire batch to fail and all allocations to be rolled back.
#[crate::sqlx_test]
async fn test_batch_allocate_instances_rollback_on_failure(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let mh1 = create_managed_host(&env).await;
    let mh2 = create_managed_host(&env).await;

    // Create an invalid machine ID that doesn't exist
    #[allow(deprecated)]
    let invalid_machine_id: StableHostMachineId = MachineId::default()
        .try_into()
        .expect("default test ID should identify a stable host");

    let batch_request = rpc::forge::BatchInstanceAllocationRequest {
        instance_requests: vec![
            build_test_instance_allocation_request(&env, &mh1, segment_id),
            // Invalid request - machine doesn't exist
            rpc::forge::InstanceAllocationRequest {
                machine_id: Some(invalid_machine_id),
                config: Some(rpc::forge::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    network: Some(single_interface_network_config(segment_id)),
                    infiniband: None,
                    network_security_group_id: None,
                    dpu_extension_services: None,
                    nvlink: None,
                    spxconfig: None,
                    power_profile: None,
                }),
                instance_id: None,
                instance_type_id: None,
                metadata: Some(rpc::forge::Metadata {
                    name: "test-instance-invalid".to_string(),
                    description: "".to_string(),
                    labels: vec![],
                }),
                allow_unhealthy_machine: false,
            },
            build_test_instance_allocation_request(&env, &mh2, segment_id),
        ],
    };

    // Call should fail
    let result = env
        .api
        .allocate_instances(tonic::Request::new(batch_request))
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.message().contains("Machine") || err.message().contains("not found"),
        "Expected error about machine not found, got: {}",
        err.message()
    );

    // Verify that the first instance was NOT created (transaction rolled back)
    let mut txn = env.db_txn().await;
    let snapshot1 = db::managed_host::load_snapshot(
        txn.as_mut(),
        &mh1.host().id,
        model::machine::LoadSnapshotOptions::default(),
    )
    .await
    .unwrap()
    .unwrap();

    assert!(
        snapshot1.instance.is_none(),
        "Instance should not exist - transaction should have rolled back"
    );

    // Verify that the third instance was also NOT created
    let snapshot2 = db::managed_host::load_snapshot(
        txn.as_mut(),
        &mh2.host().id,
        model::machine::LoadSnapshotOptions::default(),
    )
    .await
    .unwrap()
    .unwrap();

    assert!(
        snapshot2.instance.is_none(),
        "Instance should not exist - transaction should have rolled back"
    );
}

/// Allocate 2 instances sharing the same NSG in one batch.
/// Expect both instances to be created successfully with the shared NSG.
#[crate::sqlx_test]
async fn test_batch_allocate_instances_with_same_nsg(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Populate network security groups
    populate_network_security_groups(env.api.clone()).await;

    let mh1 = create_managed_host(&env).await;
    let mh2 = create_managed_host(&env).await;

    // Get an NSG ID that was created by populate_network_security_groups
    let nsg_id = "fd3ab096-d811-11ef-8fe9-7be4b2483448".to_string();

    // Build requests with the same NSG
    let mut req1 = build_test_instance_allocation_request(&env, &mh1, segment_id);
    req1.config.as_mut().unwrap().network_security_group_id = Some(nsg_id.clone());

    let mut req2 = build_test_instance_allocation_request(&env, &mh2, segment_id);
    req2.config.as_mut().unwrap().network_security_group_id = Some(nsg_id);

    let batch_request = rpc::forge::BatchInstanceAllocationRequest {
        instance_requests: vec![req1, req2],
    };

    // Call batch API - should succeed with shared NSG validation
    let response = env
        .api
        .allocate_instances(tonic::Request::new(batch_request))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(response.instances.len(), 2);
}

/// Allocate a batch where every request omits instance_type_id.
/// Expect the batch to succeed even when enforcement is set to Always.
#[crate::sqlx_test]
async fn test_batch_allocate_instances_without_instance_type_id_skips_allocation_enforcement(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::default()
            .with_compute_allocation_enforcement(ComputeAllocationEnforcement::Always),
    )
    .await;

    let instance_type_id = get_instance_type_fixture_id(&env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh1 = create_managed_host(&env).await;
    let mh2 = create_managed_host(&env).await;

    // Bind both hosts to the same instance type.
    // Expect success for fresh hosts.
    env.api
        .associate_machines_with_instance_type(tonic::Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![mh1.host().id.to_string(), mh2.host().id.to_string()],
            },
        ))
        .await
        .unwrap();

    // Build requests that omit instance_type_id.
    // Expect both requests to bypass allocation enforcement.
    let response = env
        .api
        .allocate_instances(tonic::Request::new(
            rpc::forge::BatchInstanceAllocationRequest {
                instance_requests: vec![
                    build_test_instance_allocation_request(&env, &mh1, segment_id),
                    build_test_instance_allocation_request(&env, &mh2, segment_id),
                ],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    // Verify the immediate response.
    // Expect no explicit instance type on either returned instance.
    assert_eq!(response.instances.len(), 2);
    assert!(
        response
            .instances
            .iter()
            .all(|instance| instance.instance_type_id.is_none())
    );

    let instance_ids = response
        .instances
        .iter()
        .map(|instance| instance.id.unwrap())
        .collect::<Vec<_>>();

    // Read the instances back from the API.
    // Expect no explicit instance types to be persisted.
    let persisted = env
        .api
        .find_instances_by_ids(tonic::Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids,
        }))
        .await
        .unwrap()
        .into_inner()
        .instances;

    assert_eq!(persisted.len(), 2);
    assert!(
        persisted
            .iter()
            .all(|instance| instance.instance_type_id.is_none())
    );
}

/// Send a mixed batch where one request sends instance_type_id and one omits it.
/// Expect the typed request to enforce limits and roll back the entire batch.
#[crate::sqlx_test]
async fn test_batch_allocate_instances_mixed_instance_type_id_rolls_back_on_enforced_request(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::default()
            .with_compute_allocation_enforcement(ComputeAllocationEnforcement::Always),
    )
    .await;

    let instance_type_id = get_instance_type_fixture_id(&env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh1 = create_managed_host(&env).await;
    let mh2 = create_managed_host(&env).await;

    // Bind both hosts to the same instance type.
    // Expect success for fresh hosts.
    env.api
        .associate_machines_with_instance_type(tonic::Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![mh1.host().id.to_string(), mh2.host().id.to_string()],
            },
        ))
        .await
        .unwrap();

    // Build a mixed batch with one enforced request and one omitted instance_type_id.
    // Expect the typed request to fail under Always with no allocations configured.
    let mut req1 = build_test_instance_allocation_request(&env, &mh1, segment_id);
    req1.instance_type_id = Some(instance_type_id);

    let req2 = build_test_instance_allocation_request(&env, &mh2, segment_id);

    let err = env
        .api
        .allocate_instances(tonic::Request::new(
            rpc::forge::BatchInstanceAllocationRequest {
                instance_requests: vec![req1, req2],
            },
        ))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::FailedPrecondition);

    // Verify the failed batch rolled back both instance creations.
    // Expect no persisted instances on either host.
    let mut txn = env.db_txn().await;
    for host_id in [mh1.host().id, mh2.host().id] {
        let snapshot = db::managed_host::load_snapshot(
            txn.as_mut(),
            &host_id,
            model::machine::LoadSnapshotOptions::default(),
        )
        .await
        .unwrap()
        .unwrap();

        assert!(snapshot.instance.is_none());
    }
}

// Helper function to build a test instance allocation request
fn build_test_instance_allocation_request(
    _env: &TestEnv,
    mh: &TestManagedHost,
    segment_id: NetworkSegmentId,
) -> rpc::forge::InstanceAllocationRequest {
    rpc::forge::InstanceAllocationRequest {
        machine_id: Some(mh.host().id),
        config: Some(rpc::forge::InstanceConfig {
            tenant: Some(default_tenant_config()),
            os: Some(default_os_config()),
            network: Some(single_interface_network_config(segment_id)),
            infiniband: None,
            network_security_group_id: None,
            dpu_extension_services: None,
            nvlink: None,
            spxconfig: None,
            power_profile: None,
        }),
        instance_id: None,
        instance_type_id: None,
        metadata: Some(rpc::forge::Metadata {
            name: format!("test-instance-{}", uuid::Uuid::new_v4()),
            description: "Test instance for batch allocation".to_string(),
            labels: vec![],
        }),
        allow_unhealthy_machine: false,
    }
}
