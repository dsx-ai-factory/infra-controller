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

use std::time::Duration;

use carbide_api_core::test_support::default_config;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use itertools::Itertools;
use model::expected_machine::{ExpectedInterface, ExpectedMachineData};
use model::instance::config::network::InterfaceFunctionId;
use model::machine::{InstanceState, ManagedHostState};
use model::test_support::ManagedHostConfig;
use rpc::forge::{InstanceAllocationRequest, InstanceConfig};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tonic::Request;

use super::instance_common::{
    InstanceEnv, default_os_config, default_tenant_config, host_id,
    single_interface_network_config, single_interface_network_config_with_vfs,
};

/// Verifies sparse DPF topology membership gates both allocation and network replacement.
#[sqlx_test]
async fn test_dpf_topology_rejects_unselected_vf_on_create_and_update(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let config = carbide_api_core::test_support::default_config::with_dpf_intercept_topology(&[7]);
    let env = InstanceEnv::new(pool, config).await;
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;
    let mut txn = env.harness.api().database_connection.begin().await.unwrap();
    db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &host_id(&mh))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let segment_ids = env.tenant_segments(2).await;

    // VF0 is structurally valid and globally enabled, but the replacement topology selects VF7.
    let mut network_with_unselected_vf =
        single_interface_network_config_with_vfs(segment_ids.clone());
    network_with_unselected_vf.interfaces[1].virtual_function_id = Some(0);
    let create_error = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host_id(&mh))
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(network_with_unselected_vf.clone()),
                )
                .tonic_request(),
        )
        .await
        .expect_err("an unselected topology VF must not be allocated");
    assert!(create_error.message().contains(
        "virtual function VF0 is not selected by the configured DPF intercept-bridging topology"
    ));

    // A PF-only instance remains valid; once ready, adding the same unselected VF must fail at
    // the network-update gate and leave no staged replacement request.
    let instance = mh
        .instance_builder(&env.harness)
        .single_interface_network_config(segment_ids[0])
        .build()
        .await;
    env.advance_instance_to_ready(&mh, &instance).await;
    let update_error = env
        .harness
        .api()
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: instance.rpc_instance().await.id,
                if_version_match: None,
                config: Some(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    network: Some(network_with_unselected_vf),
                    infiniband: None,
                    network_security_group_id: None,
                    dhcp_option_set_id: None,
                    dpu_extension_services: None,
                    nvlink: None,
                    spxconfig: None,
                    power_profile: None,
                }),
                metadata: Some(rpc::forge::Metadata {
                    name: "topology-update".to_string(),
                    description: String::new(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .expect_err("an unselected topology VF must not be staged");
    assert!(update_error.message().contains(
        "virtual function VF0 is not selected by the configured DPF intercept-bridging topology"
    ));

    let mut txn = env.harness.db_txn().await;
    assert!(
        instance
            .db_instance(&mut txn)
            .await
            .update_network_config_request
            .is_none()
    );
    txn.rollback().await.unwrap();
}

/// Verifies a non-DPF host uses its configured instance VF inventory even at a DPF-enabled site.
#[sqlx_test]
async fn test_instance_vf_inventory_rejects_unavailable_vf_on_create_and_update(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    // The site topology selects VF14, while the non-DPF HBN inventory exposes only VF0-VF13.
    // Per-host DPF state must choose the HBN inventory for this host.
    let mut config =
        carbide_api_core::test_support::default_config::with_dpf_intercept_topology(&[14]);
    config.dpu_config.num_of_vfs = 16;
    config
        .vmaas_config
        .as_mut()
        .expect("the DPF topology config includes VMaaS")
        .hbn_reps = Some("pf0hpf,pf0vf0-pf0vf13,pf1hpf".to_string());
    let env = InstanceEnv::new(pool, config).await;
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;
    let segment_ids = env.tenant_segments(2).await;

    // VF14 is supported by the hardware population but absent from the configured inventory.
    let mut network_with_unavailable_vf =
        single_interface_network_config_with_vfs(segment_ids.clone());
    network_with_unavailable_vf.interfaces[1].virtual_function_id = Some(14);
    let create_error = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host_id(&mh))
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(network_with_unavailable_vf.clone()),
                )
                .tonic_request(),
        )
        .await
        .expect_err("an unavailable VF must not be allocated");
    assert!(create_error.message().contains(
        "virtual function VF14 is not available in the configured instance VF inventory"
    ));

    // A PF-only instance remains valid; adding VF14 must fail without staging the replacement.
    let instance = mh
        .instance_builder(&env.harness)
        .single_interface_network_config(segment_ids[0])
        .build()
        .await;
    env.advance_instance_to_ready(&mh, &instance).await;
    let update_error = env
        .harness
        .api()
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: instance.rpc_instance().await.id,
                if_version_match: None,
                config: Some(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    network: Some(network_with_unavailable_vf),
                    infiniband: None,
                    network_security_group_id: None,
                    dhcp_option_set_id: None,
                    dpu_extension_services: None,
                    nvlink: None,
                    spxconfig: None,
                    power_profile: None,
                }),
                metadata: Some(rpc::forge::Metadata {
                    name: "unavailable-vf-update".to_string(),
                    description: String::new(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .expect_err("an unavailable VF must not be staged");
    assert!(update_error.message().contains(
        "virtual function VF14 is not available in the configured instance VF inventory"
    ));

    let mut txn = env.harness.db_txn().await;
    assert!(
        instance
            .db_instance(&mut txn)
            .await
            .update_network_config_request
            .is_none()
    );
    txn.rollback().await.unwrap();
}

/// Verifies older callers that omit VF IDs use sparse HBN inventory on create and update.
#[sqlx_test]
async fn test_implicit_instance_vfs_follow_sparse_hbn_inventory_on_create_and_update(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config = default_config::get();
    config.dpu_config.num_of_vfs = 16;
    config
        .vmaas_config
        .as_mut()
        .expect("the default test configuration includes VMaaS")
        .hbn_reps = Some("pf0hpf,pf0vf2,pf0vf5,pf1hpf".to_string());
    let env = InstanceEnv::new(pool, config).await;
    let create_mh = env.create_managed_host(ManagedHostConfig::default()).await;
    let update_mh = env.create_managed_host(ManagedHostConfig::default()).await;
    let segment_ids = env.tenant_segments(3).await;

    let implicit_network = || {
        let mut network = single_interface_network_config_with_vfs(segment_ids.clone());
        for interface in &mut network.interfaces {
            if interface.function_type() == rpc::InterfaceFunctionType::Virtual {
                interface.virtual_function_id = None;
            }
        }
        network
    };

    let created_instance = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host_id(&create_mh))
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(implicit_network()),
                )
                .tonic_request(),
        )
        .await
        .expect("implicit VFs from a sparse inventory must be allocated")
        .into_inner();
    let created_vfs = created_instance
        .config
        .expect("the allocated instance includes its config")
        .network
        .expect("the allocated instance includes its network config")
        .interfaces
        .into_iter()
        .filter(|interface| interface.function_type() == rpc::InterfaceFunctionType::Virtual)
        .map(|interface| {
            interface
                .virtual_function_id
                .expect("NICo resolves every implicit VF ID")
        })
        .collect_vec();
    assert_eq!(created_vfs, vec![2, 5]);

    let update_instance = update_mh
        .instance_builder(&env.harness)
        .single_interface_network_config(segment_ids[0])
        .build()
        .await;
    env.advance_instance_to_ready(&update_mh, &update_instance)
        .await;
    env.harness
        .api()
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: update_instance.rpc_instance().await.id,
            if_version_match: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(implicit_network()),
                infiniband: None,
                network_security_group_id: None,
                dhcp_option_set_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            metadata: Some(rpc::forge::Metadata {
                name: "implicit-vf-update".to_string(),
                description: String::new(),
                labels: vec![],
            }),
        }))
        .await
        .expect("an update with implicit VFs must use the sparse inventory");

    let mut txn = env.harness.db_txn().await;
    let staged_update = update_instance
        .db_instance(&mut txn)
        .await
        .update_network_config_request
        .expect("the replacement network config must be staged");
    let staged_vfs = staged_update
        .new_config
        .interfaces
        .iter()
        .filter_map(|interface| match &interface.function_id {
            InterfaceFunctionId::Physical {} => None,
            InterfaceFunctionId::Virtual { id } => Some(*id),
        })
        .collect_vec();
    assert_eq!(staged_vfs, vec![2, 5]);
    txn.rollback().await.unwrap();
}

