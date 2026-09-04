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

use std::collections::HashSet;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use ::rpc::forge::forge_server::Forge;
use ::rpc::forge::{
    AdminForceDeleteMachineRequest, IbPartitionStatus, InstancesByIdsRequest, TenantState,
};
use carbide_dpf::DpuDeploymentType;
use carbide_ib_fabric::config::IBFabricConfig;
use carbide_ib_fabric::ib::{self, GetPartitionOptions, IBFabricManager};
use carbide_machine_controller::dpf::{DpfOperations, MockDpfOperations};
use carbide_uuid::infiniband::IBPartitionId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{MachineId, MachineType};
use carbide_uuid::vpc::VpcPrefixId;
use common::api_fixtures::dpu::create_dpu_machine;
use common::api_fixtures::host::host_discover_dhcp;
use common::api_fixtures::ib_partition::{DEFAULT_TENANT, create_ib_partition};
use common::api_fixtures::instance::create_instance_with_ib_config;
use common::api_fixtures::tpm_attestation::EK_CERT_SERIALIZED;
use common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_managed_host, create_managed_host_multi_dpu,
    create_managed_host_with_dpf, create_test_env, create_test_env_with_overrides, get_config,
    get_instance_type_fixture_id,
};
use config_version::ConfigVersion;
use model::hardware_info::TpmEkCertificate;
use model::ib::{DEFAULT_IB_FABRIC_NAME, IbMembership};
use model::ib_partition::PartitionKey;
use model::instance::NewInstance;
use model::instance::config::InstanceConfig;
use model::instance::config::extension_services::InstanceExtensionServicesConfig;
use model::instance::config::infiniband::InstanceInfinibandConfig;
use model::instance::config::network::{InstanceNetworkConfig, NetworkDetails};
use model::instance::config::nvlink::InstanceNvLinkConfig;
use model::instance::config::spx::InstanceSpxConfig;
use model::instance::config::tenant_config::TenantConfig;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{InstanceState, ManagedHostState};
use model::metadata::Metadata;
use model::os::{InlineIpxe, OperatingSystem, OperatingSystemVariant};
use model::resource_pool::{ResourcePoolDef, ResourcePoolType};
use model::site_explorer::ExploredManagedHost;
use model::tenant::TenantOrganizationId;
use sqlx::{PgConnection, Row};
use tonic::Request;

use crate::api::Api;
use crate::attestation as attest;
use crate::tests::common;