/// Verifies an unchanged sparse-VF network with omitted wire IDs remains a network no-op,
/// because legacy callers must not acquire expansion-only locks for metadata updates.
#[sqlx_test]
async fn test_implicit_sparse_vf_noop_update_bypasses_overlap_lock(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    // Configure a sparse inventory so converter placeholders differ from the assigned VF IDs.
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config = default_config::get();
    config.dpu_config.num_of_vfs = 16;
    config
        .vmaas_config
        .as_mut()
        .expect("the default test configuration includes VMaaS")
        .hbn_reps = Some("pf0hpf,pf0vf2,pf0vf5,pf1hpf".to_string());
    let env = InstanceEnv::new(pool, config).await;
    let host = env.create_managed_host(ManagedHostConfig::default()).await;
    let segment_ids = env.tenant_segments(3).await;
    let implicit_network = || {
        let mut network = single_interface_network_config_with_vfs(segment_ids.clone());
        for interface in &mut network.interfaces {
            if interface.function_type() == rpc::InterfaceFunctionType::Virtual {
                interface.virtual_function_id = None;
            }
        }
        network
    };

    // Allocate the active instance through the legacy wire shape so it stores sparse VFs 2 and 5.
    let instance = host
        .instance_builder(&env.harness)
        .config(
            InstanceConfig::builder()
                .tenant(default_tenant_config())
                .os(default_os_config())
                .network(implicit_network())
                .into(),
        )
        .build()
        .await;
    env.advance_instance_to_ready(&host, &instance).await;

    // Re-submit the full network without VF IDs while changing only metadata.
    let before = instance.rpc_instance().await;
    let mut update_config = before
        .config
        .clone()
        .expect("the instance includes its config");
    for interface in &mut update_config
        .network
        .as_mut()
        .expect("the instance includes its network config")
        .interfaces
    {
        if interface.function_type() == rpc::InterfaceFunctionType::Virtual {
            interface.virtual_function_id = None;
        }
    }
    let mut updated_metadata = before
        .metadata
        .clone()
        .expect("the instance includes its metadata");
    updated_metadata.description = "metadata-only sparse VF update".to_string();

    // Hold the expansion lock; the unchanged resolved network must complete without waiting on it.
    let mut blocker = env.harness.db_txn().await;
    db::tenant_prefix_overlap::lock_checks(&mut blocker)
        .await
        .unwrap();
    let updated = tokio::time::timeout(
        Duration::from_secs(10),
        env.harness.api().update_instance_config(Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: before.id,
                if_version_match: None,
                config: Some(update_config),
                metadata: Some(updated_metadata.clone()),
            },
        )),
    )
    .await
    .expect("an unchanged sparse-VF update must bypass the overlap lock")
    .expect("an unchanged sparse-VF update must succeed")
    .into_inner();
    blocker.rollback().await.unwrap();

    // Confirm only metadata changed and no network replacement was staged.
    assert_eq!(updated.metadata.as_ref(), Some(&updated_metadata));
    let persisted = db::instance::find_by_id(&env.harness.api().database_connection, instance.id)
        .await
        .unwrap()
        .expect("the instance remains persisted");
    assert_eq!(
        persisted.network_config_version.to_string(),
        before.network_config_version
    );
    assert!(persisted.update_network_config_request.is_none());
    assert_eq!(
        persisted
            .config
            .network
            .interfaces
            .iter()
            .filter_map(|interface| match &interface.function_id {
                InterfaceFunctionId::Physical {} => None,
                InterfaceFunctionId::Virtual { id } => Some(*id),
            })
            .collect_vec(),
        vec![2, 5]
    );
}

/// Verifies topology-free BF4+CX9 hosts reject VF14 through public create and update APIs.
/// Generic DPF accepts this structurally valid VF without topology, so this is an Astra-specific
/// discriminator that protects both handlers' independent inventory lookups.
#[sqlx_test]
async fn test_bf4_cx9_topology_free_public_instance_paths_use_astra_static_inventory(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    // Enable managed-host admission and keep VF14 structurally valid, but remove configured
    // topology so Astra's fixed static inventory is authoritative.
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config = carbide_api_core::test_support::default_config::get();
    config.dpf.enabled = true;
    config.dpu_config.num_of_vfs = 16;
    config.vmaas_config = None;
    let env = InstanceEnv::new(pool, config).await;
    let create_mh = create_bf4_astra_host(&env, 3).await;
    let update_mh = create_bf4_astra_host(&env, 4).await;
    let segment_ids = env.tenant_segments(2).await;

    // Override only the VF ID: VF14 is valid for generic DPF but absent from Astra's static inventory.
    let vf14_network = || {
        let mut network = single_interface_network_config_with_vfs(segment_ids.clone());
        network.interfaces[1].virtual_function_id = Some(14);
        network
    };

    // The create path must select Astra and reject VF14 from its static VF0 through VF13 inventory.
    let create_error = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host_id(&create_mh))
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(vf14_network()),
                )
                .tonic_request(),
        )
        .await
        .expect_err("topology-free BF4 Astra creation must reject VF14");

    // Establish an assigned Ready instance so update exercises its independent host lookup.
    let update_instance = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host_id(&update_mh))
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(single_interface_network_config(segment_ids[0])),
                )
                .tonic_request(),
        )
        .await
        .expect("the update fixture instance must be allocated")
        .into_inner();
    let update_instance_id = update_instance
        .id
        .expect("the allocated update fixture includes its ID");
    // Network updates require an assigned Ready host; DPF convergence is outside this contract.
    let mut txn = env.harness.db_txn().await;
    db::machine::update_state(
        txn.as_mut(),
        &host_id(&update_mh),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // The update path must make the same Astra classification and reject the same VF.
    let update_error = env
        .harness
        .api()
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(update_instance_id),
            if_version_match: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(vf14_network()),
                infiniband: None,
                network_security_group_id: None,
                dhcp_option_set_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            metadata: Some(rpc::forge::Metadata {
                name: "bf4-astra-static-vf-update".to_string(),
                description: String::new(),
                labels: vec![],
            }),
        }))
        .await
        .expect_err("topology-free BF4 Astra update must reject VF14");

    // The Astra-specific diagnostic proves neither handler silently took generic DPF behavior.
    for error in [create_error, update_error] {
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(
            error
                .message()
                .contains("not available in the BF4 Astra static VF inventory")
        );
    }
}