async fn get_partition_status(api: &Api, ib_partition_id: IBPartitionId) -> IbPartitionStatus {
    let segment = api
        .find_ib_partitions_by_ids(Request::new(rpc::forge::IbPartitionsByIdsRequest {
            ib_partition_ids: vec![ib_partition_id],
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .ib_partitions
        .remove(0);

    segment.status.unwrap()
}

#[crate::sqlx_test]
async fn test_admin_force_delete_dpu_only(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await.unwrap();
    db::resource_pool::define(
        &mut txn,
        model::resource_pool::common::LOOPBACK_IP_V6,
        &ResourcePoolDef {
            pool_type: ResourcePoolType::Ipv6,
            prefix: Some("2001:db8::/125".to_string()),
            ranges: vec![],
            delegate_prefix_len: None,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let mut txn = env.pool.begin().await.unwrap();
    let initial_loopback_v6_pool_stats = db::resource_pool::stats(
        txn.as_mut(),
        env.common_pools.ethernet.pool_loopback_ip_v6.name(),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

    let mut txn = env.pool.begin().await.unwrap();
    let dpu_machine = db::machine::find_one(
        txn.as_mut(),
        &dpu_machine_id,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(dpu_machine.network_config.loopback_ip_v6.is_some());
    let allocated_loopback_v6_pool_stats = db::resource_pool::stats(
        txn.as_mut(),
        env.common_pools.ethernet.pool_loopback_ip_v6.name(),
    )
    .await
    .unwrap();
    assert_eq!(
        allocated_loopback_v6_pool_stats.used,
        initial_loopback_v6_pool_stats.used + 1
    );
    assert!(
        !db::state_history::find_by_object_ids(
            &mut txn,
            db::state_history::StateHistoryTableId::Machine,
            &[dpu_machine_id],
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert!(
        !db::machine_topology::find_by_machine_ids(&mut txn, &[dpu_machine_id])
            .await
            .unwrap()
            .is_empty()
    );

    // Model the stale snapshot race directly: the reservation still belongs
    // to this DPU even when its persisted network config no longer names it.
    sqlx::query(
        "UPDATE machines
         SET network_config = jsonb_set(
             network_config,
             '{loopback_ip_v6}',
             'null'::jsonb
         )
         WHERE id = $1",
    )
    .bind(dpu_machine_id)
    .execute(txn.as_mut())
    .await
    .unwrap();
    let network_config = db::machine::get_network_config(txn.as_mut(), &dpu_machine_id)
        .await
        .unwrap();
    assert_eq!(network_config.value.loopback_ip_v6, None);

    let host = db::machine::find_host_by_dpu_machine_id(&mut txn, &dpu_machine_id)
        .await
        .unwrap()
        .unwrap();

    txn.commit().await.unwrap();

    let response = force_delete(&env, &dpu_machine_id).await;
    validate_delete_response(&response, Some(&host.id), &dpu_machine_id);
    assert_eq!(
        response.dpu_machine_interface_id,
        dpu_machine.status.interfaces[0].id.to_string()
    );

    assert!(response.all_done, "DPU must be deleted");

    // Validate that the DPU is gone
    validate_machine_deletion(&env, &dpu_machine_id, None).await;

    let mut txn = env.pool.begin().await.unwrap();
    let released_loopback_v6_pool_stats = db::resource_pool::stats(
        txn.as_mut(),
        env.common_pools.ethernet.pool_loopback_ip_v6.name(),
    )
    .await
    .unwrap();
    assert_eq!(
        released_loopback_v6_pool_stats,
        initial_loopback_v6_pool_stats
    );
}

#[crate::sqlx_test]
async fn test_admin_force_delete_dpu_and_host_by_dpu_machine_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let (host_machine_id, dpu_machine_id) = create_managed_host(&env).await.into();

    let response = force_delete(&env, &dpu_machine_id).await;
    validate_delete_response(&response, Some(&host_machine_id), &dpu_machine_id);
    assert!(response.all_done, "Host must be deleted");

    for id in [host_machine_id.into(), dpu_machine_id.into()] {
        validate_machine_deletion(&env, &id, None).await;
    }

    let retry_response = force_delete(&env, &dpu_machine_id).await;
    assert!(
        retry_response.all_done,
        "Core force deletion must remain idempotent when the target is already absent"
    );
}

async fn is_ek_cert_status_entry_present(txn: &mut PgConnection) -> bool {
    let query = "SELECT COUNT(1)::integer from ek_cert_verification_status;";
    let all_ek_cert_status_count: i32 = sqlx::query(query)
        .fetch_one(txn)
        .await
        .expect("Could not get ek cert statuses")
        .try_get("count")
        .expect("Could not get ek cert status count");

    all_ek_cert_status_count > 0
}

#[crate::sqlx_test]
async fn test_admin_force_delete_dpu_and_host_by_host_machine_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let (host_machine_id, dpu_machine_id) = create_managed_host(&env).await.into();

    let bmc_addrs = vec![
        IpAddr::from_str(
            env.find_machine(&host_machine_id)
                .await
                .first()
                .unwrap()
                .bmc_info
                .as_ref()
                .unwrap()
                .ip
                .as_ref()
                .unwrap(),
        )
        .unwrap(),
        IpAddr::from_str(
            env.find_machine(&dpu_machine_id)
                .await
                .first()
                .unwrap()
                .bmc_info
                .as_ref()
                .unwrap()
                .ip
                .as_ref()
                .unwrap(),
        )
        .unwrap(),
    ];

    let mut txn = env.pool.begin().await.unwrap();

    // create entry in ek_cert_verification_status table
    let ek_cert = TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec());

    attest::match_insert_new_ek_cert_status_against_ca(&mut txn, &ek_cert, &host_machine_id)
        .await
        .expect("Could not insert EK status");

    // Fake some explored endpoints
    for addr in &bmc_addrs {
        db::explored_endpoints::insert(*addr, &Default::default(), false, &mut txn)
            .await
            .unwrap();
    }

    assert!(
        !db::explored_endpoints::find_all_by_ip(bmc_addrs[0], &mut txn)
            .await
            .unwrap()
            .is_empty()
    );

    txn.commit().await.unwrap();

    let mut txn = env.pool.begin().await.unwrap();
    assert!(
        is_ek_cert_status_entry_present(&mut txn).await,
        "FAILURE: EK cert status entry should have been created"
    );

    let response = force_delete(&env, &host_machine_id).await;
    validate_delete_response(&response, Some(&host_machine_id), &dpu_machine_id);

    assert!(env.find_machine(&host_machine_id).await.is_empty());
    assert!(env.find_machine(&dpu_machine_id).await.is_empty());

    assert!(response.all_done, "Host and DPU must be deleted");
    assert!(
        !is_ek_cert_status_entry_present(&mut txn).await,
        "FAILURE: EK cert status entry should have been deleted"
    );

    // Everything should be gone now
    for id in [host_machine_id.into(), dpu_machine_id.into()] {
        validate_machine_deletion(&env, &id, Some(&bmc_addrs)).await;
    }
}

#[crate::sqlx_test]
async fn test_admin_force_delete_dpu_and_partially_discovered_host(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;
    let host_machine_interface_id = host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;

    // The MachineInterface for the host should now exist and be linked to the DPU
    let mut ifaces = env
        .api
        .find_interfaces(tonic::Request::new(rpc::forge::InterfaceSearchQuery {
            id: Some(host_machine_interface_id),
            ip: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ifaces.interfaces.len(), 1);
    let iface = ifaces.interfaces.remove(0);
    assert_eq!(iface.attached_dpu_machine_id, Some(dpu_machine_id.into()));

    let mut txn = env.pool.begin().await.unwrap();
    let host = db::machine::find_host_by_dpu_machine_id(&mut txn, &dpu_machine_id)
        .await
        .unwrap()
        .unwrap();
    txn.commit().await.unwrap();

    let response = force_delete(&env, &dpu_machine_id).await;
    validate_delete_response(&response, Some(&host.id), &dpu_machine_id);
    assert!(response.all_done, "DPU must be deleted");

    validate_machine_deletion(&env, &dpu_machine_id, None).await;

    // The MachineInterface for the host should still exist
    let mut ifaces = env
        .api
        .find_interfaces(tonic::Request::new(rpc::forge::InterfaceSearchQuery {
            id: Some(host_machine_interface_id),
            ip: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ifaces.interfaces.len(), 1);
    let iface = ifaces.interfaces.remove(0);
    assert_eq!(iface.attached_dpu_machine_id, None);
}

/// Force-deletion and a concurrent exploration pass touch the same tables;
/// this pins the lock ordering that keeps the pair deadlock-free. The
/// exploration-order transaction holds every `explored_managed_hosts` row
/// (`explored_managed_host::update` opens with a full-table delete) while
/// force-delete runs, then touches one of the host's `machine_interfaces`
/// rows -- the two-table cycle captured from CI. Force-delete takes the
/// explored tables before any interface rows, so it blocks cleanly on the
/// exploration transaction instead of deadlocking against it.
#[crate::sqlx_test]
async fn test_admin_force_delete_orders_locks_against_exploration(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;

    // The host's BMC ip and one interface id, plus an `explored_managed_hosts`
    // row for the BMC ip so the delete has a row to contend on.
    let mut txn = env.pool.begin().await.unwrap();
    let machine = db::machine::find_one(txn.as_mut(), &host.id, MachineSearchConfig::default())
        .await
        .unwrap()
        .unwrap();
    let bmc_ip = machine
        .status
        .bmc_info
        .ip
        .expect("managed host fixture has a BMC ip");
    let interface_id = machine.status.interfaces[0].id;
    let explored_host = ExploredManagedHost {
        host_bmc_ip: bmc_ip,
        dpus: Vec::new(),
    };
    db::explored_managed_host::update(txn.as_mut(), &[&explored_host])
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Exploration-order transaction: hold the explored_managed_hosts rows
    // first, exactly like site-explorer's persistence pass.
    let mut exploration_txn = env.pool.begin().await.unwrap();
    db::explored_managed_host::update(exploration_txn.as_mut(), &[&explored_host])
        .await
        .unwrap();

    // Launch the force-delete and wait until it blocks on those rows.
    let api = host.api.clone();
    let host_id = host.id;
    let force_delete_task = tokio::spawn(async move {
        api.admin_force_delete_machine(tonic::Request::new(AdminForceDeleteMachineRequest {
            host_query: host_id.to_string(),
            delete_interfaces: true,
            delete_bmc_interfaces: true,
            delete_bmc_credentials: false,
            allow_delete_with_orphaned_dpf_crds: false,
            delete_bmc_suppressions: false,
            delete_retained_boot_interfaces: false,
            allow_delete_with_instance_type: false,
            allow_delete_with_instance: false,
        }))
        .await
    });
    wait_until_blocked_on(&env.pool, "explored_managed_hosts").await;

    // Now take the machine_interfaces row exploration touches second (the
    // identity UPDATE only exists for its row lock). If force-delete already
    // held interface rows here, this pair would deadlock with a 40P01.
    sqlx::query("UPDATE machine_interfaces SET id = id WHERE id = $1")
        .bind(interface_id)
        .execute(exploration_txn.as_mut())
        .await
        .expect("exploration-order interface update must not deadlock against force-delete");
    exploration_txn.commit().await.unwrap();

    let response = force_delete_task
        .await
        .unwrap()
        .expect("force delete completes once exploration commits")
        .into_inner();
    assert!(response.all_done);
    validate_machine_deletion(&env, &host.dpu_ids[0], None).await;
}

/// Multi-endpoint exploration persistence and force-delete must acquire
/// `explored_endpoints` rows in the same ascending address order. The fixture
/// allocates DPU BMC addresses before the host BMC address, so the old
/// host-first force-delete order formed an inverse-order cycle here.
#[crate::sqlx_test]
async fn test_admin_force_delete_orders_endpoint_locks_by_address(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host_multi_dpu(&env, 2).await;

    let mut txn = env.pool.begin().await.unwrap();
    let host_machine = db::machine::find_one(
        txn.as_mut(),
        &managed_host.id,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let dpu_machines = db::machine::find_dpus_by_host_machine_id(txn.as_mut(), &managed_host.id)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let host_address = host_machine
        .status
        .bmc_info
        .ip
        .expect("managed host fixture has a BMC ip");
    let mut endpoint_addresses = dpu_machines
        .iter()
        .filter_map(|machine| machine.status.bmc_info.ip)
        .chain(std::iter::once(host_address))
        .collect::<Vec<_>>();
    endpoint_addresses.sort_unstable();
    assert_eq!(endpoint_addresses.len(), 3);
    assert_ne!(
        endpoint_addresses[0], host_address,
        "fixture must put a DPU endpoint below the host endpoint"
    );

    // Simulate site-explorer taking the lowest endpoint row first.
    let mut exploration_txn = env.pool.begin().await.unwrap();
    sqlx::query("UPDATE explored_endpoints SET address = address WHERE address = $1")
        .bind(endpoint_addresses[0])
        .execute(exploration_txn.as_mut())
        .await
        .unwrap();

    let api = managed_host.api.clone();
    let host_id = managed_host.id;
    let force_delete_task = tokio::spawn(async move {
        api.admin_force_delete_machine(tonic::Request::new(AdminForceDeleteMachineRequest {
            host_query: host_id.to_string(),
            delete_interfaces: false,
            delete_bmc_interfaces: false,
            delete_bmc_credentials: false,
            allow_delete_with_orphaned_dpf_crds: false,
            delete_bmc_suppressions: false,
            delete_retained_boot_interfaces: false,
            allow_delete_with_instance_type: false,
            allow_delete_with_instance: false,
        }))
        .await
    });
    wait_until_blocked_on(&env.pool, "explored_endpoints").await;

    // Continue site-explorer's updates in canonical order. Force-delete must
    // be waiting on the first row without holding any higher-address endpoint.
    for address in endpoint_addresses.iter().skip(1) {
        sqlx::query("UPDATE explored_endpoints SET address = address WHERE address = $1")
            .bind(address)
            .execute(exploration_txn.as_mut())
            .await
            .expect("ascending endpoint update must not deadlock against force-delete");
    }
    exploration_txn.commit().await.unwrap();

    let response = force_delete_task
        .await
        .unwrap()
        .expect("force delete completes once exploration commits")
        .into_inner();
    assert!(response.all_done);
    for machine_id in managed_host
        .dpu_ids
        .iter()
        .copied()
        .map(MachineId::from)
        .chain(std::iter::once(managed_host.id.into()))
    {
        validate_machine_deletion(&env, &machine_id, None).await;
    }
}

/// Site Explorer updates a machine topology before its explored endpoint.
/// Force-delete must take those locks in the same order: otherwise it can
/// hold the endpoint while waiting for the topology, forming a cycle when
/// Site Explorer proceeds to the endpoint.
#[crate::sqlx_test]
async fn test_admin_force_delete_orders_topology_before_endpoint(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host(&env).await;

    let mut txn = env.pool.begin().await.unwrap();
    let machine = db::machine::find_one(
        txn.as_mut(),
        &managed_host.id,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let bmc_ip = machine
        .status
        .bmc_info
        .ip
        .expect("managed host fixture has a BMC ip");
    txn.commit().await.unwrap();

    // Simulate Site Explorer's firmware refresh holding the topology row.
    let mut exploration_txn = env.pool.begin().await.unwrap();
    sqlx::query("UPDATE machine_topologies SET machine_id = machine_id WHERE machine_id = $1")
        .bind(managed_host.id)
        .execute(exploration_txn.as_mut())
        .await
        .unwrap();

    let api = managed_host.api.clone();
    let host_id = managed_host.id;
    let force_delete_task = tokio::spawn(async move {
        api.admin_force_delete_machine(tonic::Request::new(AdminForceDeleteMachineRequest {
            host_query: host_id.to_string(),
            delete_interfaces: false,
            delete_bmc_interfaces: false,
            delete_bmc_credentials: false,
            allow_delete_with_orphaned_dpf_crds: false,
            delete_bmc_suppressions: false,
            delete_retained_boot_interfaces: false,
            allow_delete_with_instance_type: false,
            allow_delete_with_instance: false,
        }))
        .await
    });
    wait_until_blocked_on_any(&env.pool, &["machine_topologies", "cleanup_machine_by_id"]).await;

    // With canonical topology -> endpoint ordering, force-delete is still
    // waiting for the topology and has not locked the endpoint.
    sqlx::query("UPDATE explored_endpoints SET address = address WHERE address = $1")
        .bind(bmc_ip)
        .execute(exploration_txn.as_mut())
        .await
        .expect("endpoint update must not deadlock against force-delete");
    exploration_txn.commit().await.unwrap();

    let response = force_delete_task
        .await
        .unwrap()
        .expect("force delete completes once exploration commits")
        .into_inner();
    assert!(response.all_done);
    validate_machine_deletion(&env, &managed_host.id, None).await;
}

/// Polls `pg_stat_activity` until some backend in this test's database sits
/// in a lock wait on a query that names `relation`. The `datname` filter
/// keeps parallel per-test databases on the shared server out of the match,
/// and the monitor query receives the relation as a bind parameter, so it
/// never matches its own text. The generous cap covers the force-delete
/// RPC's pre-transaction work (Redfish attempts against the fixture BMC)
/// on slow CI runners.
async fn wait_until_blocked_on(pool: &sqlx::PgPool, relation: &str) {
    wait_until_blocked_on_any(pool, &[relation]).await;
}

async fn wait_until_blocked_on_any(pool: &sqlx::PgPool, queries: &[&str]) {
    for _ in 0..600 {
        let waiting_queries: Vec<String> = sqlx::query_scalar(
            "SELECT query FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        if waiting_queries
            .iter()
            .any(|query| queries.iter().any(|needle| query.contains(needle)))
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("force delete never blocked on any of {queries:?}");
}

async fn force_delete(
    env: &TestEnv,
    machine_id: &MachineId,
) -> rpc::forge::AdminForceDeleteMachineResponse {
    env.api
        .admin_force_delete_machine(tonic::Request::new(force_delete_request(machine_id)))
        .await
        .unwrap()
        .into_inner()
}

fn force_delete_request(machine_id: &impl std::fmt::Display) -> AdminForceDeleteMachineRequest {
    AdminForceDeleteMachineRequest {
        host_query: machine_id.to_string(),
        delete_interfaces: false,
        delete_bmc_interfaces: false,
        delete_bmc_credentials: false,
        allow_delete_with_orphaned_dpf_crds: false,
        delete_bmc_suppressions: false,
        delete_retained_boot_interfaces: false,
        allow_delete_with_instance_type: false,
        allow_delete_with_instance: false,
    }
}

/// Test-only function that locks one address so force-delete pauses after
/// marking the Instance but before physically deleting it.
async fn lock_instance_address(txn: &mut PgConnection, instance_id: InstanceId) {
    let address_id: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT id FROM instance_addresses
         WHERE instance_id = $1
         ORDER BY id
         LIMIT 1
         FOR UPDATE",
    )
    .bind(instance_id)
    .fetch_optional(txn)
    .await
    .unwrap();
    assert!(
        address_id.is_some(),
        "fixture Instance must have an address to gate cleanup",
    );
}

async fn retired_membership_is_recorded(pool: &sqlx::PgPool, membership: &IbMembership) -> bool {
    db::retired_ib_membership::find_recorded_candidates(pool, std::slice::from_ref(membership))
        .await
        .unwrap()
        == vec![membership.clone()]
}

fn validate_delete_response(
    response: &rpc::forge::AdminForceDeleteMachineResponse,
    host_machine_id: Option<&MachineId>,
    dpu_machine_id: &MachineId,
) {
    assert_eq!(response.dpu_machine_id, dpu_machine_id.to_string());
    assert_eq!(
        response.managed_host_machine_id,
        host_machine_id.map(|id| id.to_string()).unwrap_or_default()
    );
    assert!(!response.dpu_bmc_ip.is_empty());
    if let Some(host_machine_id) = host_machine_id {
        if host_machine_id.machine_type() == MachineType::Host {
            assert!(!response.managed_host_bmc_ip.is_empty());
        }
    } else {
        assert!(response.managed_host_bmc_ip.is_empty());
    }
}

fn validate_delete_response_multi_dpu(
    response: &rpc::forge::AdminForceDeleteMachineResponse,
    host_machine_id: Option<&MachineId>,
    dpu_machine_ids: &[carbide_uuid::machine::MachineId],
) {
    assert_eq!(
        response
            .dpu_machine_ids
            .iter()
            .map(|i| i.to_owned())
            .collect::<HashSet<_>>(),
        dpu_machine_ids
            .iter()
            .map(|i| i.to_string())
            .collect::<HashSet<_>>()
    );
    assert_eq!(
        response.managed_host_machine_id,
        host_machine_id.map(|id| id.to_string()).unwrap_or_default()
    );
    assert!(!response.dpu_bmc_ip.is_empty());
    if let Some(host_machine_id) = host_machine_id {
        if host_machine_id.machine_type() == MachineType::Host {
            assert!(!response.managed_host_bmc_ip.is_empty());
        }
    } else {
        assert!(response.managed_host_bmc_ip.is_empty());
    }
}

/// Validates that the Machine has been fully deleted
async fn validate_machine_deletion(
    env: &TestEnv,
    machine_id: &MachineId,
    bmc_addrs: Option<&Vec<IpAddr>>,
) {
    // The machine should be now be gone in the API
    let response = env.find_machine(machine_id).await;
    assert!(response.is_empty());

    // And it should also be gone on the DB layer
    let mut txn = env.pool.begin().await.unwrap();
    assert!(
        db::machine::find_one(txn.as_mut(), machine_id, MachineSearchConfig::default())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db::machine_topology::find_by_machine_ids(&mut txn, &[*machine_id])
            .await
            .unwrap()
            .is_empty()
    );

    // The history should remain in table.
    assert!(
        !db::state_history::find_by_object_ids(
            &mut txn,
            db::state_history::StateHistoryTableId::Machine,
            &[*machine_id],
        )
        .await
        .unwrap()
        .is_empty()
    );

    if let Some(bmc_addrs) = bmc_addrs {
        for bmc_addr in bmc_addrs {
            assert!(
                db::explored_endpoints::find_all_by_ip(*bmc_addr, &mut txn)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }
    txn.rollback().await.unwrap();
}

/// Allocation locks the host Machine before inserting its Instance. If
/// force-delete waits behind that lock, it must read membership after the wait
/// and reject deletion without the explicit Instance override.
#[crate::sqlx_test]
async fn test_admin_force_delete_rejects_instance_committed_before_machine_lock(
    pool: sqlx::PgPool,
) {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host(&env).await;
    let instance_id = InstanceId::new();

    // Model allocation's Machine -> Instance lock/write order. This
    // transaction owns the host `machines` row while no `instances` row is
    // visible, then persists the Instance before releasing the Machine.
    let mut allocation_txn = env.pool.begin().await.unwrap();
    let locked_machine = db::machine::find_one(
        allocation_txn.as_mut(),
        &managed_host.id,
        MachineSearchConfig {
            for_update: true,
            ..MachineSearchConfig::default()
        },
    )
    .await
    .unwrap();
    assert!(
        locked_machine.is_some(),
        "fixture host must exist to be locked",
    );
    assert!(
        db::instance::find_id_by_machine_id(allocation_txn.as_mut(), &managed_host.id)
            .await
            .unwrap()
            .is_none(),
        "fixture host must not have an Instance before concurrent allocation",
    );

    // Force-delete cannot read authoritative Instance membership until it owns
    // the same Machine row lock as allocation.
    let api = managed_host.api.clone();
    let host_id = managed_host.id;
    let force_delete_task = tokio::spawn(async move {
        api.admin_force_delete_machine(tonic::Request::new(AdminForceDeleteMachineRequest {
            host_query: host_id.to_string(),
            delete_interfaces: false,
            delete_bmc_interfaces: false,
            delete_bmc_credentials: false,
            allow_delete_with_orphaned_dpf_crds: false,
            delete_bmc_suppressions: false,
            delete_retained_boot_interfaces: false,
            allow_delete_with_instance_type: false,
            allow_delete_with_instance: false,
        }))
        .await
    });
    wait_until_blocked_on(&env.pool, "SELECT row_to_json(m.*)").await;

    let config = InstanceConfig {
        tenant: TenantConfig {
            tenant_organization_id: TenantOrganizationId::try_from("force-delete-race".to_string())
                .unwrap(),
            tenant_keyset_ids: Vec::new(),
            hostname: None,
        },
        os: OperatingSystem {
            user_data: None,
            variant: OperatingSystemVariant::Ipxe(InlineIpxe {
                ipxe_script: "#!ipxe".to_string(),
            }),
            phone_home_enabled: false,
            run_provisioning_instructions_on_every_boot: false,
        },
        network: InstanceNetworkConfig::default(),
        infiniband: InstanceInfinibandConfig::default(),
        network_security_group_id: None,
        extension_services: InstanceExtensionServicesConfig::default(),
        nvlink: InstanceNvLinkConfig::default(),
        spxconfig: InstanceSpxConfig::default(),
        power_profile: None,
    };
    let version = ConfigVersion::initial();
    db::instance::batch_persist(
        vec![NewInstance {
            instance_id,
            machine_id: managed_host.id.into(),
            instance_type_id: None,
            config: &config,
            metadata: Metadata::default(),
            config_version: version,
            network_config_version: version,
            ib_config_version: version,
            extension_services_config_version: version,
            nvlink_config_version: version,
            spx_config_version: version,
        }],
        allocation_txn.as_mut(),
    )
    .await
    .unwrap();
    allocation_txn.commit().await.unwrap();

    let error = force_delete_task
        .await
        .unwrap()
        .expect_err("force-delete must reject the Instance committed while it waited");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(
        db::instance::find_by_id(&env.pool, instance_id)
            .await
            .unwrap()
            .is_some(),
        "the rejected force-delete must retain the concurrent Instance"
    );
    for machine_id in managed_host
        .dpu_ids
        .iter()
        .copied()
        .map(MachineId::from)
        .chain(std::iter::once(managed_host.id.into()))
    {
        assert_eq!(
            env.find_machine(&machine_id).await.len(),
            1,
            "the rejected force-delete must retain Machine {machine_id}"
        );
    }
}

#[crate::sqlx_test]
async fn test_admin_force_delete_rereads_config_committed_before_marker(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let [segment_id, generated_segment_id] = env
        .create_vpc_and_tenant_segments(2)
        .await
        .try_into()
        .unwrap();
    let managed_host = create_managed_host(&env).await;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    let mut writer_txn = env.pool.begin().await.unwrap();
    let initial = db::instance::find_by_id(writer_txn.as_mut(), instance.id)
        .await
        .unwrap()
        .unwrap();
    let mut updated_metadata = initial.metadata.clone();
    updated_metadata.description = "committed before force-delete".to_string();
    // Model the final writes of a config transaction after its generated
    // segment exists. The transaction keeps the Instance lock until it commits.
    let mut requested_network = initial.config.network.clone();
    let interface = &mut requested_network.interfaces[0];
    interface.network_details = Some(NetworkDetails::VpcPrefixId(VpcPrefixId::new()));
    interface.network_segment_id = Some(generated_segment_id);
    db::instance::trigger_update_network_config_request(
        &instance.id,
        &initial.config.network,
        &requested_network,
        &mut writer_txn,
    )
    .await
    .unwrap();
    db::instance::update_config(
        writer_txn.as_mut(),
        instance.id,
        initial.config_version,
        initial.config.clone(),
        updated_metadata.clone(),
    )
    .await
    .unwrap();

    let mut address_guard = env.pool.begin().await.unwrap();
    lock_instance_address(address_guard.as_mut(), instance.id).await;

    let api = env.api.clone();
    let machine_id = managed_host.id;
    let force_delete = async move {
        let mut request = force_delete_request(&machine_id);
        request.allow_delete_with_instance = true;
        api.admin_force_delete_machine(Request::new(request)).await
    };
    let orchestrate = async {
        wait_until_blocked_on(&env.pool, "SELECT row_to_json(m.*)").await;
        writer_txn.commit().await.unwrap();

        wait_until_blocked_on(&env.pool, "SELECT id FROM instance_addresses").await;
        let marked = db::instance::find_by_id(&env.pool, instance.id)
            .await
            .unwrap()
            .expect("address gate must keep the marked Instance available");
        assert!(marked.deleted.is_some());
        assert_eq!(
            marked
                .update_network_config_request
                .as_ref()
                .expect("the pending network update committed before the marker must be captured")
                .new_config,
            requested_network,
        );
        assert_eq!(marked.metadata, updated_metadata);
        assert_eq!(
            marked.config_version.version_nr(),
            initial.config_version.version_nr() + 1,
        );

        address_guard.commit().await.unwrap();
    };

    let (response, ()) = tokio::join!(force_delete, orchestrate);
    let response = response.unwrap().into_inner();
    validate_delete_response(&response, Some(&managed_host.id), &managed_host.dpu().id);
    for machine_id in [managed_host.id.into(), managed_host.dpu().id.into()] {
        validate_machine_deletion(&env, &machine_id, None).await;
    }
    let generated_segment_is_deleted: bool =
        sqlx::query_scalar("SELECT deleted IS NOT NULL FROM network_segments WHERE id = $1")
            .bind(generated_segment_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert!(
        generated_segment_is_deleted,
        "force-delete must clean resources from the configuration committed before the marker",
    );
}

#[crate::sqlx_test]
async fn test_admin_force_delete_marker_rejects_started_config_update(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let managed_host = create_managed_host(&env).await;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    let mut stale_writer_txn = env.pool.begin().await.unwrap();
    let initial = db::instance::find_by_id(stale_writer_txn.as_mut(), instance.id)
        .await
        .unwrap()
        .unwrap();
    let initial_metadata = initial.metadata.clone();
    let mut rejected_metadata = initial_metadata.clone();
    rejected_metadata.description = "must not commit after force-delete".to_string();

    let mut address_guard = env.pool.begin().await.unwrap();
    lock_instance_address(address_guard.as_mut(), instance.id).await;

    let api = env.api.clone();
    let machine_id = managed_host.id;
    let force_delete = async move {
        let mut request = force_delete_request(&machine_id);
        request.allow_delete_with_instance = true;
        api.admin_force_delete_machine(Request::new(request)).await
    };
    let orchestrate = async {
        wait_until_blocked_on(&env.pool, "SELECT id FROM instance_addresses").await;

        let error = db::instance::update_config(
            stale_writer_txn.as_mut(),
            instance.id,
            initial.config_version,
            initial.config.clone(),
            rejected_metadata,
        )
        .await
        .expect_err("a config update whose terminal write runs after the marker must not commit");
        assert!(
            matches!(error, db::DatabaseError::FailedPrecondition(_)),
            "unexpected config update error: {error:?}",
        );
        stale_writer_txn.rollback().await.unwrap();

        let marked = db::instance::find_by_id(&env.pool, instance.id)
            .await
            .unwrap()
            .expect("address gate must keep the marked Instance available");
        assert!(marked.deleted.is_some());
        assert_eq!(marked.metadata, initial_metadata);
        assert_eq!(marked.config_version, initial.config_version);

        address_guard.commit().await.unwrap();
    };

    let (response, ()) = tokio::join!(force_delete, orchestrate);
    let response = response.unwrap().into_inner();
    validate_delete_response(&response, Some(&managed_host.id), &managed_host.dpu().id);
    for machine_id in [managed_host.id.into(), managed_host.dpu().id.into()] {
        validate_machine_deletion(&env, &machine_id, None).await;
    }
}

#[crate::sqlx_test]
async fn test_admin_force_delete_host_with_ib_instance(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        ..Default::default()
    });

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;

    let segment_id = env.create_vpc_and_tenant_segment().await;
    let (ib_partition_id, ib_partition) = create_ib_partition(
        &env,
        "test_ib_partition".to_string(),
        DEFAULT_TENANT.to_string(),
    )
    .await;

    env.run_ib_partition_controller_iteration().await;

    let ib_partition_status = get_partition_status(&env.api, ib_partition_id).await;
    assert_eq!(
        TenantState::try_from(ib_partition_status.state).unwrap(),
        TenantState::Ready
    );
    assert_eq!(
        ib_partition.status.clone().unwrap().state,
        ib_partition_status.state
    );
    assert_eq!(
        ib_partition.status.clone().unwrap().pkey,
        ib_partition_status.pkey
    );
    assert!(ib_partition_status.pkey.is_some());
    assert!(ib_partition_status.mtu.is_none());
    assert!(ib_partition_status.rate_limit.is_none());
    assert!(ib_partition_status.service_level.is_none());

    let mh = create_managed_host(&env).await;

    env.run_machine_state_controller_iteration().await;

    let mut txn = env
        .pool
        .clone()
        .begin()
        .await
        .expect("Unable to create transaction on database pool");

    let machine = mh.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();

    let ib_fabric = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();

    assert_eq!(machine.current_state(), &ManagedHostState::Ready);
    assert!(!machine.is_dpu());
    assert!(machine.status.hardware_info.as_ref().is_some());
    assert_eq!(
        machine
            .status
            .hardware_info
            .as_ref()
            .unwrap()
            .infiniband_interfaces
            .len(),
        6
    );
    assert!(
        machine
            .status
            .infiniband_status_observation
            .as_ref()
            .is_some()
    );
    assert_eq!(
        machine
            .status
            .infiniband_status_observation
            .as_ref()
            .unwrap()
            .ib_interfaces
            .len(),
        6
    );
    assert_eq!(ib_fabric.find_ib_port(None).await.unwrap().len(), 6);

    let ib_config = rpc::forge::InstanceInfinibandConfig {
        ib_interfaces: vec![rpc::forge::InstanceIbInterfaceConfig {
            function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
            virtual_function_id: None,
            ib_partition_id: Some(ib_partition_id),
            device: "MT2910 Family [ConnectX-7]".to_string(),
            vendor: None,
            device_instance: 1,
        }],
    };

    let (tinstance, instance) =
        create_instance_with_ib_config(&env, &mh, ib_config, segment_id).await;

    let mut txn = env
        .pool
        .clone()
        .begin()
        .await
        .expect("Unable to create transaction on database pool");
    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready
        }
    ));
    txn.commit().await.unwrap();

    let check_instance = tinstance.rpc_instance().await;
    assert_eq!(check_instance.machine_id(), mh.id.into());
    assert_eq!(check_instance.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance, check_instance);

    let ib_config = check_instance.config().infiniband();
    assert_eq!(ib_config.ib_interfaces.len(), 1);

    let ib_status = check_instance.status().infiniband();
    assert_eq!(ib_status.ib_interfaces.len(), 1);

    // one ib port in UFM
    let hex_pkey = ib_partition.status.clone().unwrap().pkey.unwrap();
    let pkey: u16 = u16::from_str_radix(hex_pkey.strip_prefix("0x").unwrap(), 16)
        .expect("Failed to parse string to integer");
    let guid = ib_status.ib_interfaces[0].guid.clone().unwrap();
    let guids = HashSet::from_iter([guid.clone()]);
    let filter = ib::Filter {
        guids: Some(guids.clone()),
        pkey: Some(pkey),
        state: Some(model::ib::IBPortState::Active),
    };
    assert_eq!(ib_fabric.find_ib_port(Some(filter)).await.unwrap().len(), 1);

    let retired_membership = IbMembership {
        fabric: DEFAULT_IB_FABRIC_NAME.to_string(),
        pkey: PartitionKey::try_from(pkey).unwrap(),
        guid,
    };
    let ib_network = ib_fabric
        .get_ib_network(
            pkey,
            GetPartitionOptions {
                include_guids_data: false,
                include_qos_conf: true,
            },
        )
        .await
        .unwrap();

    let mock_fabric = env.ib_fabric_manager.get_mock_manager();
    mock_fabric.set_unbind_failure(true);
    let mut request = force_delete_request(&mh.id);
    request.allow_delete_with_instance = true;
    let error = env
        .api
        .admin_force_delete_machine(Request::new(request))
        .await
        .expect_err("the simulated UFM failure must stop force-delete");
    assert!(error.message().contains("simulated UFM unbind failure"));
    let retained_instance = db::instance::find_by_id(&env.pool, tinstance.id)
        .await
        .unwrap()
        .expect("UFM failure must not delete the Instance");
    let deleted_at = retained_instance
        .deleted
        .expect("force-delete must mark the Instance before calling UFM");
    assert!(
        retired_membership_is_recorded(&env.pool, &retired_membership).await,
        "force-delete must commit the retired membership before calling UFM"
    );

    let mut request = force_delete_request(&mh.id);
    request.allow_delete_with_instance = true;
    let repeated_error = env
        .api
        .admin_force_delete_machine(Request::new(request))
        .await
        .expect_err("the repeated UFM failure must stop the retry");
    assert!(
        repeated_error
            .message()
            .contains("simulated UFM unbind failure")
    );
    let retained_after_retry = db::instance::find_by_id(&env.pool, tinstance.id)
        .await
        .unwrap()
        .expect("a repeated UFM failure must retain the Instance");
    assert_eq!(
        retained_after_retry.deleted.as_ref(),
        Some(&deleted_at),
        "a retry must preserve the original deletion timestamp",
    );

    mock_fabric.set_unbind_failure(false);

    let mut request = force_delete_request(&mh.id);
    request.allow_delete_with_instance = true;
    let response = env
        .api
        .admin_force_delete_machine(Request::new(request))
        .await
        .unwrap()
        .into_inner();
    validate_delete_response(&response, Some(&mh.id), &mh.dpu().id);

    // after host deleted, ib port should be removed from UFM
    let filter = ib::Filter {
        guids: Some(guids.iter().cloned().collect()),
        pkey: Some(pkey),
        state: Some(model::ib::IBPortState::Active),
    };
    assert_eq!(ib_fabric.find_ib_port(Some(filter)).await.unwrap().len(), 0);

    assert!(env.find_machine(&mh.id).await.is_empty());
    assert!(env.find_machine(&mh.dpu().id).await.is_empty());

    assert_eq!(response.ufm_unregistrations, 1);
    assert!(response.all_done, "Host and DPU must be deleted");
    assert!(
        retired_membership_is_recorded(&env.pool, &retired_membership).await,
        "successful retry must keep the exact retired membership"
    );

    // Model a bind from an older monitor pass completing after force-delete.
    // The durable record must let a later pass remove it again.
    ib_fabric
        .bind_ib_ports(ib_network, vec![retired_membership.guid.clone()])
        .await
        .unwrap();
    env.run_ib_fabric_monitor_iteration().await;
    let filter = ib::Filter {
        guids: Some(guids),
        pkey: Some(pkey),
        state: Some(model::ib::IBPortState::Active),
    };
    assert_eq!(ib_fabric.find_ib_port(Some(filter)).await.unwrap().len(), 0);
    assert!(
        retired_membership_is_recorded(&env.pool, &retired_membership).await,
        "monitor cleanup must keep the exact retired membership"
    );

    // Everything should be gone now
    for id in [mh.id.into(), mh.dpu().id.into()] {
        validate_machine_deletion(&env, &id, None).await;
    }
}

#[crate::sqlx_test]
async fn test_admin_force_delete_managed_host_multi_dpu(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    let host_machine = mh.host().rpc_machine().await;
    let dpu_ids = host_machine
        .status
        .as_ref()
        .unwrap()
        .associated_dpu_machine_ids
        .clone();
    assert_eq!(
        dpu_ids.len(),
        2,
        "Should have gotten 2 DPUs from the managed host we created"
    );

    assert!(
        env.api
            .find_machines_by_ids(tonic::Request::new(rpc::forge::MachinesByIdsRequest {
                machine_ids: dpu_ids.clone(),
                ..Default::default()
            }))
            .await
            .is_ok_and(|response| response.into_inner().machines.len() == 2),
        "Expected to find 2 dpu machines when looking up by ID"
    );

    // Delete the *host* machine
    let response = force_delete(&env, &mh.host().id).await;

    validate_delete_response_multi_dpu(&response, Some(&mh.host().id), dpu_ids.as_slice());

    for id in [&[mh.host().id.into()], dpu_ids.as_slice()].concat().iter() {
        validate_machine_deletion(&env, id, None).await;
    }
}

#[crate::sqlx_test]
async fn test_admin_force_delete_dpu_from_managed_host_multi_dpu(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    let dpu_0_id = mh.dpu_n(0).id;
    let rpc_dpu_ids = mh
        .dpu_ids
        .clone()
        .into_iter()
        .map(Into::into)
        .collect::<Vec<carbide_uuid::machine::MachineId>>();
    assert_eq!(
        mh.dpu_ids.len(),
        2,
        "Should have gotten 2 DPUs from the managed host we created"
    );

    assert!(
        env.api
            .find_machines_by_ids(tonic::Request::new(rpc::forge::MachinesByIdsRequest {
                machine_ids: rpc_dpu_ids.clone(),
                ..Default::default()
            }))
            .await
            .is_ok_and(|response| response.into_inner().machines.len() == 2),
        "Expected to find 2 dpu machines when looking up by ID"
    );

    // Delete one of the *dpu* machines, which should cascade and delete the host and other DPU machines
    let response = force_delete(&env, &dpu_0_id).await;

    validate_delete_response_multi_dpu(&response, Some(&mh.host().id), &rpc_dpu_ids);

    for id in mh
        .dpu_ids
        .iter()
        .copied()
        .map(MachineId::from)
        .chain([mh.id.into()])
    {
        validate_machine_deletion(&env, &id, None).await;
    }
}

// test_admin_force_delete_tenant_state verifies that an instance containing a host machine in a ForceDeletion state will have a TenantState of Terminating.
#[crate::sqlx_test]
async fn test_admin_force_delete_tenant_state(pool: sqlx::PgPool) {
    // 1) setup
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    // 2) mock force-delete

    // If we use the RPC API to try to force delete this instance, everything is probably going to be cleaned up and we will likely not be able to retrieve the host's machine.
    // The simplest solution to test how we map ManagedHostState::ForceDeletion -->  TenantState::Terminating is to manually set the machine's
    // ManagedHostState to ForceDeletion in the DB.

    let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();

    let host_machine = mh.host().db_machine(&mut txn).await;

    db::machine::advance(
        &host_machine,
        &mut txn,
        &ManagedHostState::ForceDeletion,
        None,
    )
    .await
    .unwrap();

    txn.commit().await.unwrap();

    // 3) verify instance's tenant state is rpc::forge::TenantState::Terminating
    let request_instances = tonic::Request::new(InstancesByIdsRequest {
        instance_ids: vec![tinstance.id],
    });
    let mut instance_list = env
        .api
        .find_instances_by_ids(request_instances)
        .await
        .map(|response| response.into_inner())
        .unwrap();

    assert_eq!(instance_list.instances.len(), 1);
    let instance = instance_list.instances.pop().unwrap();

    let current_tenant_state = instance
        .status
        .as_ref()
        .unwrap()
        .tenant
        .as_ref()
        .unwrap()
        .state();
    let expected_tenant_state = rpc::forge::TenantState::Terminating;
    assert_eq!(
        current_tenant_state, expected_tenant_state,
        "The instance has a tenant state of {current_tenant_state:#?} instead of {expected_tenant_state:#?}"
    );
}

#[crate::sqlx_test]
async fn test_admin_force_delete_with_instance_type(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let instance_type_id = get_instance_type_fixture_id(&env).await;

    let (tmp_machine_id, _) = create_managed_host(&env).await.into();

    // Associate the machine with the instance type
    let _ = env
        .api
        .associate_machines_with_instance_type(tonic::Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![tmp_machine_id.to_string()],
            },
        ))
        .await
        .unwrap();

    let mut request = force_delete_request(&tmp_machine_id);

    // The default request should fail because the machine is associated with
    // an instance type.
    let error = env
        .api
        .admin_force_delete_machine(tonic::Request::new(request.clone()))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    let retained_machines = env.find_machine(&tmp_machine_id).await;
    assert_eq!(retained_machines.len(), 1);
    assert_eq!(
        retained_machines[0]
            .config
            .as_ref()
            .unwrap()
            .instance_type_id,
        Some(instance_type_id.to_string())
    );

    // An explicit override should remove the associated machine.
    request.allow_delete_with_instance_type = true;
    let response = env
        .api
        .admin_force_delete_machine(tonic::Request::new(request))
        .await
        .unwrap()
        .into_inner();
    assert!(
        response.all_done,
        "the machine should delete when the instance type override is set"
    );
    assert!(env.find_machine(&tmp_machine_id).await.is_empty());
}

/// Force delete with DPF: the node_id and dpu_device_names passed to
/// force_delete_host must be BMC MAC-derived ids, not 64-char MachineIds,
/// so that the resulting K8s resource names stay within the 48-char limit
/// after the SDK adds the `node-` / `device-` CR prefixes.
#[crate::sqlx_test]
async fn test_admin_force_delete_with_dpf_uses_bmc_mac(pool: sqlx::PgPool) {
    type DpfCallLog = Vec<(String, Vec<String>)>;
    let captured_calls: Arc<std::sync::Mutex<DpfCallLog>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut mock = MockDpfOperations::new();

    mock.expect_register_dpu_device().returning(|_, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_deployment_type_for_dpu()
        .returning(|__, _| Ok(DpuDeploymentType::Bf3));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    mock.expect_get_dpu_phase()
        .returning(|_, _| Ok(carbide_dpf::DpuPhase::Ready));

    let cap = captured_calls.clone();
    mock.expect_force_delete_host()
        .returning(move |node_name, device_names| {
            cap.lock()
                .unwrap()
                .push((node_name.to_string(), device_names.to_vec()));
            Ok(())
        });

    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);
    let mut config = get_config();
    config.dpf = crate::cfg::file::DpfConfig {
        enabled: true,
        deployments: crate::cfg::file::DpfDeploymentsConfig {
            bf3: crate::cfg::file::DpfDeploymentConfig {
                bfb_url: Some("http://example.com/test.bfb".to_string()),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };

    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        create_managed_host_with_dpf(&env),
    )
    .await
    .expect("timed out during initial provisioning");
    let host_id = mh.id;

    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        force_delete(&env, &host_id),
    )
    .await
    .expect("timed out during force_delete");

    let calls = captured_calls.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        1,
        "force_delete_host should have been called exactly once, got: {calls:?}"
    );

    let (node_id, device_ids) = &calls[0];

    // BMC MAC -> dpf_id formats as `xx-xx-xx-xx-xx-xx` (17 chars). The SDK adds
    // a `node-` prefix to form the CR name, so the id itself must leave room
    // for that prefix within the 48-char DPUNode CRD limit.
    let mac_re = regex::Regex::new(r"^[0-9a-f]{2}(-[0-9a-f]{2}){5}$").unwrap();
    assert!(
        mac_re.is_match(node_id),
        "node_id should be a BMC MAC-derived id (xx-xx-xx-xx-xx-xx), got: {node_id}",
    );
    assert!(
        format!("node-{node_id}").len() <= 48,
        "node CR name must be <= 48 chars for DPUNode CRD, got {} chars",
        format!("node-{node_id}").len(),
    );

    for id in device_ids {
        assert!(
            mac_re.is_match(id),
            "dpu device id should be a MAC-derived id (xx-xx-xx-xx-xx-xx), got: {id}",
        );
        assert!(
            format!("device-{id}").len() <= 48,
            "device CR name must be <= 48 chars, got {} chars",
            format!("device-{id}").len(),
        );
    }
}

/// `delete_interfaces` keeps each deleted interface's boot pair alive in
/// `retained_boot_interfaces`: the vendor-named Redfish interface id is the
/// one piece a re-ingested machine can't always rediscover on its own
/// (after a DPU-to-NIC mode flip the BMC can report the id without its
/// MAC), so the pair is recorded at the only moment it's guaranteed
/// complete -- deletion.
#[crate::sqlx_test]
async fn test_admin_force_delete_retains_boot_interface_ids(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    // Record a boot interface id on the host's interface row, as
    // site-explorer would during exploration.
    let mut txn = env.pool.begin().await.unwrap();
    let host_machine = db::machine::find_one(
        txn.as_mut(),
        &host_machine_id,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let boot_mac = host_machine.status.interfaces[0].mac_address;
    db::machine_interface::set_boot_interface_id(boot_mac, "NIC.Slot.5-1", txn.as_mut())
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let response = env
        .api
        .admin_force_delete_machine(tonic::Request::new(AdminForceDeleteMachineRequest {
            host_query: host_machine_id.to_string(),
            delete_interfaces: true,
            delete_bmc_interfaces: false,
            delete_bmc_credentials: false,
            allow_delete_with_orphaned_dpf_crds: false,
            delete_bmc_suppressions: false,
            delete_retained_boot_interfaces: false,
            allow_delete_with_instance_type: false,
            allow_delete_with_instance: false,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(response.all_done, "host must be deleted");
    assert!(response.host_interfaces_deleted);

    let mut txn = env.pool.begin().await.unwrap();
    // The interface row is gone...
    assert!(
        db::machine_interface::find_by_mac_address(txn.as_mut(), boot_mac)
            .await
            .unwrap()
            .is_empty()
    );
    // ...and its boot pair survives in retention, ready for the re-ingest.
    assert_eq!(
        db::retained_boot_interface::find_by_mac(txn.as_mut(), boot_mac, None)
            .await
            .unwrap()
            .as_deref(),
        Some("NIC.Slot.5-1"),
    );
    txn.rollback().await.unwrap();
}

/// Clearing suppressions and retained boot pairs is opt-in so the default
/// force-delete path still leaves rediscovery suppressions and boot-target
/// memory intact. With both flags set (plus interface deletes), the wipe
/// matches a permanent removal that expects a clean rediscovery.
#[crate::sqlx_test]
async fn test_admin_force_delete_clears_suppressions_and_retained_boot(pool: sqlx::PgPool) {
    use model::bmc_suppression::{BmcSuppressionSubsystem, NewBmcSuppression};

    let env = create_test_env(pool).await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    let mut txn = env.pool.begin().await.unwrap();
    let host_machine = db::machine::find_one(
        txn.as_mut(),
        &host_machine_id,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let boot_mac = host_machine.status.interfaces[0].mac_address;
    let bmc_mac = host_machine.status.bmc_info.mac.expect("host has BMC MAC");
    db::machine_interface::set_boot_interface_id(boot_mac, "NIC.Slot.5-1", txn.as_mut())
        .await
        .unwrap();
    db::bmc_suppression::upsert(
        txn.as_mut(),
        &NewBmcSuppression {
            bmc_mac_address: bmc_mac,
            subsystem: BmcSuppressionSubsystem::SiteExplorer,
            reason: "test".to_string(),
        },
    )
    .await
    .unwrap();
    db::bmc_suppression::upsert(
        txn.as_mut(),
        &NewBmcSuppression {
            bmc_mac_address: bmc_mac,
            subsystem: BmcSuppressionSubsystem::Dhcp,
            reason: "test".to_string(),
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let response = env
        .api
        .admin_force_delete_machine(tonic::Request::new(AdminForceDeleteMachineRequest {
            host_query: host_machine_id.to_string(),
            delete_interfaces: true,
            delete_bmc_interfaces: true,
            delete_bmc_credentials: false,
            allow_delete_with_orphaned_dpf_crds: false,
            delete_bmc_suppressions: true,
            delete_retained_boot_interfaces: true,
            allow_delete_with_instance_type: false,
            allow_delete_with_instance: false,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(response.all_done);
    assert!(response.host_interfaces_deleted);

    let mut txn = env.pool.begin().await.unwrap();
    assert!(
        db::bmc_suppression::find(txn.as_mut(), bmc_mac, BmcSuppressionSubsystem::SiteExplorer)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db::bmc_suppression::find(txn.as_mut(), bmc_mac, BmcSuppressionSubsystem::Dhcp)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db::retained_boot_interface::find_by_mac(txn.as_mut(), boot_mac, None)
            .await
            .unwrap()
            .is_none()
    );
    txn.rollback().await.unwrap();
}