/// Verifies BF4 Astra carries one sparse intercept-topology VF through public create and update.
/// This protects topology wiring; the topology-free sibling above supplies the class discriminator.
#[sqlx_test]
async fn test_bf4_astra_implicit_instance_vfs_use_intercept_topology_on_create_and_update(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    // Configure only VF14 so sequential allocation would expose admission/provisioning drift;
    // the remaining host and segment fixtures provide independent create and update baselines.
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config =
        carbide_api_core::test_support::default_config::with_dpf_intercept_topology(&[14]);
    config.dpu_config.num_of_vfs = 16;
    let env = InstanceEnv::new(pool, config).await;
    let create_mh = create_bf4_astra_host(&env, 1).await;
    let update_mh = create_bf4_astra_host(&env, 2).await;
    let segment_ids = env.tenant_segments(2).await;

    // Clear the otherwise-complete fixture's VF identity so create and update must allocate from
    // the configured sparse topology while retaining the remaining network setup.
    let implicit_network = || {
        let mut network = single_interface_network_config_with_vfs(segment_ids.clone());
        network.interfaces[1].virtual_function_id = None;
        network
    };

    // Create an instance through the public API with an implicit VF identity.
    let created_instance = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host_id(&create_mh))
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(implicit_network()),
                )
                .tonic_request(),
        )
        .await
        .expect("BF4 Astra must allocate an implicit VF from its intercept topology")
        .into_inner();

    // The response must expose the sole VF selected by the replacement inventory.
    let created_instance_id = created_instance
        .id
        .expect("the allocated instance includes its ID");
    let created_network = created_instance
        .config
        .as_ref()
        .expect("the allocated instance includes its config")
        .network
        .as_ref()
        .expect("the allocated instance includes its network config")
        .clone();
    let created_vfs = created_network
        .interfaces
        .iter()
        .filter(|interface| interface.function_type() == rpc::InterfaceFunctionType::Virtual)
        .map(|interface| {
            interface
                .virtual_function_id
                .expect("NICo resolves every implicit VF ID")
        })
        .collect_vec();
    assert_eq!(created_vfs, vec![14]);

    // Re-read through the public API to prove the resolved topology identity was persisted.
    let persisted_instances = env
        .harness
        .api()
        .find_instances_by_ids(Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![created_instance_id],
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(persisted_instances.instances.len(), 1);
    assert_eq!(
        persisted_instances.instances[0]
            .config
            .as_ref()
            .and_then(|config| config.network.as_ref()),
        Some(&created_network)
    );

    // Allocate directly because this test needs only an existing instance as the update baseline.
    let update_instance = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host_id(&update_mh))
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(single_interface_network_config(segment_ids[0])),
                )
                .tonic_request(),
        )
        .await
        .expect("the update fixture instance must be allocated")
        .into_inner();
    let update_instance_id = update_instance
        .id
        .expect("the allocated update fixture includes its ID");
    // Network updates require an assigned Ready host; DPF convergence is outside this contract.
    let mut txn = env.harness.db_txn().await;
    db::machine::update_state(
        txn.as_mut(),
        &host_id(&update_mh),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // Update through the public API with another implicit VF identity.
    env.harness
        .api()
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(update_instance_id),
            if_version_match: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(implicit_network()),
                infiniband: None,
                network_security_group_id: None,
                dhcp_option_set_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            metadata: Some(rpc::forge::Metadata {
                name: "bf4-astra-implicit-vf-update".to_string(),
                description: String::new(),
                labels: vec![],
            }),
        }))
        .await
        .expect("a BF4 Astra update must use the intercept topology");

    // Reload the staged replacement and verify persistence uses the same sparse VF identity.
    let mut txn = env.harness.db_txn().await;
    let staged_update = db::instance::find_by_id(txn.as_mut(), update_instance_id)
        .await
        .unwrap()
        .expect("the update fixture instance remains persisted")
        .update_network_config_request
        .expect("the replacement network config must be staged");
    let staged_vfs = staged_update
        .new_config
        .interfaces
        .iter()
        .filter_map(|interface| match &interface.function_id {
            InterfaceFunctionId::Physical {} => None,
            InterfaceFunctionId::Virtual { id } => Some(*id),
        })
        .collect_vec();
    assert_eq!(staged_vfs, vec![14]);
    txn.rollback().await.unwrap();
}

/// Verifies raw protobuf VF identities cannot alias selected topology VFs during conversion.
#[sqlx_test]
async fn test_public_instance_endpoints_reject_out_of_range_wire_vfs(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    // Include the truncation targets for 256 (VF0) and 263 (VF7). Without wire-range validation,
    // both malformed values would pass exact topology membership after a lossy u32-to-u8 cast.
    let config =
        carbide_api_core::test_support::default_config::with_dpf_intercept_topology(&[0, 7, 15]);
    let env = InstanceEnv::new(pool, config).await;
    let create_mh = env.create_managed_host(ManagedHostConfig::default()).await;
    let update_mh = env.create_managed_host(ManagedHostConfig::default()).await;
    let mut txn = env.harness.api().database_connection.begin().await.unwrap();
    for mh in [&create_mh, &update_mh] {
        db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &host_id(mh))
            .await
            .unwrap();
    }
    txn.commit().await.unwrap();
    let segment_ids = env.tenant_segments(2).await;
    let update_instance = update_mh
        .instance_builder(&env.harness)
        .single_interface_network_config(segment_ids[0])
        .build()
        .await;
    env.advance_instance_to_ready(&update_mh, &update_instance)
        .await;
    let update_instance_id = update_instance.rpc_instance().await.id;

    // Build the protobuf directly so values wider than the model's u8 cannot be normalized by
    // test helpers before they reach the public handlers.
    let config_with_wire_vf = |wire_vf_id| {
        let mut network = single_interface_network_config_with_vfs(segment_ids.clone());
        network.interfaces[1].virtual_function_id = Some(wire_vf_id);
        rpc::InstanceConfig {
            tenant: Some(default_tenant_config()),
            os: Some(default_os_config()),
            network: Some(network),
            infiniband: None,
            network_security_group_id: None,
            dhcp_option_set_id: None,
            dpu_extension_services: None,
            nvlink: None,
            spxconfig: None,
            power_profile: None,
        }
    };
    let metadata = |operation: &str, wire_vf_id| rpc::forge::Metadata {
        name: format!("{operation}-vf{wire_vf_id}"),
        description: String::new(),
        labels: vec![],
    };

    for wire_vf_id in [16, 256, 263] {
        let create_error = env
            .harness
            .api()
            .allocate_instance(Request::new(rpc::forge::InstanceAllocationRequest {
                machine_id: Some(host_id(&create_mh)),
                config: Some(config_with_wire_vf(wire_vf_id)),
                instance_id: None,
                instance_type_id: None,
                metadata: Some(metadata("create", wire_vf_id)),
                allow_unhealthy_machine: false,
                external_validation_request_id: None,
            }))
            .await
            .expect_err("out-of-range wire VF must be rejected during instance creation");
        assert_eq!(create_error.code(), tonic::Code::InvalidArgument);
        assert!(
            create_error
                .message()
                .contains(&format!("virtual function ID of value {wire_vf_id}"))
        );

        let update_error = env
            .harness
            .api()
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: update_instance_id,
                if_version_match: None,
                config: Some(config_with_wire_vf(wire_vf_id)),
                metadata: Some(metadata("update", wire_vf_id)),
            }))
            .await
            .expect_err("out-of-range wire VF must be rejected during instance update");
        assert_eq!(update_error.code(), tonic::Code::InvalidArgument);
        assert!(
            update_error
                .message()
                .contains(&format!("virtual function ID of value {wire_vf_id}"))
        );
    }

    // VF15 is the highest valid value and must pass the same public create and update paths.
    env.harness
        .api()
        .allocate_instance(Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(host_id(&create_mh)),
            config: Some(config_with_wire_vf(15)),
            instance_id: None,
            instance_type_id: None,
            metadata: Some(metadata("create", 15)),
            allow_unhealthy_machine: false,
            external_validation_request_id: None,
        }))
        .await
        .expect("VF15 must remain valid during instance creation");
    env.harness
        .api()
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: update_instance_id,
            if_version_match: None,
            config: Some(config_with_wire_vf(15)),
            metadata: Some(metadata("update", 15)),
        }))
        .await
        .expect("VF15 must remain valid during instance update");
}

/// Builds a BF4/CX9 host through ordinary ingestion without creating Astra DPA rows.
async fn create_bf4_astra_host(env: &InstanceEnv, cx9_mac_suffix: u8) -> TestManagedHost {
    let config = ManagedHostConfig::default();
    let host = env
        .create_managed_host(ManagedHostConfig {
            expected_machine_data: Some(ExpectedMachineData {
                serial_number: config.serial.clone(),
                dpf_enabled: Some(false),
                ..Default::default()
            }),
            ..config
        })
        .await;
    let mut txn = env.harness.db_txn().await;

    // Mark the attached DPU as BF4, which combines with the CX9 declaration to select Astra.
    let dpu = host.first_dpu().db_machine(&mut txn).await;
    let mut hardware_info = dpu
        .status
        .hardware_info
        .expect("the fixture DPU includes hardware information");
    hardware_info
        .dmi_data
        .as_mut()
        .expect("the fixture DPU includes DMI information")
        .product_name = "BlueField-4 SmartNIC Main Card".to_string();
    // Reopen topology ingestion so the persisted BF4 product mutation reaches host classification.
    db::machine_topology::set_topology_update_needed(txn.as_mut(), &dpu.id, true)
        .await
        .unwrap();
    db::machine_topology::create_or_update(txn.as_mut(), &dpu.id, &hardware_info)
        .await
        .unwrap();
    // Record DPF ingestion after the BF4 topology update so admission reads the updated product.
    db::machine::mark_machine_ingestion_done_with_dpf(txn.as_mut(), &host_id(&host))
        .await
        .unwrap();

    // Declare CX9 in expected-machine data without creating a site-gated Astra DPA row.
    let host_machine = host.host.db_machine(&mut txn).await;
    let bmc_mac = host_machine
        .status
        .bmc_info
        .mac
        .expect("the fixture host includes a BMC MAC");
    let mut expected_machine = db::expected_machine::find_by_bmc_mac_address(txn.as_mut(), bmc_mac)
        .await
        .unwrap()
        .expect("the fixture host includes an expected-machine declaration");
    // Keep parallel hosts distinct while adding the expected-machine signal used with the
    // persisted BF4 product.
    expected_machine.data.interfaces.push(ExpectedInterface {
        mac_address: Some(mac_address::MacAddress::from([
            0x02,
            0,
            0,
            0,
            0,
            cx9_mac_suffix,
        ])),
        nic_type: Some("CX9".to_string()), // Drives profile classification with the DPU product.
        ..ExpectedInterface::default()
    });
    db::expected_machine::update(txn.as_mut(), &expected_machine)
        .await
        .unwrap();

    // Expected-machine CX9 remains the sole Astra signal while the site-gated row is absent.
    let astra_interfaces = db::dpa_interface::find_by_machine_id(
        txn.as_mut(),
        *host_id(&host),
        model::dpa_interface::DpaSearchConfig {
            only_svpc: false,
            only_astra: true,
        },
    )
    .await
    .unwrap();
    assert!(astra_interfaces.is_empty());
    txn.commit().await.unwrap();

    host
}
