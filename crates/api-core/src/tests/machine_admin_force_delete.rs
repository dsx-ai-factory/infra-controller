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
use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use carbide_uuid::infiniband::IBPartitionId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{MachineId, MachineIdSubtypeTrait, MachineType};
use carbide_uuid::vpc::VpcPrefixId;
use common::api_fixtures::dpu::create_dpu_machine;
use common::api_fixtures::host::host_discover_dhcp;
use common::api_fixtures::ib_partition::{DEFAULT_TENANT, create_ib_partition};
use common::api_fixtures::instance::{create_instance_with_ib_config, seed_service_vpc_endpoint};
use common::api_fixtures::tpm_attestation::EK_CERT_SERIALIZED;
use common::api_fixtures::vpc::create_vpc;
use common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_managed_host, create_managed_host_multi_dpu,
    create_managed_host_with_dpf, create_test_env, create_test_env_with_overrides, get_config,
    get_instance_type_fixture_id, network_configured_with_health,
};
use config_version::ConfigVersion;
use model::address_selection_strategy::AddressSelectionStrategy;
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
use model::machine_interface::InterfaceType;
use model::machine_interface_address::MachineInterfaceAssociation;
use model::metadata::Metadata;
use model::os::{InlineIpxe, OperatingSystem, OperatingSystemVariant};
use model::resource_pool::common::{FNN_ASN, LOOPBACK_IP, VPC_DPU_LOOPBACK};
use model::resource_pool::{OwnerType, ResourcePoolDef, ResourcePoolEntryState, ResourcePoolType};
use model::site_explorer::{EndpointExplorationReport, ExploredManagedHost};
use model::tenant::TenantOrganizationId;
use sqlx::{PgConnection, Row};
use tonic::Request;

use crate::api::Api;
use crate::attestation as attest;
use crate::tests::common;
use crate::tests::dpf::phase_observation;

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
    let (vpc_id, _) = create_vpc(&env, "force-delete loopback".to_string(), None, None).await;

    let mut txn = env.pool.begin().await.unwrap();
    let dpu_machine = db::machine::find_one(
        txn.as_mut(),
        &dpu_machine_id,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let vpc_loopback = db::vpc_dpu_loopback::get_or_allocate_loopback_ip_for_vpc(
        &env.common_pools,
        &mut txn,
        &dpu_machine_id,
        &vpc_id,
    )
    .await
    .expect("allocate VPC loopback");
    let allocations = [
        (
            LOOPBACK_IP,
            dpu_machine
                .network_config
                .loopback_ip
                .expect("DPU IPv4 loopback")
                .to_string(),
        ),
        (FNN_ASN, dpu_machine.asn.expect("DPU ASN").to_string()),
        (VPC_DPU_LOOPBACK, vpc_loopback.to_string()),
    ];
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

    let db_pool = &env.pool;
    let allocation_state = |(name, value): (String, String)| async move {
        let entry = db::resource_pool::find_value(db_pool, &value)
            .await
            .expect("find allocation")
            .into_iter()
            .find(|entry| entry.pool_name == name)
            .expect("allocation must remain in its pool");
        Ok::<_, ()>(entry.state.0)
    };
    check_cases_async(
        allocations.iter().map(|(name, value)| Case {
            scenario: name,
            input: (name.to_string(), value.clone()),
            expect: Yields(ResourcePoolEntryState::Allocated {
                owner: dpu_machine_id.to_string(),
                owner_type: OwnerType::Machine.to_string(),
            }),
        }),
        &allocation_state,
    )
    .await;

    let response = force_delete(&env, &dpu_machine_id).await;
    validate_delete_response(&response, Some(&host.id), &dpu_machine_id);
    assert_eq!(
        response.dpu_machine_interface_id,
        dpu_machine.status.interfaces[0].id.to_string()
    );

    assert!(response.all_done, "DPU must be deleted");

    // Validate that the DPU is gone
    validate_machine_deletion(&env, &dpu_machine_id, None).await;

    check_cases_async(
        allocations.into_iter().map(|(name, value)| Case {
            scenario: name,
            input: (name.to_string(), value),
            expect: Yields(ResourcePoolEntryState::Free),
        }),
        allocation_state,
    )
    .await;
    let mut txn = env.pool.begin().await.unwrap();
    assert!(
        db::vpc_dpu_loopback::find(&mut txn, &dpu_machine_id, &vpc_id)
            .await
            .expect("find deleted VPC loopback")
            .is_none()
    );
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
    use futures_util::future::maybe_done;
    use tokio::time::{Duration, timeout};

    let env = create_test_env(pool).await;
    let (host_machine_id, dpu_machine_id) = create_managed_host(&env).await.into();

    let bmc_addrs = vec![
        IpAddr::from_str(
            env.find_machine(&host_machine_id)
                .await
                .first()
                .unwrap()
                .bmc
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
                .bmc
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

    // Pause the real cleanup DELETE after its Machine phase has committed.
    let statement = format!(
        r#"
        CREATE FUNCTION pause_force_delete_host_cleanup()
        RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            PERFORM pg_advisory_xact_lock(5333, 61);
            RETURN OLD;
        END;
        $$;
        CREATE TRIGGER pause_force_delete_host_cleanup
        BEFORE DELETE ON machines
        FOR EACH ROW WHEN (OLD.id = '{host_machine_id}')
        EXECUTE FUNCTION pause_force_delete_host_cleanup();
        "#
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
        .execute(&env.pool)
        .await
        .unwrap();
    let mut gate = env.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(5333, 61)")
        .execute(gate.as_mut())
        .await
        .unwrap();
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(gate.as_mut())
        .await
        .unwrap();
    let deletion = maybe_done(
        env.api
            .admin_force_delete_machine(Request::new(force_delete_request(&host_machine_id))),
    );
    let retry = maybe_done(
        env.api
            .admin_force_delete_machine(Request::new(force_delete_request(&host_machine_id))),
    );
    tokio::pin!(deletion, retry);
    let cleanup_wait = tokio::select! {
        () = &mut deletion => Err("first force-delete passed its cleanup gate".to_string()),
        observed = observe_force_delete_state_lock_wait(&env.pool, Some(gate_pid), None) => observed,
    };
    let host_wait = if let Ok((cleanup_pid, _, _)) = cleanup_wait.as_ref() {
        Some(tokio::select! {
            () = &mut deletion => Err("first force-delete passed its cleanup gate".to_string()),
            () = &mut retry => Err("second force-delete did not wait for the host".to_string()),
            observed = observe_force_delete_state_lock_wait(&env.pool, Some(*cleanup_pid), None) => observed,
        })
    } else {
        None
    };
    let release = gate.rollback().await;
    let mut cancellations = Vec::new();
    let finished = {
        let joined = async {
            tokio::join!(&mut deletion, &mut retry);
        };
        tokio::pin!(joined);
        let finished = timeout(Duration::from_secs(60), &mut joined).await.is_ok();
        if !finished {
            let observed_pids = cleanup_wait
                .as_ref()
                .ok()
                .map(|wait| wait.0)
                .into_iter()
                .chain(
                    host_wait
                        .as_ref()
                        .and_then(|wait| wait.as_ref().ok())
                        .map(|wait| wait.0),
                );
            for pid in observed_pids {
                cancellations.push((
                    pid,
                    sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                        .bind(pid)
                        .fetch_one(&env.pool)
                        .await,
                ));
            }
            timeout(Duration::from_secs(30), &mut joined)
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "cancelled force-delete calls did not join: {error}; \
                     cleanup_wait={cleanup_wait:?}; host_wait={host_wait:?}; \
                     release={release:?}; cancellations={cancellations:?}"
                    )
                });
        }
        finished
    };
    let deletion_result = deletion.as_mut().take_output().expect("first call joined");
    let retry_result = retry.as_mut().take_output().expect("second call joined");
    let dropped_gate = sqlx::raw_sql(
        "DROP TRIGGER pause_force_delete_host_cleanup ON machines;
         DROP FUNCTION pause_force_delete_host_cleanup();",
    )
    .execute(&env.pool)
    .await;
    let diagnostic = format!(
        "cleanup_wait={cleanup_wait:?}; host_wait={host_wait:?}; release={release:?}; \
         finished={finished}; cancellations={cancellations:?}; \
         deletion={deletion_result:?}; retry={retry_result:?}"
    );
    release.expect("release the cleanup gate");
    dropped_gate.expect("remove the joined cleanup gate");
    assert!(finished, "{diagnostic}");
    let (_, cleanup_query, cleanup_advisory) = cleanup_wait.expect("observe real cleanup");
    assert!(
        cleanup_advisory && cleanup_query.contains("cleanup_machine_by_id"),
        "{diagnostic}"
    );
    let (_, host_query, host_advisory) = host_wait
        .expect("retry started")
        .expect("observe host wait");
    assert!(
        !host_advisory
            && host_query.starts_with("SELECT id FROM machines WHERE id = $1 FOR UPDATE"),
        "{diagnostic}"
    );
    let retry_response = retry_result
        .expect("a vanished host is already absent")
        .into_inner();
    assert_eq!(
        retry_response,
        rpc::forge::AdminForceDeleteMachineResponse {
            all_done: true,
            ..Default::default()
        },
        "{diagnostic}"
    );
    let response = deletion_result
        .expect("the original deletion succeeds")
        .into_inner();
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
    assert_eq!(iface.attached_dpu_machine_id, Some(dpu_machine_id));

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
            release_preserved_addresses: false,
            wait_for_instance_dpu: false,
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

/// The persisted fate of a marked host-interface address after a force-delete.
struct MarkedAddressFate {
    mac: mac_address::MacAddress,
    address: IpAddr,
    allocation_type: model::allocation_type::AllocationType,
    /// Reservations owning the address after teardown (parked rows).
    parked: Vec<db::machine_interface_address::ReservedAddress>,
    /// Rows referencing the address at all, parked or active.
    rows_remaining: i64,
}

/// Marks a host-interface address for preservation, force-deletes the host with
/// `release_preserved_addresses`, and reports what the RPC's teardown left
/// persisted for that address.
async fn force_delete_marked_host_address(
    env: &TestEnv,
    release_preserved_addresses: bool,
) -> MarkedAddressFate {
    let host = create_managed_host(env).await;

    let mut txn = env.pool.begin().await.unwrap();
    let machine = db::machine::find_one(txn.as_mut(), &host.id, MachineSearchConfig::default())
        .await
        .unwrap()
        .unwrap();
    let interface = &machine.status.interfaces[0];
    let interface_id = interface.id;
    let mac = interface.mac_address;
    // Mark the interface's existing IPv4 address (an interface holds at most one
    // address per family) for preservation.
    let existing = db::machine_interface_address::find_for_interface(txn.as_mut(), interface_id)
        .await
        .unwrap();
    let existing = existing
        .into_iter()
        .find(|a| a.address.is_ipv4())
        .expect("host interface fixture has an IPv4 address");
    let address = existing.address;
    let allocation_type = existing.allocation_type;
    assert!(
        db::machine_interface_address::mark_reserved(
            txn.as_mut(),
            interface_id,
            carbide_network::ip::IpAddressFamily::Ipv4,
        )
        .await
        .unwrap(),
        "the interface's IPv4 address should be marked for preservation"
    );
    txn.commit().await.unwrap();

    let response = host
        .api
        .admin_force_delete_machine(Request::new(AdminForceDeleteMachineRequest {
            allow_delete_with_instance_type: false,
            allow_delete_with_instance: false,
            host_query: host.id.to_string(),
            delete_interfaces: true,
            delete_bmc_interfaces: true,
            delete_bmc_credentials: false,
            allow_delete_with_orphaned_dpf_crds: false,
            delete_bmc_suppressions: false,
            delete_retained_boot_interfaces: false,
            release_preserved_addresses,
            wait_for_instance_dpu: false,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(response.all_done);

    let mut txn = env.pool.begin().await.unwrap();
    let parked =
        db::machine_interface_address::find_reserved(txn.as_mut(), Some(mac), Some(address))
            .await
            .unwrap();
    let rows_remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM machine_interface_addresses WHERE address = $1")
            .bind(address)
            .fetch_one(txn.as_mut())
            .await
            .unwrap();
    txn.commit().await.unwrap();

    MarkedAddressFate {
        mac,
        address,
        allocation_type,
        parked,
        rows_remaining,
    }
}

/// Default force-delete parks a marked interface address as a MAC-owned
/// reservation; the RPC's `release_preserved_addresses` flag instead removes it
/// outright. Proves the flag reaches interface teardown, which the DB-helper
/// test cannot show.
#[crate::sqlx_test]
async fn force_delete_parks_marked_address_unless_release_requested(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    // Default: the marked address survives as a parked reservation owned by the
    // interface's MAC.
    let parked_fate = force_delete_marked_host_address(&env, false).await;
    assert_eq!(
        parked_fate.parked,
        vec![db::machine_interface_address::ReservedAddress {
            address: parked_fate.address,
            reserved_by_mac: parked_fate.mac,
            allocation_type: parked_fate.allocation_type,
        }]
    );
    assert_eq!(parked_fate.rows_remaining, 1);

    // Explicit release: the marked address is deleted, leaving no reservation.
    let released_fate = force_delete_marked_host_address(&env, true).await;
    assert!(released_fate.parked.is_empty());
    assert_eq!(released_fate.rows_remaining, 0);
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
            release_preserved_addresses: false,
            wait_for_instance_dpu: false,
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
            release_preserved_addresses: false,
            wait_for_instance_dpu: false,
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

/// Two requests capture the same machine snapshots. Between their cleanup
/// commits, another machine acquires an interface and BMC address. The delayed
/// cleanup must leave the replacement interfaces and discovery records intact.
#[crate::sqlx_test]
async fn test_admin_force_delete_preserves_reassigned_resources(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let original = create_managed_host(&env).await;
    let replacement = create_managed_host(&env).await;
    let replacement_host_id: MachineId = replacement.id.into();
    let replacement_dpu_id: MachineId = replacement.dpu_ids[0].into();
    let original_host =
        db::machine::find_one(&env.pool, &original.id, MachineSearchConfig::default())
            .await
            .unwrap()
            .unwrap();
    let original_dpu = db::machine::find_one(
        &env.pool,
        &original.dpu_ids[0],
        MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let replacement_host = db::machine::find_one(
        &env.pool,
        &replacement_host_id,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let host_ip = original_host.status.bmc_info.ip.unwrap();
    let dpu_ip = original_dpu.status.bmc_info.ip.unwrap();
    let host_interface_id = original_host.status.interfaces[0].id;
    let dpu_interface_id = original_dpu.status.interfaces[0].id;
    let explored_host = ExploredManagedHost {
        host_bmc_ip: host_ip,
        dpus: Vec::new(),
    };
    let mut gate = env.pool.begin().await.unwrap();
    db::explored_managed_host::update(&mut gate, &[&explored_host])
        .await
        .unwrap();
    gate.commit().await.unwrap();

    // Queue first cleanup, replacement, then delayed cleanup on the same
    // advisory lock. Reaching this lock proves each RPC committed admission.
    let mut gate = env.pool.begin().await.unwrap();
    db::machine_interface::lock_all_admin_segments(&mut gate)
        .await
        .unwrap();
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(gate.as_mut())
        .await
        .unwrap();
    let mut first_request = force_delete_request(&original.id);
    first_request.delete_bmc_interfaces = true;
    let first_call = env
        .api
        .admin_force_delete_machine(Request::new(first_request));
    tokio::pin!(first_call);
    let first_pid = tokio::select! {
        result = &mut first_call => panic!("first cleanup passed the gate: {result:?}"),
        pid = wait_for_advisory_waiter(&env.pool, gate_pid, None) => pid,
    };

    let mut replacement_txn = env.pool.begin().await.unwrap();
    let replacement_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(replacement_txn.as_mut())
        .await
        .unwrap();
    let reassign = async {
        db::machine_interface::lock_all_admin_segments(&mut replacement_txn)
            .await
            .unwrap();
        assert!(env.find_machine(&original.id).await.is_empty());

        // The first request detached this interface. The replacement already
        // has a primary interface, so attach this one as a secondary.
        sqlx::query("UPDATE machine_interfaces SET primary_interface = false WHERE id = $1")
            .bind(host_interface_id)
            .execute(replacement_txn.as_mut())
            .await
            .unwrap();
        db::machine_interface::associate_interface_with_machine(
            &host_interface_id,
            MachineInterfaceAssociation::Machine(replacement_host_id),
            &mut replacement_txn,
        )
        .await
        .unwrap();

        // Move the replacement BMC to the freed address, with its own MAC
        // and a new interface ID. Leave the old DPU BMC address unassociated.
        db::machine_interface::delete(
            &replacement_host
                .status
                .bmc_info
                .machine_interface_id
                .unwrap(),
            &mut replacement_txn,
            false,
        )
        .await
        .unwrap();
        let segment = db::network_segment::for_static_address(&mut replacement_txn, host_ip)
            .await
            .unwrap();
        let bmc_interface = db::machine_interface::create_with_type(
            &mut replacement_txn,
            &[segment],
            &replacement_host.status.bmc_info.mac.unwrap(),
            false,
            AddressSelectionStrategy::StaticAddress(host_ip),
            InterfaceType::Bmc,
            None,
        )
        .await
        .unwrap();
        db::machine_interface::associate_bmc_interface(
            &bmc_interface.id,
            MachineInterfaceAssociation::Machine(replacement_host_id),
            &mut replacement_txn,
        )
        .await
        .unwrap();
        for (address, machine_id) in [(host_ip, replacement_host_id), (dpu_ip, replacement_dpu_id)]
        {
            db::explored_endpoints::insert(
                address,
                &EndpointExplorationReport {
                    machine_id: Some(machine_id),
                    ..Default::default()
                },
                false,
                &mut replacement_txn,
            )
            .await
            .unwrap();
        }
        // Add only this replacement host. `update` rewrites the whole table.
        sqlx::query("INSERT INTO explored_managed_hosts (host_bmc_ip, explored_dpus) VALUES ($1, '[]'::jsonb)")
            .bind(host_ip)
            .execute(replacement_txn.as_mut())
            .await
            .unwrap();

        replacement_txn.commit().await.unwrap();
        bmc_interface.id
    };
    tokio::pin!(reassign);
    tokio::select! {
        result = &mut first_call => panic!("first cleanup passed the gate: {result:?}"),
        _ = &mut reassign => panic!("replacement passed the first cleanup"),
        _ = wait_for_advisory_waiter(&env.pool, first_pid, Some(replacement_pid)) => {},
    }

    let mut delayed_request = force_delete_request(&original.id);
    delayed_request.delete_interfaces = true;
    delayed_request.delete_bmc_interfaces = true;
    let delayed_call = env
        .api
        .admin_force_delete_machine(Request::new(delayed_request));
    tokio::pin!(delayed_call);
    tokio::select! {
        result = &mut first_call => panic!("first cleanup passed the gate: {result:?}"),
        _ = &mut reassign => panic!("replacement passed the first cleanup"),
        result = &mut delayed_call => panic!("delayed cleanup passed replacement: {result:?}"),
        _ = wait_for_advisory_waiter(&env.pool, replacement_pid, None) => {},
    }
    gate.commit().await.unwrap();
    let (first_response, bmc_interface_id, delayed_response) =
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            tokio::join!(first_call, reassign, delayed_call)
        })
        .await
        .expect("both cleanups and replacement must complete in queue order");
    let first_response = first_response.unwrap().into_inner();
    assert!(first_response.all_done);
    assert!(first_response.host_bmc_interface_deleted);
    assert!(first_response.dpu_bmc_interface_deleted);
    let delayed_response = delayed_response.unwrap().into_inner();
    assert!(delayed_response.all_done);
    assert!(delayed_response.host_interfaces_deleted);
    assert!(delayed_response.dpu_interfaces_deleted);
    assert!(!delayed_response.host_bmc_interface_deleted);
    assert!(!delayed_response.dpu_bmc_interface_deleted);

    let mut txn = env.pool.begin().await.unwrap();
    let bmc_interface = db::machine_interface::find_by_ip(txn.as_mut(), host_ip)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bmc_interface.id, bmc_interface_id);
    assert_ne!(
        Some(bmc_interface.id),
        original_host.status.bmc_info.machine_interface_id
    );
    assert_eq!(bmc_interface.machine_id, Some(replacement_host_id));
    assert_eq!(
        Some(bmc_interface.mac_address),
        replacement_host.status.bmc_info.mac
    );
    for (interface_id, expected_owner) in [
        (host_interface_id, Some(replacement_host_id)),
        (dpu_interface_id, None),
    ] {
        let interface = db::machine_interface::find_one(txn.as_mut(), interface_id)
            .await
            .unwrap();
        assert_eq!(interface.machine_id, expected_owner);
    }
    assert!(
        db::machine_interface::find_by_ip(txn.as_mut(), dpu_ip)
            .await
            .unwrap()
            .is_none()
    );
    // No interface proves ownership at the DPU address. Its new exploration
    // report must survive that uncertainty, not be treated as the old DPU.
    let endpoints = db::explored_endpoints::find_by_ips(txn.as_mut(), vec![host_ip, dpu_ip])
        .await
        .unwrap();
    assert_eq!(endpoints.len(), 2);
    for (address, machine_id) in [(host_ip, replacement_host_id), (dpu_ip, replacement_dpu_id)] {
        let endpoint = endpoints
            .iter()
            .find(|endpoint| endpoint.address == address)
            .unwrap();
        assert_eq!(endpoint.report.machine_id, Some(machine_id));
    }
    let hosts = db::explored_managed_host::find_by_ips(txn.as_mut(), vec![host_ip])
        .await
        .unwrap();
    assert_eq!(hosts.len(), 1);
    txn.rollback().await.unwrap();
    assert!(env.find_machine(&original.id).await.is_empty());
    assert!(env.find_machine(&original.dpu_ids[0]).await.is_empty());
}

/// `wait_for_advisory_waiter` observes a specific database lock queue edge,
/// including a waiter blocked behind an earlier waiter rather than its holder.
async fn wait_for_advisory_waiter(
    pool: &sqlx::PgPool,
    blocker_pid: i32,
    waiter_pid: Option<i32>,
) -> i32 {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let pid = sqlx::query_scalar(
                "SELECT pid FROM pg_stat_activity
                 WHERE datname = current_database() AND wait_event_type = 'Lock'
                   AND $1 = ANY(pg_blocking_pids(pid))
                   AND ($2::integer IS NULL OR pid = $2)
                   AND strpos(query, 'pg_advisory_xact_lock') > 0",
            )
            .bind(blocker_pid)
            .bind(waiter_pid)
            .fetch_optional(pool)
            .await
            .unwrap();
            if let Some(pid) = pid {
                return pid;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("expected advisory lock queue edge did not appear")
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
        .admin_force_delete_machine(Request::new(force_delete_request(machine_id)))
        .await
        .unwrap()
        .into_inner()
}

async fn force_delete_with_network_ack(
    env: &TestEnv,
    machine_id: &MachineId,
) -> Result<tonic::Response<rpc::forge::AdminForceDeleteMachineResponse>, tonic::Status> {
    let response = env
        .api
        .admin_force_delete_machine(Request::new(AdminForceDeleteMachineRequest {
            wait_for_instance_dpu: true,
            ..force_delete_request(machine_id)
        }))
        .await?;
    if response.get_ref().all_done {
        return Ok(response);
    }

    let mut txn = env.pool.begin().await.unwrap();
    let snapshot = db::managed_host::load_snapshot(txn.as_mut(), machine_id, Default::default())
        .await
        .unwrap()
        .unwrap();
    let dpu_ids = snapshot.host_snapshot.associated_dpu_machine_ids();
    txn.commit().await.unwrap();
    for dpu_id in dpu_ids {
        network_configured_with_health(env, &dpu_id, None).await;
    }
    env.api
        .admin_force_delete_machine(Request::new(force_delete_request(machine_id)))
        .await
}

// Lifecycle fixtures authorize Instance cleanup; denial tests pass explicit requests.
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
        allow_delete_with_instance: true,
        release_preserved_addresses: false,
        wait_for_instance_dpu: false,
    }
}

/// Builds a DPF-enabled environment whose caller controls host-deletion outcomes so retry and
/// orphan policies share identical managed-host setup.
async fn dpf_force_delete_test_env(pool: sqlx::PgPool, mut mock: MockDpfOperations) -> TestEnv {
    // Supply DPF ingestion responses while leaving host-deletion behavior under caller control.
    mock.expect_get_service_versions_for_dpu().returning(|_| {
        Ok(vec![carbide_dpf::DpuServiceVersion {
            name: "test-service".to_string(),
            version: "test-version".to_string(),
            url: "https://example.com/test-service".to_string(),
        }])
    });
    mock.expect_register_dpu_device()
        .returning(|_, _, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_deployment_type_for_dpu()
        .returning(|__, _| Ok(DpuDeploymentType::Bf3));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    mock.expect_get_dpu_phase_observation()
        .returning(|_, _| Ok(phase_observation(carbide_dpf::DpuPhase::Ready)));

    // Enable the BF3 deployment required for the managed host to use DPF ingestion.
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

    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);
    create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await
}

/// Verifies force-delete rejects persisted service endpoints before its
/// irreversible Machine transition, so retries leave the normal cleanup owner
/// and the tenant instance intact.
#[crate::sqlx_test]
async fn force_delete_rejects_service_vpc_endpoints_before_state_transition(pool: sqlx::PgPool) {
    // Create an assigned host whose instance has durable endpoint ownership.
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let managed_host = create_managed_host(&env).await;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let endpoint =
        seed_service_vpc_endpoint(&env, instance.id, managed_host.dpu_ids[0], segment_id).await;

    // Both the first request and its retry must fail without exposing ForceDeletion.
    for attempt in [
        // The initial request proves the endpoint guard prevents entering ForceDeletion.
        "initial request",
        // The retry proves rollback leaves the same guard and ownership state intact.
        "retry",
    ] {
        let error = env
            .api
            .admin_force_delete_machine(Request::new(force_delete_request(&managed_host.id)))
            .await
            .expect_err(attempt);
        assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{attempt}");
        assert_eq!(
            error.message(),
            format!(
                "instance {} retains service VPC endpoints; wait for observed service-network cleanup before force deletion",
                instance.id
            ),
            "{attempt}"
        );

        // Reload after each rollback to catch a committed state or deletion marker.
        let mut txn = env.db_txn().await;
        let snapshot = managed_host.snapshot(&mut txn).await;
        assert_eq!(
            snapshot.managed_state,
            ManagedHostState::Assigned {
                instance_state: InstanceState::Ready,
            },
            "{attempt}"
        );
        let persisted = snapshot.instance.expect("instance remains assigned");
        assert!(persisted.deleted.is_none(), "{attempt}");
        assert_eq!(
            persisted.config.network.service_interfaces,
            vec![endpoint.clone()],
            "{attempt}"
        );
        txn.commit().await.unwrap();
    }
}

/// Verifies explicit orphan consent remains request-scoped through Admin acknowledgement,
/// preserving every Core-owned service-network record when a polling retry omits consent and
/// removing them without a tenant configuration update only after consent is repeated.
#[crate::sqlx_test]
async fn force_delete_orphan_override_removes_service_vpc_ownership_after_admin_ack(
    pool: sqlx::PgPool,
) {
    // Use distinct host and service segments so only service-endpoint cleanup can retire the latter.
    let env = create_test_env(pool).await;
    let (host_segment_id, service_segment_id) = env.create_vpc_and_dual_tenant_segment().await;
    let managed_host = create_managed_host(&env).await;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(host_segment_id)
        .build()
        .await;
    let endpoint = seed_service_vpc_endpoint(
        &env,
        instance.id,
        managed_host.dpu_ids[0],
        service_segment_id,
    )
    .await;

    // Persist the normalized address and exact DPU/VPC loopback so the override must release them
    // and mark the service segment instead of only deleting the Instance JSON.
    let service_address: IpAddr = "192.0.2.1".parse().unwrap();
    let mut txn = env.db_txn().await;
    sqlx::query(
        "INSERT INTO instance_addresses \
            (instance_id, address, prefix, segment_id, vpc_id) \
         VALUES ($1, $2, '192.0.2.0/31'::cidr, $3, $4)",
    )
    .bind(instance.id)
    .bind(service_address)
    .bind(service_segment_id)
    .bind(endpoint.vpc_id)
    .execute(txn.as_mut())
    .await
    .unwrap();
    let service_loopback = db::vpc_dpu_loopback::get_or_allocate_loopback_ip_for_vpc(
        &env.common_pools,
        &mut txn,
        &endpoint.dpu_id,
        &endpoint.vpc_id,
    )
    .await
    .unwrap();
    assert_eq!(
        db::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &instance.id,
            &service_segment_id,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|address| address.address)
        .collect::<Vec<_>>(),
        vec![service_address],
    );
    txn.commit().await.unwrap();

    let orphan_request = || AdminForceDeleteMachineRequest {
        allow_delete_with_orphaned_dpf_crds: true,
        wait_for_instance_dpu: true,
        ..force_delete_request(&managed_host.id)
    };

    // The first request publishes Admin but retains all ownership until the DPU acknowledges it.
    let waiting = env
        .api
        .admin_force_delete_machine(Request::new(orphan_request()))
        .await
        .unwrap()
        .into_inner();
    assert!(!waiting.all_done);
    let mut txn = env.db_txn().await;
    let snapshot = managed_host.snapshot(&mut txn).await;
    assert_eq!(snapshot.managed_state, ManagedHostState::ForceDeletion);
    assert!(snapshot.use_admin_network());
    assert!(!snapshot.managed_host_network_config_version_synced());
    let retained = snapshot
        .instance
        .expect("Admin wait must retain the Instance");
    assert_eq!(
        retained.config.network.service_interfaces,
        vec![endpoint.clone()],
    );
    assert_eq!(
        db::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &instance.id,
            &service_segment_id,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|address| address.address)
        .collect::<Vec<_>>(),
        vec![service_address],
    );
    assert!(
        db::vpc_dpu_loopback::find(&mut txn, &endpoint.dpu_id, &endpoint.vpc_id)
            .await
            .unwrap()
            .is_some(),
    );
    txn.commit().await.unwrap();

    // Acknowledging Admin satisfies the independently persisted wait requirement.
    network_configured_with_health(&env, &managed_host.dpu_ids[0], None).await;

    // Omitting orphan consent after acknowledgement must reject this polling request without
    // releasing the endpoint, normalized address, or exact DPU/VPC loopback allocation.
    let error = env
        .api
        .admin_force_delete_machine(Request::new(AdminForceDeleteMachineRequest {
            wait_for_instance_dpu: true,
            ..force_delete_request(&managed_host.id)
        }))
        .await
        .expect_err("orphan consent must be repeated on every polling request");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        format!(
            "instance {} retains service VPC endpoints; wait for observed service-network cleanup before force deletion",
            instance.id
        ),
    );

    // Reload every ownership record to prove the rejected request remained fail-closed.
    let mut txn = env.db_txn().await;
    let snapshot = managed_host.snapshot(&mut txn).await;
    assert_eq!(snapshot.managed_state, ManagedHostState::ForceDeletion);
    assert!(snapshot.use_admin_network());
    assert!(snapshot.managed_host_network_config_version_synced());
    assert_eq!(
        snapshot
            .instance
            .expect("rejected polling request must retain the Instance")
            .config
            .network
            .service_interfaces,
        vec![endpoint.clone()],
    );
    assert_eq!(
        db::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &instance.id,
            &service_segment_id,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|address| address.address)
        .collect::<Vec<_>>(),
        vec![service_address],
    );
    let retained_loopback =
        db::vpc_dpu_loopback::find(&mut txn, &endpoint.dpu_id, &endpoint.vpc_id)
            .await
            .unwrap()
            .expect("rejected polling request must retain the DPU/VPC loopback");
    assert_eq!(retained_loopback.loopback_ip, service_loopback);
    txn.commit().await.unwrap();
    let loopback_allocation =
        db::resource_pool::find_value(&env.pool, &service_loopback.to_string())
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.pool_name == VPC_DPU_LOOPBACK)
            .expect("service loopback remains represented in its pool");
    assert_eq!(
        loopback_allocation.state.0,
        ResourcePoolEntryState::Allocated {
            owner: endpoint.dpu_id.to_string(),
            owner_type: OwnerType::Machine.to_string(),
        },
    );

    // Repeating orphan consent allows the acknowledged force deletion to complete.
    let completed = env
        .api
        .admin_force_delete_machine(Request::new(orphan_request()))
        .await
        .unwrap()
        .into_inner();
    assert!(completed.all_done);
    assert!(
        db::instance::find_by_id(&env.pool, instance.id)
            .await
            .unwrap()
            .is_none(),
    );
    let mut txn = env.db_txn().await;
    assert!(
        db::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &instance.id,
            &service_segment_id,
        )
        .await
        .unwrap()
        .is_empty(),
    );
    assert!(
        db::vpc_dpu_loopback::find(&mut txn, &endpoint.dpu_id, &endpoint.vpc_id)
            .await
            .unwrap()
            .is_none(),
    );
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT deleted IS NOT NULL FROM network_segments WHERE id = $1",
        )
        .bind(service_segment_id)
        .fetch_one(txn.as_mut())
        .await
        .unwrap(),
    );
    txn.commit().await.unwrap();
    let service_loopback_value = service_loopback.to_string();
    let loopback_allocation = db::resource_pool::find_value(&env.pool, &service_loopback_value)
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.pool_name == VPC_DPU_LOOPBACK)
        .expect("service loopback remains represented in its pool");
    assert_eq!(loopback_allocation.state.0, ResourcePoolEntryState::Free);
    for machine_id in [managed_host.id.into(), managed_host.dpu_ids[0].into()] {
        validate_machine_deletion(&env, &machine_id, None).await;
    }
}

/// Verifies the post-ForceDeletion reread rejects an endpoint committed by a
/// pre-existing Instance writer, because observed service-network cleanup must
/// retain a live Instance even after the Machine transition commits.
#[crate::sqlx_test]
async fn force_delete_rejects_endpoint_committed_after_state_transition(pool: sqlx::PgPool) {
    // Create a coherent attachment and endpoint, then retain only the attachment
    // so the early Machine-level guard observes no committed membership.
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let managed_host = create_managed_host(&env).await;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let endpoint =
        seed_service_vpc_endpoint(&env, instance.id, managed_host.dpu_ids[0], segment_id).await;
    let mut baseline_txn = env.db_txn().await;
    let seeded = db::instance::find_by_id(baseline_txn.as_mut(), instance.id)
        .await
        .unwrap()
        .expect("seeded Instance remains stored");
    let mut network_without_endpoint = seeded.config.network.clone();
    network_without_endpoint.service_interfaces.clear();
    db::instance::update_network_config(
        baseline_txn.as_mut(),
        instance.id,
        seeded.network_config_version,
        &network_without_endpoint,
        false,
    )
    .await
    .unwrap();
    baseline_txn.commit().await.unwrap();

    // Lock the Instance and stage the endpoint without committing it. Force-delete
    // can pass its unlocked early read but must wait at the post-transition reread.
    let mut writer_txn = env.db_txn().await;
    db::instance::find_by_id_for_update(writer_txn.as_mut(), instance.id)
        .await
        .unwrap()
        .expect("fixture Instance must exist to be locked");
    let current = db::instance::find_by_id(writer_txn.as_mut(), instance.id)
        .await
        .unwrap()
        .expect("locked Instance remains stored");
    assert!(
        current.config.network.service_interfaces.is_empty(),
        "the committed baseline must not trip the early endpoint guard",
    );
    let mut network_with_endpoint = current.config.network.clone();
    network_with_endpoint
        .service_interfaces
        .push(endpoint.clone());
    db::instance::update_network_config(
        writer_txn.as_mut(),
        instance.id,
        current.network_config_version,
        &network_with_endpoint,
        false,
    )
    .await
    .unwrap();

    // Start through the public RPC and wait until ForceDeletion has committed and
    // the late guard is blocked acquiring the writer-owned Instance row.
    let api = managed_host.api.clone();
    let host_id = managed_host.id;
    let force_delete_task = tokio::spawn(async move {
        api.admin_force_delete_machine(Request::new(force_delete_request(&host_id)))
            .await
    });
    wait_until_blocked_on(&env.pool, "FOR UPDATE OF i").await;

    // Publish the endpoint into the late reread, which must stop before marking
    // the Instance deleted or starting ownership cleanup.
    writer_txn.commit().await.unwrap();
    let error = force_delete_task
        .await
        .expect("force-delete task must complete")
        .expect_err("the post-transition endpoint guard must reject force-delete");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        format!(
            "instance {} retains service VPC endpoints; wait for observed service-network cleanup before force deletion",
            instance.id,
        ),
    );

    // Reload persisted state. ForceDeletion proves the early guard passed, while
    // a live Instance proves the late guard ran before the deletion marker.
    let mut txn = env.db_txn().await;
    let snapshot = managed_host.snapshot(&mut txn).await;
    assert_eq!(
        snapshot.managed_state,
        ManagedHostState::ForceDeletion,
        "the Machine transition must commit before the guarded Instance reread",
    );
    let persisted = snapshot
        .instance
        .expect("the late guard must retain the Instance");
    assert!(
        persisted.deleted.is_none(),
        "the late guard must stop before marking the Instance deleted",
    );
    assert_eq!(
        persisted.config.network.service_interfaces,
        vec![endpoint],
        "the concurrent endpoint must remain owned by the live Instance",
    );
    txn.commit().await.unwrap();
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

fn validate_delete_response<H, D>(
    response: &rpc::forge::AdminForceDeleteMachineResponse,
    host_machine_id: Option<&H>,
    dpu_machine_id: &D,
) where
    H: MachineIdSubtypeTrait,
    D: MachineIdSubtypeTrait,
{
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

fn validate_delete_response_multi_dpu<H, D>(
    response: &rpc::forge::AdminForceDeleteMachineResponse,
    host_machine_id: Option<&H>,
    dpu_machine_ids: &[D],
) where
    H: MachineIdSubtypeTrait,
    D: MachineIdSubtypeTrait,
{
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
    use std::time::Duration;

    use futures_util::future::maybe_done;
    use tokio::time::timeout;

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
    let allocation_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(allocation_txn.as_mut())
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
            release_preserved_addresses: false,
            wait_for_instance_dpu: false,
        }))
        .await
    });
    let abort_force_delete = force_delete_task.abort_handle();
    let force_delete_task = maybe_done(force_delete_task);
    tokio::pin!(force_delete_task);
    let readiness = tokio::select! {
        () = &mut force_delete_task => Err("force-delete finished before its host-lock wait".to_string()),
        observed = observe_force_delete_state_lock_wait(&env.pool, Some(allocation_pid), None) => observed,
    };

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
    let instance_write = db::instance::batch_persist(
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
    .await;
    let allocation_released = if instance_write.is_ok() {
        allocation_txn.commit().await
    } else {
        allocation_txn.rollback().await
    };
    let timed_out = timeout(Duration::from_secs(30), &mut force_delete_task)
        .await
        .is_err();
    if timed_out {
        abort_force_delete.abort();
        timeout(Duration::from_secs(30), &mut force_delete_task)
            .await
            .expect("cancelled force-delete must join after allocation gate release");
    }
    let result = force_delete_task
        .as_mut()
        .take_output()
        .expect("force-delete task joined");
    tracing::info!(
        ?allocation_pid,
        ?readiness,
        ?allocation_released,
        ?result,
        "Joined force-delete before reporting allocation-race readiness"
    );
    allocation_released.expect("release the allocation host gate");
    let (_, query, _) = readiness.expect("observe force-delete behind the actual allocation");
    assert!(
        query.contains("SELECT id FROM machines WHERE id = $1 FOR UPDATE"),
        "{query}"
    );
    instance_write.expect("the allocation fixture must persist its concurrent Instance");
    assert!(
        !timed_out,
        "force-delete exceeded its released-gate completion timeout"
    );
    let error = result
        .expect("force-delete task completes normally")
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
    db::instance::find_by_id_for_update(writer_txn.as_mut(), instance.id)
        .await
        .unwrap()
        .expect("fixture Instance must exist to be locked");
    let initial = db::instance::find_by_id(writer_txn.as_mut(), instance.id)
        .await
        .unwrap()
        .unwrap();
    let mut updated_metadata = initial.metadata.clone();
    updated_metadata.description = "committed before force-delete".to_string();
    // Hold the Instance lock while force-delete captures its initial config.
    // Commit the final config writes only once deletion reaches the marker lock.
    let mut requested_network = initial.config.network.clone();
    let interface = &mut requested_network.interfaces[0];
    interface.network_details = Some(NetworkDetails::VpcPrefixId(VpcPrefixId::new()));
    interface.network_segment_id = Some(generated_segment_id);
    let mut address_guard = env.pool.begin().await.unwrap();
    lock_instance_address(address_guard.as_mut(), instance.id).await;

    let machine_id = managed_host.id.into();
    let force_delete = force_delete_with_network_ack(&env, &machine_id);
    let orchestrate = async {
        wait_until_blocked_on(&env.pool, "FOR UPDATE OF i").await;
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

    let machine_id = managed_host.id.into();
    let force_delete = force_delete_with_network_ack(&env, &machine_id);
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
    assert_eq!(check_instance.machine_id(), mh.id);
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

    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut instance_guard = env.pool.begin().await.unwrap();
        db::instance::find_by_id_for_update(instance_guard.as_mut(), tinstance.id)
            .await
            .unwrap()
            .expect("the failed cleanup must retain the Instance");
        assert_eq!(
            sqlx::query_scalar::<_, Option<bool>>(
                "SELECT force_delete_requires_admin_ack FROM machines WHERE id = $1",
            )
            .bind(mh.id)
            .fetch_one(instance_guard.as_mut())
            .await
            .unwrap(),
            Some(false),
        );
        let addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
            instance_guard.as_mut(),
            &tinstance.id,
            &segment_id,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|address| (address.address, address.prefix, address.vpc_id))
        .collect::<HashSet<_>>();
        assert!(!addresses.is_empty());

        // Pause the default request after it commits admission. A later
        // opt-in must still prevent this request from deleting the Instance.
        let deleting = env
            .api
            .admin_force_delete_machine(Request::new(force_delete_request(&mh.id)));
        tokio::pin!(deleting);
        tokio::select! {
            result = &mut deleting => panic!("default request passed the Instance lock: {result:?}"),
            _ = wait_until_blocked_on(&env.pool, "FOR UPDATE OF i") => {},
        }

        // The first routing lock has been released. Queue the opt-in ahead
        // of the default request's final cleanup transaction.
        let mut routing_guard = env.pool.begin().await.unwrap();
        db::tenant_prefix_overlap::lock_checks(&mut routing_guard)
            .await
            .unwrap();
        let routing_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(routing_guard.as_mut())
            .await
            .unwrap();
        let opting_in = env.api.admin_force_delete_machine(Request::new(
            AdminForceDeleteMachineRequest {
                wait_for_instance_dpu: true,
                ..force_delete_request(&mh.id)
            },
        ));
        tokio::pin!(opting_in);
        let opt_in_pid = tokio::select! {
            result = &mut deleting => panic!("default request passed the Instance lock: {result:?}"),
            result = &mut opting_in => panic!("opt-in request passed the routing lock: {result:?}"),
            pid = wait_for_advisory_waiter(&env.pool, routing_pid, None) => pid,
        };

        // Let UFM cleanup finish, then verify its final transaction waits
        // behind the opt-in before releasing the routing lock.
        instance_guard.commit().await.unwrap();
        tokio::select! {
            result = &mut deleting => panic!("default cleanup passed the queued opt-in: {result:?}"),
            result = &mut opting_in => panic!("opt-in request passed the routing lock: {result:?}"),
            _ = wait_for_advisory_waiter(&env.pool, opt_in_pid, None) => {},
        }
        routing_guard.commit().await.unwrap();
        let (deleting, opting_in) = tokio::join!(deleting, opting_in);
        for (request, response) in [("default", deleting), ("opt-in", opting_in)] {
            assert!(
                !response.unwrap().into_inner().all_done,
                "{request} request must wait for the Admin acknowledgement",
            );
        }

        let mut txn = env.pool.begin().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, Option<bool>>(
                "SELECT force_delete_requires_admin_ack FROM machines WHERE id = $1",
            )
            .bind(mh.id)
            .fetch_one(txn.as_mut())
            .await
            .unwrap(),
            Some(true),
        );
        let snapshot = mh.snapshot(&mut txn).await;
        assert_eq!(snapshot.managed_state, ManagedHostState::ForceDeletion);
        assert!(snapshot.use_admin_network());
        assert!(!snapshot.managed_host_network_config_version_synced());
        assert_eq!(snapshot.dpu_snapshots.len(), mh.dpu_ids.len());
        let retained = snapshot.instance.as_ref().unwrap();
        assert_eq!(retained.id, tinstance.id);
        assert_eq!(retained.deleted.as_ref(), Some(&deleted_at));
        let retained_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &tinstance.id,
            &segment_id,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|address| (address.address, address.prefix, address.vpc_id))
        .collect::<HashSet<_>>();
        assert_eq!(retained_addresses, addresses);
        txn.commit().await.unwrap();

        mh.network_configured(&env).await;
    })
    .await
    .expect("concurrent force-delete requests must retain the Instance until Admin is acknowledged");

    let response = force_delete(&env, &mh.id).await;
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
                machine_ids: dpu_ids.iter().copied().map(Into::into).collect(),
                ..Default::default()
            }))
            .await
            .is_ok_and(|response| response.into_inner().machines.len() == 2),
        "Expected to find 2 dpu machines when looking up by ID"
    );

    // Delete the *host* machine
    let response = force_delete(&env, &mh.host().id).await;

    validate_delete_response_multi_dpu(&response, Some(&mh.host().id), dpu_ids.as_slice());

    validate_machine_deletion(&env, &mh.host().id, None).await;
    for id in &dpu_ids {
        validate_machine_deletion(&env, id, None).await;
    }
}

/// Default cleanup and a fresh request without an Instance do not require an
/// acknowledgement, even when the DPU is offline with tenant networking requested.
#[crate::sqlx_test]
async fn force_delete_without_an_instance_wait_requirement_cleans_up_offline(pool: sqlx::PgPool) {
    struct CleanupCase {
        delete_instance_first: bool,
        wait_for_instance_dpu: bool,
    }

    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let cases = [
        Case {
            scenario: "an assigned host does not wait unless requested",
            input: CleanupCase {
                delete_instance_first: false,
                wait_for_instance_dpu: false,
            },
            expect: Yields(true),
        },
        Case {
            scenario: "an opted-in request without an Instance ignores leftover tenant intent",
            input: CleanupCase {
                delete_instance_first: true,
                wait_for_instance_dpu: true,
            },
            expect: Yields(true),
        },
    ];

    check_cases_async(cases, |case| {
        let env = &env;
        async move {
            let managed_host = create_managed_host(env).await;
            let instance = managed_host
                .instance_builer(env)
                .single_interface_network_config(segment_id)
                .build()
                .await;
            let mut txn = env.pool.begin().await.unwrap();
            if case.delete_instance_first {
                db::instance::delete(instance.id, txn.as_mut())
                    .await
                    .unwrap();
            }
            sqlx::query("UPDATE machines SET network_status_observation = NULL WHERE id = $1")
                .bind(managed_host.dpu_ids[0])
                .execute(txn.as_mut())
                .await
                .unwrap();
            let snapshot = managed_host.snapshot(&mut txn).await;
            assert_eq!(snapshot.instance.is_none(), case.delete_instance_first);
            assert!(!snapshot.use_admin_network());
            assert!(!snapshot.managed_host_network_config_version_synced());
            txn.commit().await.unwrap();

            let response = env
                .api
                .admin_force_delete_machine(Request::new(AdminForceDeleteMachineRequest {
                    wait_for_instance_dpu: case.wait_for_instance_dpu,
                    ..force_delete_request(&managed_host.id)
                }))
                .await
                .unwrap()
                .into_inner();
            assert!(
                db::instance::find_by_id(&env.pool, instance.id)
                    .await
                    .unwrap()
                    .is_none()
            );
            for machine_id in [managed_host.id.into(), managed_host.dpu_ids[0].into()] {
                validate_machine_deletion(env, &machine_id, None).await;
            }
            Ok::<_, ()>(response.all_done)
        }
    })
    .await;
}

// Pause the real FD Admin-group write after H/interface/D admission. The
// gate transaction owns only an advisory, so it cannot reject D admission.
async fn gate_force_delete_group_write(
    pool: &sqlx::PgPool,
    dpu_id: carbide_uuid::machine::DpuMachineId,
) -> Result<sqlx::PgTransaction<'static>, sqlx::Error> {
    let statement = format!(
        r#"
        CREATE FUNCTION pause_force_delete_group_write()
        RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            PERFORM pg_advisory_xact_lock(5333, 51);
            RETURN NEW;
        END;
        $$;
        CREATE TRIGGER pause_force_delete_group_write
        BEFORE UPDATE OF network_config_version ON machines
        FOR EACH ROW WHEN (OLD.id = '{dpu_id}')
        EXECUTE FUNCTION pause_force_delete_group_write();
        "#
    );
    // Only the canonical typed DPU ID is interpolated into test DDL.
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
        .execute(pool)
        .await?;
    let mut gate = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(5333, 51)")
        .execute(gate.as_mut())
        .await?;
    Ok(gate)
}

async fn remove_force_delete_group_write_gate(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        "DROP TRIGGER pause_force_delete_group_write ON machines;
         DROP FUNCTION pause_force_delete_group_write();",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Force-delete releases its entire routing and Machine prefix before waiting
/// for history; normal state persistence takes history before its Machine row.
#[crate::sqlx_test]
async fn force_delete_and_controller_state_persistence_preserve_history_order(pool: sqlx::PgPool) {
    #[derive(Clone, Copy, Debug)]
    enum FirstWriter {
        Controller,
        ForceDelete,
    }

    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mut failures = Vec::new();
    for first_writer in [FirstWriter::Controller, FirstWriter::ForceDelete] {
        let managed_host = create_managed_host(&env).await;
        let instance = managed_host
            .instance_builer(&env)
            .single_interface_network_config(segment_id)
            .build()
            .await;
        let host_id = managed_host.id;
        for dpu_id in &managed_host.dpu_ids {
            network_configured_with_health(&env, dpu_id, None).await;
        }
        let failure = model::machine::FailureDetails {
            cause: model::machine::FailureCause::NVMECleanFailed {
                err: "state-history competitor".into(),
            },
            source: model::machine::FailureSource::Scout,
            failed_at: chrono::Utc::now(),
        };
        let mut txn = env.pool.begin().await.unwrap();
        for machine_id in managed_host
            .dpu_ids
            .iter()
            .copied()
            .map(MachineId::from)
            .chain([host_id.into()])
        {
            sqlx::query(
                "UPDATE machines SET reset_requested = NULL,
                    reprovisioning_requested = NULL, host_reprovisioning_requested = NULL,
                    last_reboot_requested = NULL WHERE id = $1",
            )
            .bind(machine_id)
            .execute(txn.as_mut())
            .await
            .unwrap();
        }
        db::machine::update_failure_details_by_machine_id(
            &host_id.into(),
            txn.as_mut(),
            failure.clone(),
        )
        .await
        .unwrap();
        let before = managed_host.snapshot(&mut txn).await;
        assert_eq!(
            before.managed_state,
            ManagedHostState::Assigned {
                instance_state: InstanceState::Ready,
            },
        );
        assert_eq!(before.host_snapshot.status.failure_details, failure);
        assert!(before.host_snapshot.reset_requested.is_none());
        assert!(before.host_snapshot.host_reprovision_requested.is_none());
        assert!(before.host_snapshot.status.last_reboot_requested.is_none());
        for dpu in &before.dpu_snapshots {
            assert!(dpu.reprovision_requested.is_none());
            assert!(dpu.status.last_reboot_requested.is_none());
            let observation = dpu
                .network_status_observation
                .as_ref()
                .expect("fixture DPU has a fresh heartbeat");
            assert!(
                chrono::Utc::now().signed_duration_since(observation.observed_at)
                    < chrono::Duration::minutes(1),
            );
        }
        assert!(!before.use_admin_network());
        assert!(before.managed_host_network_config_version_synced());
        let addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &instance.id,
            &segment_id,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|address| (address.address, address.prefix, address.vpc_id))
        .collect::<Vec<_>>();
        assert!(!addresses.is_empty());
        txn.commit().await.unwrap();

        let mut controller_txn = env.pool.begin().await.unwrap();
        let controller_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(controller_txn.as_mut())
            .await
            .unwrap();
        let mut group_gate = None;
        let blocker_pid = match first_writer {
            FirstWriter::Controller => {
                // Pause the real state writer at advance's history-retention boundary.
                sqlx::query(
                    "SELECT pg_advisory_xact_lock(\
                        hashtextextended($1, 'machine_state_history'::regclass::bigint)\
                    )",
                )
                .bind(host_id.to_string())
                .execute(controller_txn.as_mut())
                .await
                .unwrap();
                controller_pid
            }
            FirstWriter::ForceDelete => {
                let mut gate = gate_force_delete_group_write(&env.pool, managed_host.dpu_ids[0])
                    .await
                    .unwrap();
                let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(gate.as_mut())
                    .await
                    .unwrap();
                group_gate = Some(gate);
                pid
            }
        };

        let api = env.api.clone();
        let mut deletion = tokio::spawn(async move {
            api.admin_force_delete_machine(Request::new(AdminForceDeleteMachineRequest {
                wait_for_instance_dpu: true,
                ..force_delete_request(&host_id)
            }))
            .await
        });
        let deletion_wait =
            observe_force_delete_state_lock_wait(&env.pool, Some(blocker_pid), None).await;
        let prefix_probe = if matches!(first_writer, FirstWriter::Controller) {
            Some(async {
                let mut probe = env.pool.begin().await
                    .map_err(|error| format!("prefix probe begin: {error:?}"))?;
                let routing = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    sqlx::query_scalar::<_, bool>(
                        "SELECT pg_try_advisory_xact_lock(
                            hashtextextended('tenant_prefix_overlap:checks', 0)
                        )",
                    )
                    .fetch_one(probe.as_mut()),
                ).await;
                let host = if matches!(routing, Ok(Ok(true))) {
                    Some(sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
                        .bind(host_id).execute(probe.as_mut()).await)
                } else {
                    None
                };
                let rollback = probe.rollback().await;
                if !matches!(routing, Ok(Ok(true)))
                    || !matches!(host, Some(Ok(_)))
                    || rollback.is_err()
                {
                    return Err(format!("retained force-delete prefix: routing={routing:?}; host={host:?}; rollback={rollback:?}"));
                }
                Ok::<(), String>(())
            }.await)
        } else {
            None
        };
        let expected_version = before.host_snapshot.state.version;
        let controller_version = expected_version.increment();
        // Global failure parking returns this plain transition from Assigned Ready,
        // without an earlier Machine write. Drive its SQL owner against the real RPC.
        let controller_state = ManagedHostState::Assigned {
            instance_state: InstanceState::Failed {
                details: before.host_snapshot.status.failure_details.clone(),
                machine_id: host_id.into(),
            },
        };
        let state_for_writer = controller_state.clone();
        let mut controller = tokio::spawn(async move {
            let result = db::machine::try_update_controller_state(
                controller_txn.as_mut(),
                &host_id,
                expected_version,
                controller_version,
                &state_for_writer,
            )
            .await;
            match result {
                Ok(applied) => controller_txn
                    .commit()
                    .await
                    .map(|()| applied)
                    .map_err(|error| format!("controller commit: {error:?}")),
                Err(error) => {
                    let rollback = controller_txn.rollback().await;
                    Err(format!(
                        "controller write: {error:?}; rollback: {rollback:?}"
                    ))
                }
            }
        });
        let controller_wait = if group_gate.is_some() {
            Some(observe_force_delete_state_lock_wait(&env.pool, None, Some(controller_pid)).await)
        } else {
            None
        };
        let gate_release = match group_gate {
            Some(gate) => gate.rollback().await,
            None => Ok(()),
        };

        // Either database session can be the deadlock victim. Release the gate and
        // collect both results before asserting, including cancellation on timeout.
        let (deletion_result, controller_result) = tokio::join!(
            tokio::time::timeout(std::time::Duration::from_secs(60), &mut deletion),
            tokio::time::timeout(std::time::Duration::from_secs(60), &mut controller),
        );
        if deletion_result.is_err() {
            deletion.abort();
            deletion.await.ok();
        }
        if controller_result.is_err() {
            controller.abort();
            controller.await.ok();
        }
        let group_gate_cleanup = if matches!(first_writer, FirstWriter::ForceDelete) {
            remove_force_delete_group_write_gate(&env.pool).await
        } else {
            Ok(())
        };
        let diagnostic = format!(
            "{first_writer:?}: deletion_wait={deletion_wait:?}; prefix_probe={prefix_probe:?}; \
             controller_wait={controller_wait:?}; gate_release={gate_release:?}; \
             group_gate_cleanup={group_gate_cleanup:?}; \
             deletion={deletion_result:?}; controller={controller_result:?}"
        );
        let (Ok(Ok(Ok(response))), Ok(Ok(Ok(applied)))) = (deletion_result, controller_result)
        else {
            failures.push(diagnostic);
            continue;
        };
        assert!(
            gate_release.is_ok() && group_gate_cleanup.is_ok(),
            "{diagnostic}"
        );
        let (_, deletion_query, deletion_advisory_wait) = deletion_wait.expect(&diagnostic);
        match first_writer {
            FirstWriter::Controller => {
                assert!(matches!(prefix_probe, Some(Ok(()))), "{diagnostic}");
                assert!(deletion_advisory_wait, "{diagnostic}");
                assert!(
                    deletion_query.contains("machine_state_history")
                        || deletion_query.contains("pg_advisory_xact_lock"),
                    "{diagnostic}",
                );
                assert!(
                    matches!(applied, db::ConditionalWrite::Applied(_)),
                    "{diagnostic}",
                );
            }
            FirstWriter::ForceDelete => {
                assert!(deletion_advisory_wait, "{diagnostic}");
                assert!(
                    deletion_query.contains("machine_group_member_ids"),
                    "{diagnostic}",
                );
                let (_, controller_query, waits_for_history) = controller_wait
                    .expect("controller wait was observed")
                    .expect(&diagnostic);
                assert!(waits_for_history, "{diagnostic}");
                assert!(
                    controller_query.contains("machine_state_history"),
                    "{diagnostic}",
                );
                assert_eq!(
                    applied,
                    db::ConditionalWrite::NotApplied(db::ControllerStateNotCurrent),
                    "{diagnostic}",
                );
            }
        }
        assert!(!response.get_ref().all_done, "{diagnostic}");
        assert_eq!(response.get_ref().instance_id, instance.id.to_string());
        let mut txn = env.pool.begin().await.unwrap();
        let after = managed_host.snapshot(&mut txn).await;
        assert_eq!(after.managed_state, ManagedHostState::ForceDeletion);
        assert_eq!(after.host_snapshot.status.failure_details, failure);
        assert!(after.use_admin_network());
        assert!(!after.managed_host_network_config_version_synced());
        assert_eq!(
            after.host_snapshot.network_config.version.version_nr(),
            before.host_snapshot.network_config.version.version_nr() + 1,
        );
        assert_eq!(
            after.host_snapshot.state.version.version_nr(),
            expected_version.version_nr()
                + 1
                + u64::from(matches!(first_writer, FirstWriter::Controller)),
        );
        assert_eq!(after.dpu_snapshots.len(), before.dpu_snapshots.len());
        for dpu in &after.dpu_snapshots {
            assert_eq!(dpu.state.value, ManagedHostState::ForceDeletion);
            assert_eq!(
                dpu.state.version.version_nr(),
                after.host_snapshot.state.version.version_nr(),
            );
        }
        for (machine_id, state_version) in after
            .dpu_snapshots
            .iter()
            .map(|dpu| (MachineId::from(dpu.id), dpu.state.version))
            .chain([(host_id.into(), after.host_snapshot.state.version)])
        {
            let history = db::state_history::for_object(
                txn.as_mut(),
                db::state_history::StateHistoryTableId::Machine,
                &machine_id,
            )
            .await
            .unwrap();
            let deletion_history = history
                .iter()
                .filter(|record| {
                    serde_json::from_str::<ManagedHostState>(&record.state).unwrap()
                        == ManagedHostState::ForceDeletion
                })
                .collect::<Vec<_>>();
            assert_eq!(deletion_history.len(), 1);
            assert_eq!(deletion_history[0].state_version, state_version);
            assert_eq!(
                history
                    .iter()
                    .filter(|record| record.state_version == controller_version)
                    .count(),
                usize::from(matches!(first_writer, FirstWriter::Controller)),
                "stale controller history must roll back for {machine_id}",
            );
            for record in history
                .iter()
                .filter(|record| record.state_version == controller_version)
            {
                assert_eq!(
                    serde_json::from_str::<ManagedHostState>(&record.state).unwrap(),
                    controller_state,
                );
            }
        }
        assert_eq!(
            sqlx::query_scalar::<_, Option<bool>>(
                "SELECT force_delete_requires_admin_ack FROM machines WHERE id = $1",
            )
            .bind(host_id)
            .fetch_one(txn.as_mut())
            .await
            .unwrap(),
            Some(true),
        );
        let retained = after.instance.as_ref().unwrap();
        assert_eq!(retained.id, instance.id);
        assert!(retained.deleted.is_none());
        let retained_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &instance.id,
            &segment_id,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|address| (address.address, address.prefix, address.vpc_id))
        .collect::<Vec<_>>();
        assert_eq!(retained_addresses, addresses);
        assert!(
            sqlx::query_scalar::<_, bool>(
                "SELECT deleted IS NULL FROM network_segments WHERE id = $1",
            )
            .bind(segment_id)
            .fetch_one(txn.as_mut())
            .await
            .unwrap(),
        );
        txn.commit().await.unwrap();
        for dpu_id in &managed_host.dpu_ids {
            let config = env
                .api
                .get_managed_host_network_config(Request::new(
                    rpc::forge::ManagedHostNetworkConfigRequest {
                        dpu_machine_id: Some(*dpu_id),
                    },
                ))
                .await
                .unwrap()
                .into_inner();
            assert!(config.use_admin_network);
            assert!(config.tenant_interfaces.is_empty());
            assert_eq!(
                config.managed_host_config_version,
                after.host_snapshot.network_config.version.to_string(),
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[crate::sqlx_test]
async fn force_delete_and_dpu_network_status_release_opposing_prefixes(pool: sqlx::PgPool) {
    use std::time::{Duration, SystemTime};

    use carbide_uuid::machine::MachineInterfaceId;
    use db::test_support::postgres::wait_for_blocked_query;
    use futures_util::future::maybe_done;
    use tokio::time::timeout;

    #[derive(Clone, Copy, Debug)]
    enum FirstWriter {
        Report,
        ForceDelete,
        BmcPeerReport,
    }

    let env = create_test_env(pool).await;
    crate::db_init::store_initial_dpu_agent_upgrade_policy(&env.pool, None)
        .await
        .unwrap();
    let segment_id = env.create_vpc_and_tenant_segment().await;
    for first in [
        FirstWriter::Report,
        FirstWriter::ForceDelete,
        FirstWriter::BmcPeerReport,
    ] {
        let mh = create_managed_host(&env).await;
        let instance = mh
            .instance_builer(&env)
            .single_interface_network_config(segment_id)
            .build()
            .await;
        let dpu_id = mh.dpu_ids[0];
        network_configured_with_health(&env, &dpu_id, None).await;
        let mut setup = env.db_txn().await;
        let before = mh.snapshot(&mut setup).await;
        assert!(!before.use_admin_network());
        assert!(before.managed_host_network_config_version_synced());
        let interface_id = before
            .host_snapshot
            .status
            .interfaces
            .iter()
            .find(|interface| interface.attached_dpu_machine_id == Some(dpu_id))
            .expect("the reporting DPU's host link")
            .id;
        let outside_state_cohort = matches!(first, FirstWriter::BmcPeerReport);
        let reporter_id = if outside_state_cohort {
            let peer = carbide_uuid::machine::DpuMachineId::try_from(MachineId::new(
                carbide_uuid::machine::MachineIdSource::ProductBoardChassisSerial,
                [0xbb; 32],
                MachineType::Dpu,
            ))
            .unwrap();
            db::machine::create(
                setup.as_mut(),
                None,
                &peer.into(),
                ManagedHostState::Ready,
                None,
                model::machine::CURRENT_STATE_MODEL_VERSION,
            )
            .await
            .unwrap();
            let segment = before
                .host_snapshot
                .status
                .interfaces
                .iter()
                .find(|interface| interface.id == interface_id)
                .unwrap()
                .segment_id;
            sqlx::query(
                "INSERT INTO machine_interfaces
                    (machine_id, attached_dpu_machine_id, association_type, interface_type,
                     segment_id, mac_address, primary_interface, hostname)
                 VALUES ($1, $2, 'Machine', 'Bmc', $3, '02:53:33:00:00:bb', false, 'group-peer')",
            )
            .bind(mh.id)
            .bind(peer)
            .bind(segment)
            .execute(setup.as_mut())
            .await
            .unwrap();
            assert!(
                db::machine::find_host_by_dpu_machine_id(setup.as_mut(), &peer)
                    .await
                    .unwrap()
                    .is_none()
            );
            let group: Vec<MachineId> =
                sqlx::query_scalar("SELECT id FROM machine_group_member_ids($1)")
                    .bind(mh.id)
                    .fetch_all(setup.as_mut())
                    .await
                    .unwrap();
            assert!(group.contains(&MachineId::from(peer)));
            peer
        } else {
            dpu_id
        };
        let report_version = db::machine::get_network_config(setup.as_mut(), &reporter_id.into())
            .await
            .unwrap()
            .version;
        let interfaces: Vec<MachineInterfaceId> = sqlx::query_scalar(
            "SELECT id FROM machine_interfaces WHERE machine_id = $1 ORDER BY id",
        )
        .bind(mh.id)
        .fetch_all(setup.as_mut())
        .await
        .unwrap();
        setup.commit().await.unwrap();
        let sent_at = SystemTime::now() + Duration::from_secs(60);
        let observed_at: chrono::DateTime<chrono::Utc> = sent_at.into();
        let dhcp_at = chrono::DateTime::from_timestamp(observed_at.timestamp() + 1, 0).unwrap();

        // A BEFORE ROW gate retains the real UPDATE's D row, never a host
        // interface. Both columns name the actual first winner's statement.
        let statement = format!(
            r#"
            CREATE FUNCTION pause_report_or_force_delete()
            RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM pg_advisory_xact_lock(5333, 41);
                RETURN NEW;
            END;
            $$;
            CREATE TRIGGER pause_report_or_force_delete
            BEFORE UPDATE OF network_status_observation, controller_state ON machines
            FOR EACH ROW WHEN (OLD.id = '{reporter_id}')
            EXECUTE FUNCTION pause_report_or_force_delete();
            "#
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
            .execute(&env.pool)
            .await
            .unwrap();
        let mut gate = env.db_txn().await;
        sqlx::query("SELECT pg_advisory_xact_lock(5333, 41)")
            .execute(gate.as_mut())
            .await
            .unwrap();
        let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(gate.as_mut())
            .await
            .unwrap();
        let report = maybe_done(env.api.record_dpu_network_status(Request::new(
            rpc::forge::DpuNetworkStatus {
                dpu_machine_id: Some(reporter_id),
                dpu_agent_version: Some(
                    common::api_fixtures::dpu::TEST_DPU_AGENT_VERSION.to_string(),
                ),
                observed_at: Some(sent_at.into()),
                network_config_version: Some(report_version.to_string()),
                dpu_health: Some(rpc::health::HealthReport {
                    source: "forge-dpu-agent".to_string(),
                    ..Default::default()
                }),
                last_dhcp_requests: vec![rpc::forge::LastDhcpRequest {
                    host_interface_id: Some(interface_id),
                    timestamp: dhcp_at.to_rfc3339(),
                }],
                ..Default::default()
            },
        )));
        let delete = maybe_done(env.api.admin_force_delete_machine(Request::new(
            AdminForceDeleteMachineRequest {
                wait_for_instance_dpu: true,
                ..force_delete_request(&mh.id)
            },
        )));
        tokio::pin!(report, delete);
        let mut report_pid = None;
        let mut delete_pid = None;
        let mut wait_arm = None;
        let readiness = timeout(Duration::from_secs(25), async {
            match first {
                FirstWriter::Report | FirstWriter::BmcPeerReport => {
                    let pid = tokio::select! {
                        () = &mut report => return Err("report passed the D row gate".to_string()),
                        pid = wait_for_blocked_query(&env.pool, gate_pid,
                            "UPDATE machines SET network_status_observation") => pid,
                    };
                    report_pid = Some(pid);
                    let (pid, arm) = tokio::select! {
                        () = &mut report => return Err("report passed the D row gate".to_string()),
                        () = &mut delete => return Err("force-delete did not reach D contention".to_string()),
                        pid = wait_for_blocked_query(&env.pool, pid, "WITH locked_group") =>
                            (pid, "legacy group wait retaining interfaces"),
                        pid = wait_for_blocked_query(&env.pool, pid,
                            "SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE") =>
                            (pid, "isolated D wait"),
                    };
                    delete_pid = Some(pid);
                    wait_arm = Some(arm);
                }
                FirstWriter::ForceDelete => {
                    let pid = tokio::select! {
                        () = &mut delete => return Err("force-delete passed the D state gate".to_string()),
                        pid = wait_for_blocked_query(&env.pool, gate_pid,
                            "UPDATE machines SET controller_state_version") => pid,
                    };
                    delete_pid = Some(pid);
                    report_pid = Some(tokio::select! {
                        () = &mut delete => return Err("force-delete passed the D state gate".to_string()),
                        () = &mut report => return Err("report did not wait for force-delete's D row".to_string()),
                        pid = wait_for_blocked_query(&env.pool, pid,
                            "UPDATE machines SET network_status_observation") => pid,
                    });
                    wait_arm = Some("report waits before its interface write");
                }
            }
            Ok::<(), String>(())
        })
        .await
        .unwrap_or_else(|error| Err(format!("report/force-delete readiness: {error}")));
        let released_prefix = if !matches!(first, FirstWriter::ForceDelete) && readiness.is_ok() {
            Some(
                async {
                    let mut probe = env.pool.begin().await?;
                    let host_free =
                        db::machine::try_lock_by_id_for_update(probe.as_mut(), &mh.id.into())
                            .await?;
                    let history_free =
                        db::machine::try_lock_machine_state_history(probe.as_mut(), &mh.id.into())
                            .await?;
                    let unlocked: Vec<MachineInterfaceId> = sqlx::query_scalar(
                        "SELECT id FROM machine_interfaces WHERE machine_id = $1
                     ORDER BY id FOR UPDATE SKIP LOCKED",
                    )
                    .bind(mh.id)
                    .fetch_all(probe.as_mut())
                    .await?;
                    let earlier_d_free = if outside_state_cohort {
                        Some(
                            db::machine::try_lock_by_id_for_no_key_update(
                                probe.as_mut(),
                                &dpu_id.into(),
                            )
                            .await?,
                        )
                    } else {
                        None
                    };
                    let routing_free: bool = sqlx::query_scalar(
                        "SELECT pg_try_advisory_xact_lock(
                        hashtextextended('tenant_prefix_overlap:checks', 0))",
                    )
                    .fetch_one(probe.as_mut())
                    .await?;
                    probe.rollback().await?;
                    Ok::<_, Box<dyn std::error::Error>>((
                        host_free,
                        history_free,
                        unlocked,
                        routing_free,
                        earlier_d_free,
                    ))
                }
                .await,
            )
        } else {
            None
        };
        let at_gate =
            if !matches!(first, FirstWriter::ForceDelete) && readiness.is_ok() {
                Some(sqlx::query_as::<_, (MachineId, String, String, serde_json::Value)>(
                r#"SELECT id, controller_state_version, network_config_version, controller_state
                   FROM machines WHERE id = ANY($1) ORDER BY id COLLATE "C""#,
            )
            .bind(vec![MachineId::from(mh.id), dpu_id.into()])
            .fetch_all(&env.pool)
            .await)
            } else {
                None
            };
        let released = gate.rollback().await;
        let finished = {
            let joined = async {
                tokio::join!(&mut report, &mut delete);
            };
            tokio::pin!(joined);
            let finished = timeout(Duration::from_secs(30), &mut joined).await.is_ok();
            if !finished {
                for pid in report_pid.into_iter().chain(delete_pid) {
                    sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                        .bind(pid)
                        .fetch_one(&env.pool)
                        .await
                        .expect("cancel only the observed report/force-delete writer");
                }
                timeout(Duration::from_secs(30), &mut joined)
                    .await
                    .expect("cancelled report/force-delete writers must join");
            }
            finished
        };
        let report_result = report.as_mut().take_output().expect("report joined");
        let delete_result = delete.as_mut().take_output().expect("force-delete joined");
        let dropped_gate = sqlx::raw_sql(
            "DROP TRIGGER pause_report_or_force_delete ON machines;
             DROP FUNCTION pause_report_or_force_delete();",
        )
        .execute(&env.pool)
        .await;
        tracing::info!(
            ?first,
            ?readiness,
            ?wait_arm,
            ?report_result,
            ?delete_result,
            "Joined actual DPU report and force-delete preparation"
        );
        released.expect("release the advisory gate");
        dropped_gate.expect("remove the joined test gate");
        readiness.expect("observe both actual statement waits");
        assert!(finished, "{first:?}: writers required cancellation");
        if let Some(prefix) = released_prefix {
            let (host_free, history_free, unlocked, routing_free, earlier_d_free) = prefix.unwrap();
            assert!(host_free && history_free && routing_free, "{first:?}");
            if let Some(free) = earlier_d_free {
                assert!(free, "the state-cohort D prefix must also be released");
            }
            assert_eq!(
                unlocked, interfaces,
                "all admitted interfaces must be released"
            );
            assert_eq!(wait_arm, Some("isolated D wait"));
        }
        if let Some(at_gate) = at_gate {
            let original_dpu = before
                .dpu_snapshots
                .iter()
                .find(|dpu| dpu.id == dpu_id)
                .unwrap();
            let mut expected = vec![
                (
                    MachineId::from(mh.id),
                    before.host_snapshot.state.version.to_string(),
                    before.host_snapshot.network_config.version.to_string(),
                    serde_json::to_value(&before.host_snapshot.state.value).unwrap(),
                ),
                (
                    MachineId::from(dpu_id),
                    original_dpu.state.version.to_string(),
                    original_dpu.network_config.version.to_string(),
                    serde_json::to_value(&original_dpu.state.value).unwrap(),
                ),
            ];
            expected.sort_unstable_by_key(|row| row.0);
            assert_eq!(
                at_gate.unwrap(),
                expected,
                "rejected preparation must publish no partial marker/group"
            );
        }
        if outside_state_cohort {
            // Normal startup seeds the upgrade policy. Its second transaction
            // reports missing non-BMC ownership after committing D/I observations.
            let error =
                report_result.expect_err("only the post-commit upgrade lookup rejects the peer");
            assert_eq!(error.code(), tonic::Code::NotFound);
            assert_eq!(error.message(), format!("machine not found: {reporter_id}"));
        } else {
            report_result.expect("the complete network report must persist");
        }
        let response = delete_result
            .expect("force-delete preparation must commit")
            .into_inner();
        assert!(!response.all_done, "Admin remains unacknowledged");
        assert_eq!(response.instance_id, instance.id.to_string());
        let mut check = env.db_txn().await;
        let after = mh.snapshot(&mut check).await;
        let interface = db::machine_interface::find_one(check.as_mut(), interface_id)
            .await
            .unwrap();
        let retained_instance = db::instance::find_by_id(check.as_mut(), instance.id).await;
        let reporter = db::machine::find_one(check.as_mut(), &reporter_id, Default::default())
            .await
            .unwrap()
            .unwrap();
        check.commit().await.unwrap();
        assert!(after.use_admin_network());
        assert_eq!(
            after.host_snapshot.state.value,
            ManagedHostState::ForceDeletion
        );
        assert_ne!(
            after.host_snapshot.state.version,
            before.host_snapshot.state.version
        );
        let dpu = after
            .dpu_snapshots
            .iter()
            .find(|dpu| dpu.id == dpu_id)
            .unwrap();
        assert_eq!(dpu.state.value, ManagedHostState::ForceDeletion);
        assert_ne!(dpu.state.version, before.dpu_snapshots[0].state.version);
        if outside_state_cohort {
            assert_eq!(reporter.state.value, ManagedHostState::Ready);
            assert_eq!(
                reporter.network_config.version,
                after.host_snapshot.network_config.version
            );
        }
        let observed = reporter.network_status_observation.as_ref().unwrap();
        assert_eq!(observed.observed_at, observed_at);
        assert_eq!(observed.network_config_version, Some(report_version));
        assert_eq!(interface.last_dhcp, Some(dhcp_at));
        retained_instance
            .expect("preparation must retain the active Instance while waiting for Admin");
    }
}

async fn observe_force_delete_state_lock_wait(
    pool: &sqlx::PgPool,
    blocker_pid: Option<i32>,
    waiter_pid: Option<i32>,
) -> Result<(i32, String, bool), String> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let waiting = sqlx::query_as(
                "SELECT a.pid, a.query,
                        EXISTS(SELECT 1 FROM pg_locks l WHERE l.pid = a.pid
                               AND l.locktype = 'advisory' AND NOT l.granted)
                 FROM pg_stat_activity a
                 WHERE a.datname = current_database() AND a.wait_event_type = 'Lock'
                   AND ($1::integer IS NULL OR $1 = ANY(pg_blocking_pids(a.pid)))
                   AND ($2::integer IS NULL OR a.pid = $2)
                 ORDER BY a.pid LIMIT 1",
            )
            .bind(blocker_pid)
            .bind(waiter_pid)
            .fetch_optional(pool)
            .await
            .map_err(|error| format!("observe lock queue: {error:?}"))?;
            if let Some(waiting) = waiting {
                return Ok(waiting);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|error| format!("lock queue did not appear: {error}"))?
}

/// An opted-in deletion retains every receiver until Admin is acknowledged and
/// rechecks that acknowledgement under the routing lock before releasing resources.
#[crate::sqlx_test]
async fn force_delete_retains_instance_until_every_dpu_acknowledges_admin(pool: sqlx::PgPool) {
    let mut config = get_config();
    config.dpu_config.restart_ovs_on_use_admin_network_change = true;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let managed_host = create_managed_host_multi_dpu(&env, 2).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let mut txn = env.pool.begin().await.unwrap();
    let tenant_snapshot = managed_host.snapshot(&mut txn).await;
    assert!(!tenant_snapshot.use_admin_network());
    assert!(tenant_snapshot.managed_host_network_config_version_synced());
    let addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        txn.as_mut(),
        &instance.id,
        &segment_id,
    )
    .await
    .unwrap()
    .into_iter()
    .map(|address| (address.address, address.prefix, address.vpc_id))
    .collect::<Vec<_>>();
    assert!(!addresses.is_empty());
    txn.commit().await.unwrap();

    // A request naming one DPU still protects the host's entire topology.
    let waiting = env
        .api
        .admin_force_delete_machine(Request::new(AdminForceDeleteMachineRequest {
            wait_for_instance_dpu: true,
            ..force_delete_request(&managed_host.dpu_ids[0])
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(!waiting.all_done);
    assert_eq!(waiting.instance_id, instance.id.to_string());
    let mut txn = env.pool.begin().await.unwrap();
    let admin_snapshot = managed_host.snapshot(&mut txn).await;
    let admin_version = admin_snapshot.host_snapshot.network_config.version;
    assert!(admin_snapshot.use_admin_network());
    assert_ne!(
        admin_version,
        tenant_snapshot.host_snapshot.network_config.version
    );
    let primary_dpu = admin_snapshot
        .host_snapshot
        .status
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .unwrap()
        .attached_dpu_machine_id
        .unwrap();
    for dpu in &admin_snapshot.dpu_snapshots {
        assert_eq!(
            dpu.network_config
                .use_admin_network_changed
                .unwrap_or(false),
            dpu.id == primary_dpu,
        );
    }
    txn.commit().await.unwrap();

    // Only the first request opts in. Retries cannot cancel its recorded wait.
    for acknowledged_dpus in [0, 1] {
        if acknowledged_dpus == 1 {
            network_configured_with_health(&env, &managed_host.dpu_ids[0], None).await;
            sqlx::query("UPDATE machines SET network_status_observation = NULL WHERE id = $1")
                .bind(managed_host.dpu_ids[1])
                .execute(&env.pool)
                .await
                .unwrap();
        }
        let waiting = env
            .api
            .admin_force_delete_machine(Request::new(force_delete_request(&managed_host.id)))
            .await
            .unwrap()
            .into_inner();
        assert!(!waiting.all_done);
        let mut txn = env.pool.begin().await.unwrap();
        let snapshot = managed_host.snapshot(&mut txn).await;
        assert_eq!(snapshot.managed_state, ManagedHostState::ForceDeletion);
        assert_eq!(snapshot.host_snapshot.network_config.version, admin_version);
        assert!(!snapshot.managed_host_network_config_version_synced());
        assert_eq!(
            sqlx::query_scalar::<_, Option<bool>>(
                "SELECT force_delete_requires_admin_ack FROM machines WHERE id = $1",
            )
            .bind(managed_host.id)
            .fetch_one(txn.as_mut())
            .await
            .unwrap(),
            Some(true),
        );
        assert_eq!(snapshot.dpu_snapshots.len(), 2);
        let retained = snapshot.instance.as_ref().unwrap();
        assert_eq!(retained.id, instance.id);
        assert!(retained.deleted.is_none());
        let retained_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &instance.id,
            &segment_id,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|address| (address.address, address.prefix, address.vpc_id))
        .collect::<Vec<_>>();
        assert_eq!(retained_addresses, addresses);
        for machine_id in managed_host
            .dpu_ids
            .iter()
            .copied()
            .map(MachineId::from)
            .chain([managed_host.id.into()])
        {
            let history = db::state_history::for_object(
                txn.as_mut(),
                db::state_history::StateHistoryTableId::Machine,
                &machine_id,
            )
            .await
            .unwrap();
            assert_eq!(
                history
                    .iter()
                    .filter(|record| {
                        serde_json::from_str::<ManagedHostState>(&record.state).unwrap()
                            == ManagedHostState::ForceDeletion
                    })
                    .count(),
                1,
                "polling must not duplicate ForceDeletion history for {machine_id}",
            );
        }
        txn.commit().await.unwrap();
        for dpu_id in &managed_host.dpu_ids {
            let config = env
                .api
                .get_managed_host_network_config(Request::new(
                    rpc::forge::ManagedHostNetworkConfigRequest {
                        dpu_machine_id: Some(*dpu_id),
                    },
                ))
                .await
                .unwrap()
                .into_inner();
            assert!(config.use_admin_network);
            assert!(config.tenant_interfaces.is_empty());
            assert_eq!(
                config.managed_host_config_version,
                admin_version.to_string()
            );
        }
    }

    network_configured_with_health(&env, &managed_host.dpu_ids[1], None).await;
    let mut address_guard = env.pool.begin().await.unwrap();
    lock_instance_address(address_guard.as_mut(), instance.id).await;
    let deletion = env
        .api
        .admin_force_delete_machine(Request::new(force_delete_request(&managed_host.id)));
    let change_network = async {
        wait_until_blocked_on(&env.pool, "SELECT id FROM instance_addresses").await;
        // The final transaction already owns the routing lock while it waits
        // for the addresses; another routing writer must wait behind it.
        let competing_writer = async {
            let mut txn = env.pool.begin().await.unwrap();
            db::tenant_prefix_overlap::lock_checks(&mut txn)
                .await
                .expect("routing writer acquires the lock after deletion finishes");
            txn.commit().await.unwrap();
        };
        let invalidate_ack = async {
            wait_until_blocked_on(&env.pool, "pg_advisory_xact_lock").await;
            // Machine-side writers can change the version without the routing
            // lock. The final acknowledgement check must catch that change.
            let mut txn = env.pool.begin().await.unwrap();
            assert!(matches!(
                db::machine::try_update_network_config(
                    txn.as_mut(),
                    &managed_host.id,
                    admin_version,
                    &admin_snapshot.host_snapshot.network_config.value,
                )
                .await
                .unwrap(),
                db::ConditionalWrite::Applied(()),
            ));
            address_guard.commit().await.unwrap();
            // Keep the version update open so cleanup must lock the host
            // before checking its acknowledgement and committing removal.
            let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(txn.as_mut())
                .await
                .unwrap();
            common::postgres::wait_for_blocked_query(&env.pool, writer_pid, "SELECT row_to_json")
                .await;
            txn.commit().await.unwrap();
        };
        tokio::join!(competing_writer, invalidate_ack);
    };
    let (result, ()) = tokio::join!(deletion, change_network);
    assert!(!result.unwrap().into_inner().all_done);
    let retained = db::instance::find_by_id(&env.pool, instance.id)
        .await
        .unwrap()
        .unwrap();
    assert!(retained.deleted.is_some());
    let mut txn = env.pool.begin().await.unwrap();
    let retained_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        txn.as_mut(),
        &instance.id,
        &segment_id,
    )
    .await
    .unwrap()
    .into_iter()
    .map(|address| (address.address, address.prefix, address.vpc_id))
    .collect::<Vec<_>>();
    assert_eq!(retained_addresses, addresses);
    txn.commit().await.unwrap();
    managed_host.network_configured(&env).await;

    let response = env
        .api
        .admin_force_delete_machine(Request::new(force_delete_request(&managed_host.id)))
        .await
        .unwrap()
        .into_inner();
    assert!(response.all_done);
    assert_eq!(response.instance_id, instance.id.to_string());
    assert!(
        db::instance::find_by_id(&env.pool, instance.id)
            .await
            .unwrap()
            .is_none()
    );
    for machine_id in managed_host
        .dpu_ids
        .iter()
        .copied()
        .map(MachineId::from)
        .chain([managed_host.id.into()])
    {
        validate_machine_deletion(&env, &machine_id, None).await;
    }
}

/// Once requested, the Admin wait survives a missing Instance and an existing
/// failure between retries and machine-controller iterations.
#[crate::sqlx_test]
async fn force_delete_keeps_requested_wait_after_the_instance_disappears(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host(&env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let mut txn = env.pool.begin().await.unwrap();
    let failure = model::machine::FailureDetails {
        cause: model::machine::FailureCause::NVMECleanFailed {
            err: "failed before force deletion".to_string(),
        },
        source: model::machine::FailureSource::Scout,
        failed_at: chrono::Utc::now(),
    };
    let host = managed_host.host().db_machine(&mut txn).await;
    db::machine::update_failure_details(&host, &mut txn, failure.clone())
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let waiting = env
        .api
        .admin_force_delete_machine(Request::new(AdminForceDeleteMachineRequest {
            wait_for_instance_dpu: true,
            ..force_delete_request(&managed_host.id)
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(!waiting.all_done);
    assert_eq!(waiting.instance_id, instance.id.to_string());
    let mut txn = env.pool.begin().await.unwrap();
    db::instance::delete(instance.id, txn.as_mut())
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let mut requested_version = None;
    let mut state_version = None;
    for _ in 0..2 {
        let waiting = env
            .api
            .admin_force_delete_machine(Request::new(force_delete_request(&managed_host.id)))
            .await
            .unwrap()
            .into_inner();
        assert!(!waiting.all_done);
        env.run_machine_state_controller_iteration().await;
        let mut txn = env.pool.begin().await.unwrap();
        let snapshot = managed_host.snapshot(&mut txn).await;
        assert!(snapshot.instance.is_none());
        assert!(snapshot.use_admin_network());
        assert_eq!(snapshot.managed_state, ManagedHostState::ForceDeletion);
        assert_eq!(snapshot.dpu_snapshots.len(), 1);
        assert!(!snapshot.managed_host_network_config_version_synced());
        let version = snapshot.host_snapshot.network_config.version;
        assert_eq!(*requested_version.get_or_insert(version), version);
        let version = snapshot.host_snapshot.state.version;
        assert_eq!(*state_version.get_or_insert(version), version);
        assert_eq!(snapshot.host_snapshot.status.failure_details, failure);
        let history = db::state_history::for_object(
            txn.as_mut(),
            db::state_history::StateHistoryTableId::Machine,
            &MachineId::from(managed_host.id),
        )
        .await
        .unwrap();
        assert_eq!(
            history
                .iter()
                .filter(|record| {
                    serde_json::from_str::<ManagedHostState>(&record.state).unwrap()
                        == ManagedHostState::ForceDeletion
                })
                .count(),
            1,
            "the controller and retries must preserve the first ForceDeletion transition",
        );
        txn.commit().await.unwrap();
    }
    managed_host.network_configured(&env).await;
    let response = env
        .api
        .admin_force_delete_machine(Request::new(force_delete_request(&managed_host.id)))
        .await
        .unwrap()
        .into_inner();
    assert!(response.all_done);
    for machine_id in [managed_host.id.into(), managed_host.dpu_ids[0].into()] {
        validate_machine_deletion(&env, &machine_id, None).await;
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

    // Set `ManagedHostState::ForceDeletion` directly so this test isolates its
    // mapping to `TenantState::Terminating` from the RPC's network
    // acknowledgement workflow.

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
async fn force_delete_rechecks_instance_type_after_machine_lock_wait(pool: sqlx::PgPool) {
    use futures_util::future::maybe_done;
    use tokio::time::{Duration as TimeoutDuration, timeout};

    let env = create_test_env(pool).await;
    let instance_type_id = get_instance_type_fixture_id(&env)
        .await
        .parse::<carbide_uuid::instance_type::InstanceTypeId>()
        .unwrap();
    let mh = create_managed_host(&env).await;
    let unrelated = create_managed_host(&env).await;
    let mut gate = env.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE")
        .bind(mh.id)
        .execute(gate.as_mut())
        .await
        .unwrap();
    let before = mh.snapshot(&mut gate).await;
    assert_eq!(before.managed_state, ManagedHostState::Ready);
    assert!(before.instance.is_none());
    assert!(before.host_snapshot.config.instance_type_id.is_none());
    let ordinary_version = before.host_snapshot.version;
    let ids = mh
        .dpu_ids
        .iter()
        .copied()
        .map(MachineId::from)
        .chain([mh.id.into()])
        .collect::<Vec<_>>();
    let history_before = serde_json::to_value(
        db::state_history::find_by_object_ids(
            gate.as_mut(),
            db::state_history::StateHistoryTableId::Machine,
            &ids,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(gate.as_mut())
        .await
        .unwrap();
    let deletion = maybe_done(env.api.admin_force_delete_machine(Request::new(
        AdminForceDeleteMachineRequest {
            allow_delete_with_instance: false,
            allow_delete_with_instance_type: false,
            ..force_delete_request(&mh.id)
        },
    )));
    let independent = maybe_done(
        env.api
            .admin_force_delete_machine(Request::new(force_delete_request(&unrelated.id))),
    );
    tokio::pin!(deletion, independent);
    let readiness = tokio::select! {
        () = &mut deletion => Err("force-delete passed its Machine gate".to_string()),
        observed = observe_force_delete_state_lock_wait(&env.pool, Some(gate_pid), None) => observed,
    };
    // This call must finish while the first host remains busy. Keep its
    // future alive on timeout, then release the gate before joining both.
    let independent_before_release = readiness.is_ok()
        && timeout(TimeoutDuration::from_secs(30), &mut independent)
            .await
            .is_ok();
    let independent_ids = unrelated
        .dpu_ids
        .iter()
        .copied()
        .map(MachineId::from)
        .chain([unrelated.id.into()])
        .collect::<Vec<_>>();
    let independent_at_gate = if independent_before_release {
        Some(
            sqlx::query_scalar::<_, MachineId>("SELECT id FROM machines WHERE id = ANY($1)")
                .bind(&independent_ids)
                .fetch_all(&env.pool)
                .await,
        )
    } else {
        None
    };
    // The public association owner uses this versioned update after its
    // no-Instance/capability checks. Use its gate connection so it can commit
    // the association before the competing ForceDelete row wait finishes.
    let producer = if readiness.is_ok() {
        Some(
            db::machine::associate_machines_with_instance_type(
                gate.as_mut(),
                &instance_type_id,
                &[(mh.id.as_host_machine_id(), &ordinary_version)],
            )
            .await,
        )
    } else {
        None
    };
    let release = gate.commit().await;
    let (timed_out, cancellation) = {
        let joined = async {
            tokio::join!(&mut deletion, &mut independent);
        };
        tokio::pin!(joined);
        let timed_out = timeout(TimeoutDuration::from_secs(60), &mut joined)
            .await
            .is_err();
        let cancellation = if timed_out {
            match readiness.as_ref() {
                Ok((pid, _, _)) => Some(
                    sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                        .bind(pid)
                        .fetch_one(&env.pool)
                        .await,
                ),
                Err(_) => None,
            }
        } else {
            None
        };
        if timed_out {
            timeout(TimeoutDuration::from_secs(30), &mut joined)
                .await
                .expect("cancelled force-delete calls must join");
        }
        (timed_out, cancellation)
    };
    let result = deletion.as_mut().take_output().unwrap();
    let independent_result = independent.as_mut().take_output().unwrap();
    let mut read = env.pool.begin().await.unwrap();
    let history_after = serde_json::to_value(
        db::state_history::find_by_object_ids(
            read.as_mut(),
            db::state_history::StateHistoryTableId::Machine,
            &ids,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let after = db::machine::find_one(read.as_mut(), &mh.id, MachineSearchConfig::default())
        .await
        .unwrap();
    read.rollback().await.unwrap();
    let diagnostic = format!(
        "readiness={readiness:?}; producer={producer:?}; release={release:?}; \
         timeout={timed_out}; cancellation={cancellation:?}; response={result:?}; \
         independent_before_release={independent_before_release}; \
         independent_at_gate={independent_at_gate:?}; independent={independent_result:?}; \
         current_type={:?}; current_state={:?}",
        after
            .as_ref()
            .and_then(|machine| machine.config.instance_type_id.as_ref()),
        after.as_ref().map(|machine| machine.current_state()),
    );
    assert!(
        readiness.is_ok() && release.is_ok() && !timed_out,
        "{diagnostic}"
    );
    assert!(independent_before_release, "{diagnostic}");
    assert!(
        independent_result.unwrap().into_inner().all_done,
        "{diagnostic}"
    );
    assert!(
        independent_at_gate.unwrap().unwrap().is_empty(),
        "{diagnostic}"
    );
    let (_, query, advisory_wait) = readiness.unwrap();
    assert!(
        !advisory_wait
            && (query.starts_with("SELECT row_to_json(m.*)")
                || query.starts_with("SELECT id FROM machines WHERE id = $1 FOR UPDATE")),
        "{diagnostic}"
    );
    assert_eq!(
        producer.unwrap().unwrap(),
        vec![MachineId::from(mh.id)],
        "{diagnostic}"
    );
    assert_eq!(
        result.unwrap_err().code(),
        tonic::Code::FailedPrecondition,
        "{diagnostic}"
    );
    let after = after.expect("a denied force-delete must retain the host");
    assert_eq!(after.config.instance_type_id, Some(instance_type_id));
    assert_eq!(after.state.value, before.managed_state);
    assert_eq!(after.state.version, before.host_snapshot.state.version);
    assert_eq!(
        history_after, history_before,
        "denied deletion must leave H/D history unchanged"
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
    request.allow_delete_with_instance = false;

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

/// An interrupted DPF cleanup of an Admin-only host remains retryable without
/// its offline DPU. Both calls use BMC MAC-derived CR names within the SDK limit.
#[crate::sqlx_test]
async fn test_admin_force_delete_with_dpf_retries_offline_admin_cleanup(pool: sqlx::PgPool) {
    use model::bmc_suppression::{
        BmcSuppressionSource, BmcSuppressionSubsystem, NewBmcSuppression,
    };

    type DpfCallLog = Vec<(String, Vec<String>)>;
    let captured_calls: Arc<std::sync::Mutex<DpfCallLog>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut mock = MockDpfOperations::new();
    let cap = captured_calls.clone();
    mock.expect_force_delete_host()
        .returning(move |node_name, device_names| {
            let mut calls = cap.lock().unwrap();
            calls.push((node_name.to_string(), device_names.to_vec()));
            if calls.len() == 1 {
                Err(carbide_dpf::DpfError::InvalidState(
                    "injected cleanup failure".to_string(),
                ))
            } else {
                Ok(())
            }
        });
    let env = dpf_force_delete_test_env(pool, mock).await;

    let mh = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        create_managed_host_with_dpf(&env),
    )
    .await
    .expect("timed out during initial provisioning");
    let host_id = mh.id;

    // Remove the DPU receipt before either request so both exercise offline Admin cleanup without
    // a later network acknowledgement.
    sqlx::query("UPDATE machines SET network_status_observation = NULL WHERE id = $1")
        .bind(mh.dpu_ids[0])
        .execute(&env.pool)
        .await
        .unwrap();
    let mut txn = env.pool.begin().await.unwrap();
    let snapshot = mh.snapshot(&mut txn).await;
    assert!(snapshot.instance.is_none());
    assert!(snapshot.use_admin_network());
    assert!(!snapshot.managed_host_network_config_version_synced());
    let bmc_macs = [
        snapshot.host_snapshot.status.bmc_info.mac.unwrap(),
        snapshot.dpu_snapshots[0].status.bmc_info.mac.unwrap(),
    ];
    for bmc_mac_address in bmc_macs {
        for source in [
            BmcSuppressionSource::DpfProvisioning,
            BmcSuppressionSource::Decommissioning,
        ] {
            db::bmc_suppression::upsert(
                txn.as_mut(),
                &NewBmcSuppression {
                    bmc_mac_address,
                    subsystem: BmcSuppressionSubsystem::SiteExplorer,
                    source,
                    reason: "test".to_string(),
                },
            )
            .await
            .unwrap();
        }
    }
    txn.commit().await.unwrap();

    // Trigger DPF cleanup after confirming Admin remains unsynchronized; the first failure must
    // leave the committed ForceDeletion transition retryable.
    let error = env
        .api
        .admin_force_delete_machine(Request::new(force_delete_request(&host_id)))
        .await
        .expect_err("the first DPF cleanup fails after ForceDeletion commits");
    assert_eq!(error.code(), tonic::Code::Internal);
    assert!(error.message().contains("injected cleanup failure"));

    // Reload after the external cleanup failure to prove ForceDeletion committed without
    // restoring the missing DPU receipt.
    let mut txn = env.pool.begin().await.unwrap();
    let snapshot = mh.snapshot(&mut txn).await;
    assert_eq!(snapshot.managed_state, ManagedHostState::ForceDeletion);
    assert!(!snapshot.managed_host_network_config_version_synced());
    txn.commit().await.unwrap();

    for mac in bmc_macs {
        assert_eq!(
            db::bmc_suppression::find_for_bmc(
                &env.pool,
                mac,
                BmcSuppressionSubsystem::SiteExplorer
            )
            .await
            .unwrap(),
            vec![BmcSuppressionSource::Decommissioning],
            "only the provisioning suppression must clear for {mac}, even when DPF cleanup fails",
        );
    }

    // Retry with the same missing observation that preceded the first call.
    let response = env
        .api
        .admin_force_delete_machine(Request::new(force_delete_request(&host_id)))
        .await
        .expect("retry must preserve offline Admin cleanup")
        .into_inner();
    assert!(response.all_done);
    for machine_id in [host_id.into(), mh.dpu_ids[0].into()] {
        validate_machine_deletion(&env, &machine_id, None).await;
    }

    let calls = captured_calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "the second request must retry DPF cleanup");
    assert_eq!(calls[0], calls[1]);

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

/// Verifies explicit orphan consent completes Machine deletion after DPF cleanup fails, preserving
/// a remediation path when retrying the external operation cannot recover the host.
#[crate::sqlx_test]
async fn force_delete_orphan_override_continues_after_dpf_cleanup_failure(pool: sqlx::PgPool) {
    // Inject a persistent DPF deletion failure that only explicit orphan consent may bypass.
    let mut mock = MockDpfOperations::new();
    mock.expect_force_delete_host().times(1).returning(|_, _| {
        Err(carbide_dpf::DpfError::InvalidState(
            "injected persistent cleanup failure".to_string(),
        ))
    });
    let env = dpf_force_delete_test_env(pool, mock).await;

    // Provision a DPF-ingested host so force-delete reaches the injected external cleanup failure.
    let managed_host = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        create_managed_host_with_dpf(&env),
    )
    .await
    .expect("timed out during initial provisioning");

    // The override acknowledges the failed DPF removal and completes Core cleanup in one request.
    let response = env
        .api
        .admin_force_delete_machine(Request::new(AdminForceDeleteMachineRequest {
            allow_delete_with_orphaned_dpf_crds: true,
            ..force_delete_request(&managed_host.id)
        }))
        .await
        .expect("orphan consent must bypass the DPF cleanup failure")
        .into_inner();
    assert!(response.all_done);
    for machine_id in [managed_host.id.into(), managed_host.dpu_ids[0].into()] {
        validate_machine_deletion(&env, &machine_id, None).await;
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
            release_preserved_addresses: false,
            wait_for_instance_dpu: false,
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
    use model::bmc_suppression::{
        BmcSuppressionSource, BmcSuppressionSubsystem, NewBmcSuppression,
    };

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
            source: BmcSuppressionSource::Decommissioning,
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
            source: BmcSuppressionSource::Decommissioning,
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
            release_preserved_addresses: false,
            wait_for_instance_dpu: false,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(response.all_done);
    assert!(response.host_interfaces_deleted);

    let mut txn = env.pool.begin().await.unwrap();
    assert!(
        !db::bmc_suppression::is_suppressed(
            txn.as_mut(),
            bmc_mac,
            BmcSuppressionSubsystem::SiteExplorer
        )
        .await
        .unwrap()
    );
    assert!(
        !db::bmc_suppression::is_suppressed(txn.as_mut(), bmc_mac, BmcSuppressionSubsystem::Dhcp)
            .await
            .unwrap()
    );
    assert!(
        db::retained_boot_interface::find_by_mac(txn.as_mut(), boot_mac, None)
            .await
            .unwrap()
            .is_none()
    );
    txn.rollback().await.unwrap();
}

mod force_delete_observation {
    use std::collections::HashMap;

    use carbide_uuid::machine::{DpuMachineId, HostMachineId, MachineInterfaceId};

    use super::{InstanceId, MachineId, MachineSearchConfig};

    pub(super) struct Observation {
        pub(super) machines: HashMap<MachineId, model::machine::AnyMachine>,
        pub(super) link: model::machine::MachineInterfaceSnapshot,
        pub(super) dpu_ids: Vec<DpuMachineId>,
    }

    pub(super) async fn observe(
        pool: &sqlx::PgPool,
        host_id: HostMachineId,
        ids: &[MachineId],
        link_id: MachineInterfaceId,
    ) -> Result<Observation, Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machines = db::machine::find(
            txn.as_mut(),
            db::ObjectFilter::List(ids),
            MachineSearchConfig {
                include_history: true,
                ..Default::default()
            },
        )
        .await?
        .into_iter()
        .map(|machine| (machine.id, machine))
        .collect();
        let link = db::machine_interface::find_one(txn.as_mut(), link_id).await?;
        let dpu_ids = db::machine::find_dpus_by_host_machine_id(txn.as_mut(), &host_id)
            .await?
            .into_iter()
            .map(|machine| machine.id)
            .collect();
        txn.commit().await?;
        Ok(Observation {
            machines,
            link,
            dpu_ids,
        })
    }

    #[derive(PartialEq)]
    pub(super) struct PreservedQueryScope {
        pub(super) machines: Vec<serde_json::Value>,
        pub(super) instance: Option<serde_json::Value>,
        pub(super) addresses: Vec<serde_json::Value>,
        pub(super) allocations: Vec<serde_json::Value>,
    }

    pub(super) async fn query_scope(
        pool: &sqlx::PgPool,
        host_id: HostMachineId,
        dpu_id: DpuMachineId,
        instance_id: InstanceId,
        segment_id: carbide_uuid::network::NetworkSegmentId,
    ) -> Result<PreservedQueryScope, sqlx::Error> {
        let mut txn = pool.begin().await?;
        let machines =
            sqlx::query_scalar("SELECT to_jsonb(m) FROM machines m WHERE id = ANY($1) ORDER BY id")
                .bind(vec![MachineId::from(host_id), dpu_id.into()])
                .fetch_all(txn.as_mut())
                .await?;
        let instance = sqlx::query_scalar("SELECT to_jsonb(i) FROM instances i WHERE id = $1")
            .bind(instance_id)
            .fetch_optional(txn.as_mut())
            .await?;
        let addresses = sqlx::query_scalar(
            "SELECT to_jsonb(a) FROM instance_addresses a
             WHERE instance_id = $1 AND segment_id = $2 ORDER BY id",
        )
        .bind(instance_id)
        .bind(segment_id)
        .fetch_all(txn.as_mut())
        .await?;
        let allocations = sqlx::query_scalar(
            "SELECT to_jsonb(a) FROM resource_pool_allocations a
             WHERE owner_type = $1 AND owner_id = ANY($2) ORDER BY allocation_id",
        )
        .bind(model::resource_pool::OwnerType::Machine.to_string())
        .bind(vec![host_id.to_string(), dpu_id.to_string()])
        .fetch_all(txn.as_mut())
        .await?;
        txn.commit().await?;
        Ok(PreservedQueryScope {
            machines,
            instance,
            addresses,
            allocations,
        })
    }
}

/// Orphan deletion retains existing attachment references while generic
/// inventory reconciliation changes their classification.
#[crate::sqlx_test]
async fn force_delete_admits_existing_orphan_interface_classification(pool: sqlx::PgPool) {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use carbide_uuid::machine::HostMachineId;
    use db::test_support::postgres::wait_for_blocked_query;
    use futures_util::future::maybe_done;
    use tokio::time::{Duration, timeout};

    use self::force_delete_observation::{PreservedQueryScope, observe, query_scope};

    #[derive(Clone, Copy)]
    enum Case {
        Preallocation,
        Deletion,
    }

    let env = create_test_env(pool).await;
    let tenant_segment = env.create_vpc_and_tenant_segment().await;
    let mut failures = Vec::new();
    for (scenario, case) in [
        ("generic preallocation first", Case::Preallocation),
        ("admitted orphan deletion first", Case::Deletion),
    ] {
        let mh = create_managed_host(&env).await;
        let instance = mh
            .instance_builer(&env)
            .single_interface_network_config(tenant_segment)
            .build()
            .await;
        network_configured_with_health(&env, &mh.dpu_ids[0], None).await;
        let host_id: HostMachineId = mh.id.into();
        let dpu1_id = mh.dpu_ids[0];
        let mut setup = env.db_txn().await;
        let snapshot = mh.snapshot(&mut setup).await;
        let host_link = snapshot
            .host_snapshot
            .status
            .interfaces
            .iter()
            .find(|interface| interface.attached_dpu_machine_id == Some(dpu1_id))
            .expect("the fixture host owns its DPU link")
            .clone();
        assert!(!snapshot.host_snapshot.config.dpf.used_for_ingestion);
        assert!(!snapshot.use_admin_network());
        assert!(snapshot.managed_host_network_config_version_synced());
        setup.commit().await.unwrap();
        let producer_first = matches!(case, Case::Preallocation);
        let ip = host_link
            .addresses
            .iter()
            .copied()
            .find(IpAddr::is_ipv4)
            .expect("the existing host link has IPv4");
        let mut setup = env.db_txn().await;
        db::machine_interface::preallocate_bmc_machine_interface(
            setup.as_mut(),
            host_link.mac_address,
            ip,
            None,
        )
        .await
        .unwrap();
        let bmc = db::machine_interface::find_one(setup.as_mut(), host_link.id)
            .await
            .unwrap();
        assert_eq!(bmc.machine_id, Some(host_id.into()));
        assert_eq!(bmc.attached_dpu_machine_id, Some(dpu1_id));
        assert_eq!(bmc.interface_type, InterfaceType::Bmc);
        assert!(
            db::machine::find_host_by_dpu_machine_id(setup.as_mut(), &dpu1_id)
                .await
                .unwrap()
                .is_none()
        );
        let reservation =
            db::machine_interface_address::find_for_interface(setup.as_mut(), host_link.id)
                .await
                .unwrap()
                .into_iter()
                .find(|address| address.address == ip)
                .unwrap();
        assert_eq!(
            reservation.allocation_type,
            model::allocation_type::AllocationType::Static
        );
        setup.commit().await.unwrap();
        let ids = [MachineId::from(host_id), MachineId::from(dpu1_id)];
        let before = observe(&env.pool, host_id, &ids, host_link.id)
            .await
            .unwrap();
        let before_scope = query_scope(&env.pool, host_id, dpu1_id, instance.id, tenant_segment)
            .await
            .unwrap();
        let checkpoint = env.redfish_sim.timepoint();
        let clients_before = env.redfish_sim.create_client_calls();

        let mut gate = if producer_first {
            None
        } else {
            // Pause the actual D state write after orphan authorization.
            let statement = format!(
                r#"
                CREATE FUNCTION pause_orphan_reference_deletion()
                RETURNS trigger LANGUAGE plpgsql AS $$
                BEGIN
                    PERFORM pg_advisory_xact_lock(5333, 83);
                    RETURN NEW;
                END;
                $$;
                CREATE TRIGGER pause_orphan_reference_deletion
                BEFORE UPDATE OF controller_state ON machines
                FOR EACH ROW WHEN (OLD.id = '{dpu1_id}')
                EXECUTE FUNCTION pause_orphan_reference_deletion();
            "#
            );
            sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
                .execute(&env.pool)
                .await
                .unwrap();
            let mut gate = env.db_txn().await;
            sqlx::query("SELECT pg_advisory_xact_lock(5333, 83)")
                .execute(gate.as_mut())
                .await
                .unwrap();
            Some(gate)
        };
        let gate_pid = if let Some(gate) = gate.as_mut() {
            Some(
                sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
                    .fetch_one(gate.as_mut())
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        let mut producer_txn = env.db_txn().await;
        let producer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(producer_txn.as_mut())
            .await
            .unwrap();
        // This is Site Explorer's generic NVOS preallocation DB producer.
        // Its identical static address leaves only classification to reconcile.
        let mut producer = Box::pin(maybe_done(
            db::machine_interface::preallocate_machine_interface(
                producer_txn.as_mut(),
                host_link.mac_address,
                ip,
                None,
            ),
        ));
        if producer_first {
            (&mut producer).await;
        }
        let mut deletion = Box::pin(maybe_done(env.api.admin_force_delete_machine(
            Request::new(AdminForceDeleteMachineRequest {
                allow_delete_with_instance: false,
                ..force_delete_request(&dpu1_id)
            }),
        )));
        let mut deletion_pid = None;
        let mut observed_query = None;
        let readiness = timeout(Duration::from_secs(10), async {
            if producer_first {
                // Both old cleanup and corrected isolated admission can wait
                // here; persisted D state below distinguishes those phases.
                loop {
                    tokio::select! {
                        () = &mut deletion => return Err("orphan deletion completed while producer owned I".to_string()),
                        waiter = sqlx::query_as::<_, (i32, String)>(
                            "SELECT pid, query FROM pg_stat_activity
                             WHERE datname = current_database() AND wait_event_type = 'Lock'
                               AND $1 = ANY(pg_blocking_pids(pid)) ORDER BY pid LIMIT 1",
                        ).bind(producer_pid).fetch_optional(&env.pool) => {
                            if let Some((pid, query)) = waiter.map_err(|error| error.to_string())? {
                                deletion_pid = Some(pid);
                                observed_query = Some(query);
                                return Ok(());
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            deletion_pid = Some(tokio::select! {
                () = &mut deletion => return Err("orphan deletion missed the real state-write gate".to_string()),
                pid = wait_for_blocked_query(&env.pool, gate_pid.unwrap(), "UPDATE machines") => pid,
            });
            tokio::select! {
                () = &mut producer => Err("generic producer changed classification inside admitted orphan phase".to_string()),
                pid = wait_for_blocked_query(&env.pool, deletion_pid.unwrap(),
                    "SELECT segment_id FROM machine_interfaces WHERE id = $1 FOR UPDATE") => {
                    if pid == producer_pid {
                        Ok(())
                    } else {
                        Err(format!("unexpected producer backend: {pid}"))
                    }
                }
            }
        }).await.unwrap_or_else(|error| Err(format!("orphan reference readiness: {error}")));
        let at_gate = observe(&env.pool, host_id, &ids, host_link.id).await;
        let at_gate_scope =
            query_scope(&env.pool, host_id, dpu1_id, instance.id, tenant_segment).await;
        let prefix = if producer_first {
            Some(async {
                let mut probe = env.db_txn().await;
                let dpu = db::machine::try_lock_by_id_for_update(probe.as_mut(), &dpu1_id.into()).await?;
                let history = db::machine::try_lock_machine_state_history(probe.as_mut(), &dpu1_id.into()).await?;
                let routing: bool = sqlx::query_scalar(
                    "SELECT pg_try_advisory_xact_lock(hashtextextended('tenant_prefix_overlap:checks', 0))",
                ).fetch_one(probe.as_mut()).await?;
                probe.rollback().await?;
                Ok::<_, Box<dyn std::error::Error>>((dpu, history, routing))
            }.await)
        } else {
            None
        };
        let gate_release = match gate.take() {
            Some(gate) => Some(gate.rollback().await),
            None => None,
        };
        let producer_finished = timeout(Duration::from_secs(30), async {
            tokio::select! {
                () = &mut producer => {},
                () = &mut deletion => { (&mut producer).await; }
            }
        })
        .await
        .is_ok();
        if !producer_finished {
            for pid in Some(producer_pid).into_iter().chain(deletion_pid) {
                sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                    .bind(pid)
                    .fetch_one(&env.pool)
                    .await
                    .expect("cancel only an observed orphan-reference participant");
            }
            timeout(Duration::from_secs(30), async {
                tokio::join!(&mut producer, &mut deletion);
            })
            .await
            .expect("cancelled orphan-reference participants must join");
        }
        let producer_result = producer.as_mut().take_output().expect("producer joined");
        drop(producer);
        let producer_commit = if producer_result.is_ok() {
            producer_txn.commit().await
        } else {
            producer_txn.rollback().await
        };
        let deletion_finished = timeout(Duration::from_secs(30), &mut deletion)
            .await
            .is_ok();
        if !deletion_finished {
            if let Some(pid) = deletion_pid {
                sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                    .bind(pid)
                    .fetch_one(&env.pool)
                    .await
                    .expect("cancel the observed force-delete participant");
            }
            timeout(Duration::from_secs(30), &mut deletion)
                .await
                .expect("cancelled force-delete must join");
        }
        let deletion_result = deletion
            .as_mut()
            .take_output()
            .expect("force-delete joined");
        let trigger_cleanup = if producer_first {
            Ok(())
        } else {
            sqlx::raw_sql(
                "DROP TRIGGER pause_orphan_reference_deletion ON machines;
                 DROP FUNCTION pause_orphan_reference_deletion();",
            )
            .execute(&env.pool)
            .await
            .map(|_| ())
        };
        let after = observe(&env.pool, host_id, &ids, host_link.id).await;
        let after_scope =
            query_scope(&env.pool, host_id, dpu1_id, instance.id, tenant_segment).await;
        tracing::info!(scenario, ?readiness, ?observed_query, ?prefix,
            ?producer_result, ?deletion_result,
            at_gate_dpu_state = ?at_gate.as_ref().ok().and_then(|view| view.machines.get(&MachineId::from(dpu1_id))).map(|machine| &machine.state),
            "Joined generic preallocation and orphan force-delete participants");
        let checked = catch_unwind(AssertUnwindSafe(|| {
            if let Some(released) = gate_release {
                released.expect("release state-write gate");
            }
            assert!(
                producer_finished && deletion_finished,
                "participants required cancellation"
            );
            producer_result.expect("generic reservation reconciles the existing static interface");
            producer_commit.expect("settle the producer transaction");
            trigger_cleanup.expect("remove the joined orphan state-write gate");
            readiness.expect("observe the actual producer/force-delete boundary");
            let at_gate = at_gate.expect("read committed state while participants are gated");
            let at_gate_scope = at_gate_scope.expect("read committed ownership at the gate");
            let after = after.expect("read joined ownership");
            let after_scope = after_scope.expect("read joined scope");
            assert!(
                before_scope == at_gate_scope,
                "orphan authorization must not commit before the selected references are admitted"
            );
            assert_eq!(at_gate.link.interface_type, InterfaceType::Bmc);
            assert_eq!(after.link.interface_type, InterfaceType::Data);
            assert_eq!(after.link.machine_id, Some(host_id.into()));
            assert_eq!(env.redfish_sim.create_client_calls(), clients_before);
            assert!(
                env.redfish_sim
                    .actions_since(&checkpoint)
                    .all_hosts()
                    .is_empty()
            );
            assert_eq!(
                serde_json::to_value(&before.machines[&MachineId::from(host_id)].history).unwrap(),
                serde_json::to_value(&after.machines[&MachineId::from(host_id)].history).unwrap(),
            );
            if producer_first {
                assert_eq!(prefix.unwrap().unwrap(), (true, true, true));
                assert!(observed_query.as_deref().is_some_and(|query| {
                    query.contains("SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE")
                }));
                assert!(
                    before_scope == after_scope,
                    "fresh host consent must preserve both machines, Instance, addresses and allocations"
                );
                assert_eq!(after.link.attached_dpu_machine_id, Some(dpu1_id));
                assert_eq!(after.dpu_ids, vec![dpu1_id]);
                assert_eq!(
                    serde_json::to_value(&before.machines[&MachineId::from(dpu1_id)].history)
                        .unwrap(),
                    serde_json::to_value(&after.machines[&MachineId::from(dpu1_id)].history)
                        .unwrap(),
                );
                let error = deletion_result
                    .expect_err("newly visible host requires existing Instance consent");
                assert_eq!(error.code(), tonic::Code::FailedPrecondition);
                assert!(
                    error
                        .message()
                        .contains("attached instance must be removed")
                );
            } else {
                // The BMC-only reference was still orphaned when admitted.
                // Existing orphan cleanup is allowed; no host consent is invented.
                let response = deletion_result
                    .expect("admitted orphan deletion succeeds")
                    .into_inner();
                assert!(response.all_done);
                assert!(
                    response.managed_host_machine_id.is_empty() && response.instance_id.is_empty()
                );
                assert_eq!(response.dpu_machine_ids, vec![dpu1_id.to_string()]);
                assert!(!after.machines.contains_key(&MachineId::from(dpu1_id)));
                assert_eq!(after.link.attached_dpu_machine_id, None);
                let host_json = serde_json::to_value(host_id.to_string()).unwrap();
                let host_row = |scope: &PreservedQueryScope| {
                    scope
                        .machines
                        .iter()
                        .find(|row| row["id"] == host_json)
                        .cloned()
                };
                assert_eq!(host_row(&before_scope), host_row(&after_scope));
                assert_eq!(before_scope.instance, after_scope.instance);
                assert_eq!(before_scope.addresses, after_scope.addresses);
                let host_allocations = |scope: &PreservedQueryScope| {
                    scope
                        .allocations
                        .iter()
                        .filter(|row| row["owner_id"] == host_json)
                        .cloned()
                        .collect::<Vec<_>>()
                };
                assert_eq!(
                    host_allocations(&before_scope),
                    host_allocations(&after_scope)
                );
            }
        }));
        if let Err(error) = checked {
            let message = error
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| error.downcast_ref::<&str>().copied())
                .unwrap_or("orphan-reference assertion failed");
            failures.push(format!("{scenario}: {message}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The first ForceDeletion transaction must own its captured attachment set.
/// The Admin-ack path returns after that commit, preserving the admitted targets.
#[crate::sqlx_test]
async fn force_delete_admits_the_current_discovery_cohort(pool: sqlx::PgPool) {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use carbide_uuid::machine::{DpuMachineId, HostMachineId};
    use db::test_support::postgres::wait_for_blocked_query;
    use futures_util::future::maybe_done;
    use model::hardware_info::HardwareInfo;
    use model::machine::machine_id::from_hardware_info;
    use model::resource_pool::common::LOOPBACK_IP_V6;
    use model::resource_pool::{ResourcePoolDef, ResourcePoolType};
    use tokio::time::{Duration, timeout};

    use self::force_delete_observation::{observe, query_scope};
    use crate::tests::common::api_fixtures::dpu::dpu_discover_dhcp;

    #[derive(Clone, Copy)]
    enum Case {
        OwnerFirst,
        DiscoveryFirst,
        QueriedDpuDetaches,
        OrphanDpuAttaches,
    }

    async fn queried_ipv6_allocation(
        pool: &sqlx::PgPool,
        dpu_id: DpuMachineId,
    ) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar("SELECT value FROM resource_pool WHERE name = $1 AND state = $2")
            .bind(LOOPBACK_IP_V6)
            .bind(sqlx::types::Json(
                model::resource_pool::ResourcePoolEntryState::Allocated {
                    owner: dpu_id.to_string(),
                    owner_type: model::resource_pool::OwnerType::Machine.to_string(),
                },
            ))
            .fetch_optional(pool)
            .await
    }

    let env = create_test_env(pool).await;
    let mut setup = env.db_txn().await;
    db::resource_pool::define(
        setup.as_mut(),
        LOOPBACK_IP_V6,
        &ResourcePoolDef {
            pool_type: ResourcePoolType::Ipv6,
            prefix: Some("2001:db8::/124".to_string()),
            ranges: vec![],
            delegate_prefix_len: None,
        },
    )
    .await
    .unwrap();
    setup.commit().await.unwrap();

    let tenant_segment = env.create_vpc_and_tenant_segment().await;
    let mut failures = Vec::new();
    for (scenario, case) in [
        ("force-delete captured first", Case::OwnerFirst),
        ("discovery queued first", Case::DiscoveryFirst),
        (
            "queried DPU detaches during host wait",
            Case::QueriedDpuDetaches,
        ),
        (
            "queried orphan DPU attaches to an allocated host",
            Case::OrphanDpuAttaches,
        ),
    ] {
        let deletion_first = matches!(case, Case::OwnerFirst);
        let queried_dpu = matches!(case, Case::QueriedDpuDetaches);
        let mh = create_managed_host(&env).await;
        let instance = mh
            .instance_builer(&env)
            .single_interface_network_config(tenant_segment)
            .build()
            .await;
        network_configured_with_health(&env, &mh.dpu_ids[0], None).await;
        let host_id: HostMachineId = mh.id.into();
        let dpu1_id = mh.dpu_ids[0];
        let mut setup = env.db_txn().await;
        let snapshot = mh.snapshot(&mut setup).await;
        let host_link = snapshot
            .host_snapshot
            .status
            .interfaces
            .iter()
            .find(|interface| interface.attached_dpu_machine_id == Some(dpu1_id))
            .expect("the fixture host has its D1 link")
            .clone();
        assert!(host_link.primary_interface && !host_link.addresses.is_empty());
        assert!(!snapshot.host_snapshot.config.dpf.used_for_ingestion);
        assert!(!snapshot.use_admin_network());
        assert!(snapshot.managed_host_network_config_version_synced());
        let addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
            setup.as_mut(),
            &instance.id,
            &tenant_segment,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|value| (value.address, value.prefix, value.vpc_id))
        .collect::<Vec<_>>();
        assert!(!addresses.is_empty());
        setup.commit().await.unwrap();

        let standalone_config = env.managed_host_config();
        let mut dpu2_config = standalone_config.get_and_assert_single_dpu().clone();
        dpu2_config.host_mac_address = host_link.mac_address;
        let hardware = HardwareInfo::from(&dpu2_config);
        let dpu2_machine_id = from_hardware_info(&hardware).unwrap();
        let dpu2_id = DpuMachineId::try_from(dpu2_machine_id).unwrap();
        assert_ne!(dpu2_id, dpu1_id);
        let oob_id = dpu_discover_dhcp(&env, &dpu2_config.oob_mac_address.to_string()).await;
        let mut setup = env.db_txn().await;
        let oob = db::machine_interface::find_one(setup.as_mut(), oob_id)
            .await
            .unwrap();
        assert!(oob.machine_id.is_none());
        let dpu2 = db::machine::get_or_create(
            setup.as_mut(),
            Some(&env.common_pools),
            &dpu2_machine_id,
            &oob,
        )
        .await
        .unwrap();
        db::machine_interface::associate_interface_with_dpu_machine(
            &oob_id,
            &dpu2_machine_id,
            setup.as_mut(),
        )
        .await
        .unwrap();
        db::machine_topology::create_or_update(setup.as_mut(), &dpu2_machine_id, &hardware)
            .await
            .unwrap();
        let mut config = dpu2.network_config.value;
        config.loopback_ip = Some(
            db::resource_pool::allocate(
                &env.common_pools.ethernet.pool_loopback_ip,
                setup.as_mut(),
                OwnerType::Machine,
                &dpu2_machine_id.to_string(),
                None,
            )
            .await
            .unwrap(),
        );
        assert!(
            config.loopback_ip_v6.is_some(),
            "IPv6 is enabled and reserved for D2"
        );
        assert_eq!(
            db::machine::try_update_network_config(
                setup.as_mut(),
                &dpu2_machine_id,
                dpu2.network_config.version,
                &config,
            )
            .await
            .unwrap(),
            db::ConditionalWrite::Applied(())
        );
        let ids = [
            MachineId::from(host_id),
            MachineId::from(dpu1_id),
            dpu2_machine_id,
        ];
        assert!(
            db::machine::find_host_by_dpu_machine_id(setup.as_mut(), &dpu2_id)
                .await
                .unwrap()
                .is_none(),
            "D2 must be standalone before discovery"
        );
        setup.commit().await.unwrap();
        let before = observe(&env.pool, host_id, &ids, host_link.id)
            .await
            .unwrap();
        assert_eq!(before.dpu_ids, vec![dpu1_id]);
        assert_eq!(before.link.machine_id, Some(host_id.into()));
        assert_eq!(before.link.attached_dpu_machine_id, Some(dpu1_id));
        let caller = db::machine_interface::find_one(&env.pool, oob_id)
            .await
            .unwrap();
        assert_eq!(caller.machine_id, Some(dpu2_machine_id));
        assert_eq!(caller.mac_address, dpu2_config.oob_mac_address);
        assert!(
            before.machines[&dpu2_machine_id]
                .network_config
                .loopback_ip
                .is_some()
        );
        assert!(
            before.machines[&dpu2_machine_id]
                .network_config
                .loopback_ip_v6
                .is_some()
        );

        if matches!(case, Case::OrphanDpuAttaches) {
            // This advisory is only a deterministic test ordering device. The
            // competing attachment is made by the real Discovery endpoint.
            let mut history_gate = env.db_txn().await;
            assert!(
                db::machine::try_lock_machine_state_history(
                    history_gate.as_mut(),
                    &dpu2_machine_id,
                )
                .await
                .unwrap()
            );
            let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(history_gate.as_mut())
                .await
                .unwrap();
            let update = maybe_done(env.api.admin_force_delete_machine(Request::new(
                AdminForceDeleteMachineRequest {
                    allow_delete_with_instance: false,
                    ..force_delete_request(&dpu2_machine_id)
                },
            )));
            let discovery = maybe_done(env.api.discover_machine(Request::new(
                rpc::forge::MachineDiscoveryInfo {
                    machine_interface_id: Some(oob_id),
                    discovery_data: Some(rpc::DiscoveryData::Info(
                        rpc::DiscoveryInfo::try_from(hardware).unwrap(),
                    )),
                    create_machine: false,
                    ..Default::default()
                },
            )));
            tokio::pin!(update, discovery);
            let mut update_pid = None;
            let readiness = timeout(Duration::from_secs(25), async {
                update_pid = Some(tokio::select! {
                    () = &mut update => return Err("orphan deletion passed its history gate"),
                    pid = wait_for_blocked_query(&env.pool, gate_pid,
                        "SELECT pg_advisory_xact_lock") => pid,
                    pid = wait_for_blocked_query(&env.pool, gate_pid,
                        "INSERT INTO machine_state_history") => pid,
                });
                Ok::<(), &str>(())
            })
            .await;
            let prefix_released = async {
                let mut probe = env.db_txn().await;
                let dpu_free =
                    db::machine::try_lock_by_id_for_update(probe.as_mut(), &dpu2_machine_id)
                        .await?;
                let routing_free: bool = sqlx::query_scalar(
                    "SELECT pg_try_advisory_xact_lock(
                        hashtextextended('tenant_prefix_overlap:checks', 0))",
                )
                .fetch_one(probe.as_mut())
                .await?;
                probe.rollback().await?;
                Ok::<_, Box<dyn std::error::Error>>((dpu_free, routing_free))
            }
            .await;
            let discovery_joined = readiness.as_ref().is_ok_and(|result| result.is_ok())
                && timeout(Duration::from_secs(30), &mut discovery)
                    .await
                    .is_ok();
            let checkpoint = if discovery_joined {
                Some(query_scope(&env.pool, host_id, dpu2_id, instance.id, tenant_segment).await)
            } else {
                None
            };
            let discovery_pid_result: Result<Option<i32>, sqlx::Error> = if let Some(pid) =
                update_pid
            {
                sqlx::query_scalar(
                    "SELECT pid FROM pg_stat_activity
                     WHERE datname = current_database()
                       AND $1 = ANY(pg_blocking_pids(pid))
                       AND strpos(query, 'UPDATE machine_interfaces SET attached_dpu_machine_id') > 0",
                )
                .bind(pid)
                .fetch_optional(&env.pool)
                .await
            } else {
                Ok(None)
            };
            let discovery_pid = discovery_pid_result.as_ref().ok().copied().flatten();
            let released = history_gate.rollback().await;
            let finished = {
                let joined = async {
                    tokio::join!(&mut update, &mut discovery);
                };
                tokio::pin!(joined);
                let finished = timeout(Duration::from_secs(30), &mut joined).await.is_ok();
                if !finished {
                    for pid in update_pid.into_iter().chain(discovery_pid) {
                        sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                            .bind(pid)
                            .fetch_one(&env.pool)
                            .await
                            .expect("cancel only an observed orphan-cohort participant");
                    }
                    timeout(Duration::from_secs(30), &mut joined)
                        .await
                        .expect("cancelled orphan-cohort participants must join");
                }
                finished
            };
            let update_result = update.as_mut().take_output().expect("force-delete joined");
            let discovery_result = discovery.as_mut().take_output().expect("Discovery joined");
            let after_scope =
                query_scope(&env.pool, host_id, dpu2_id, instance.id, tenant_segment).await;
            let after = observe(&env.pool, host_id, &ids, host_link.id).await;
            released.expect("release the explicit test history gate");
            discovery_pid_result.expect("observe any blocked Discovery participant");
            readiness
                .expect("orphan history admission must reach a database wait")
                .expect("observe orphan history admission");
            assert_eq!(prefix_released.unwrap(), (true, true));
            assert!(
                discovery_joined,
                "real Discovery must commit before history is released"
            );
            assert!(finished, "orphan-cohort participants required cancellation");
            assert_eq!(
                discovery_result
                    .expect("real Discovery reattaches D2")
                    .into_inner()
                    .machine_id,
                Some(dpu2_machine_id)
            );
            let error = update_result.expect_err("current allocated-host consent is required");
            assert_eq!(error.code(), tonic::Code::FailedPrecondition);
            assert!(
                error
                    .message()
                    .contains("attached instance must be removed")
            );
            assert!(checkpoint.unwrap().unwrap() == after_scope.unwrap());
            let after = after.unwrap();
            assert_eq!(after.dpu_ids, vec![dpu2_id]);
            assert!(after.machines.contains_key(&dpu2_machine_id));
            assert_ne!(
                after.machines[&dpu2_machine_id].state.value,
                ManagedHostState::ForceDeletion
            );
            continue;
        }

        let query_id = if queried_dpu {
            dpu1_id.into()
        } else {
            MachineId::from(host_id)
        };
        if queried_dpu {
            let mut txn = env.db_txn().await;
            let selected = db::machine::find_by_query(txn.as_mut(), &query_id.to_string())
                .await
                .unwrap()
                .expect("the D query selects D1");
            assert_eq!(selected.id, MachineId::from(dpu1_id));
            assert_eq!(
                db::machine::find_host_by_dpu_machine_id(txn.as_mut(), &dpu1_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .id,
                host_id
            );
            txn.commit().await.unwrap();
        }
        let gate_id = if queried_dpu {
            MachineId::from(host_id)
        } else {
            dpu1_id.into()
        };
        let mut dpu_gate = if deletion_first {
            gate_force_delete_group_write(&env.pool, dpu1_id)
                .await
                .unwrap()
        } else {
            let mut gate = env.pool.begin().await.unwrap();
            db::machine::lock_by_id_for_update(gate.as_mut(), &gate_id)
                .await
                .unwrap();
            gate
        };
        let dpu_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(dpu_gate.as_mut())
            .await
            .unwrap();
        let mut link_gate = if deletion_first || queried_dpu {
            None
        } else {
            Some(env.db_txn().await)
        };
        let link_gate_pid: Option<i32> = if let Some(gate) = &mut link_gate {
            sqlx::query("SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE")
                .bind(host_link.id)
                .execute(gate.as_mut())
                .await
                .unwrap();
            Some(
                sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(gate.as_mut())
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        let update = maybe_done(env.api.admin_force_delete_machine(Request::new(
            AdminForceDeleteMachineRequest {
                wait_for_instance_dpu: true,
                allow_delete_with_instance: !queried_dpu,
                ..force_delete_request(&query_id)
            },
        )));
        let discovery = maybe_done(env.api.discover_machine(tonic::Request::new(
            rpc::forge::MachineDiscoveryInfo {
                machine_interface_id: Some(oob_id),
                discovery_data: Some(rpc::DiscoveryData::Info(
                    rpc::DiscoveryInfo::try_from(hardware).unwrap(),
                )),
                create_machine: false,
                ..Default::default()
            },
        )));
        tokio::pin!(update, discovery);
        let mut update_pid = None;
        let mut discovery_pid = None;
        let mut discovery_blocked = false;
        let mut wait_arm = None;
        let mut queried_discovery_wait = None;
        let readiness = timeout(Duration::from_secs(25), async {
            if queried_dpu {
                update_pid = Some(tokio::select! {
                    () = &mut update => return Err("D-query force-delete passed the host gate".to_string()),
                    pid = wait_for_blocked_query(&env.pool, dpu_gate_pid,
                        "SELECT id FROM machines WHERE id = $1 FOR UPDATE") => pid,
                });
                wait_arm = Some("queried D1 resolved before host wait");
                return timeout(Duration::from_secs(5), async {
                    tokio::select! {
                        () = &mut discovery => Ok(()),
                        waiter = async {
                            loop {
                                let waiter: Option<(i32, serde_json::Value)> = sqlx::query_as(
                                    "SELECT a.pid, jsonb_build_object(
                                         'query', a.query, 'blockers', pg_blocking_pids(a.pid),
                                         'wait_event_type', a.wait_event_type, 'wait_event', a.wait_event,
                                         'locks', (SELECT jsonb_agg(jsonb_build_object(
                                             'locktype', l.locktype, 'mode', l.mode,
                                             'relation', l.relation::regclass::text,
                                             'transactionid', l.transactionid::text,
                                             'granted', l.granted)) FROM pg_locks l WHERE l.pid = a.pid))
                                     FROM pg_stat_activity a
                                     WHERE a.datname = current_database() AND a.state = 'active'
                                       AND a.pid <> $1 AND a.pid <> $2
                                       AND $1 = ANY(pg_blocking_pids(a.pid))
                                     ORDER BY a.pid LIMIT 1",
                                ).bind(dpu_gate_pid).bind(update_pid.unwrap())
                                    .fetch_optional(&env.pool).await
                                    .map_err(|error| format!("observe discovery during H gate: {error:?}"))?;
                                if let Some(waiter) = waiter { return Ok::<_, String>(waiter); }
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                        } => {
                            let (pid, diagnostic) = waiter?;
                            discovery_pid = Some(pid);
                            queried_discovery_wait = Some(diagnostic);
                            Err("complete existing-D Discovery waited on the H gate".to_string())
                        }
                    }
                }).await.unwrap_or_else(|error| Err(format!("discovery independence: {error}")));
            }
            if deletion_first {
                update_pid = Some(tokio::select! {
                    () = &mut update => return Err("force-delete did not reach its captured D1 group write".to_string()),
                    pid = wait_for_blocked_query(&env.pool, dpu_gate_pid, "WITH locked_group") => pid,
                });
                tokio::select! {
                    () = &mut discovery => {},
                    pid = wait_for_blocked_query(&env.pool, update_pid.unwrap(), "UPDATE machine_interfaces SET attached_dpu_machine_id") => {
                        discovery_pid = Some(pid);
                        discovery_blocked = true;
                    }
                }
                wait_arm = Some("captured D1 group write");
            } else {
                discovery_pid = Some(tokio::select! {
                    () = &mut discovery => return Err("discovery passed its interface gate".to_string()),
                    pid = wait_for_blocked_query(&env.pool, link_gate_pid.unwrap(), "UPDATE machine_interfaces SET attached_dpu_machine_id") => pid,
                });
                let (pid, arm) = tokio::select! {
                    () = &mut discovery => return Err("discovery passed its interface gate".to_string()),
                    () = &mut update => return Err("force-delete completed before the discovery cohort was admitted".to_string()),
                    pid = wait_for_blocked_query(&env.pool, dpu_gate_pid, "WITH locked_group") => (pid, "legacy captured D1 group write"),
                    pid = wait_for_blocked_query(&env.pool, discovery_pid.unwrap(), "SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE") => (pid, "isolated interface wait"),
                };
                update_pid = Some(pid);
                wait_arm = Some(arm);
            }
            Ok::<(), String>(())
        }).await.unwrap_or_else(|error| Err(format!("cohort readiness: {error}")));
        let at_gate = observe(&env.pool, host_id, &ids, host_link.id).await;
        let released_prefix = if !deletion_first && !queried_dpu {
            Some(async {
                let mut probe = env.pool.begin().await?;
                let result = async {
                    sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
                        .bind(host_id).execute(probe.as_mut()).await?;
                    let history = db::machine::try_lock_machine_state_history(probe.as_mut(), &host_id.into()).await?;
                    let routing: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended('tenant_prefix_overlap:checks', 0))")
                        .fetch_one(probe.as_mut()).await?;
                    Ok::<_, Box<dyn std::error::Error>>((history, routing))
                }.await;
                probe.rollback().await?;
                result
            }.await)
        } else {
            None
        };
        let link_release = match link_gate.take() {
            Some(gate) => Some(gate.commit().await),
            None => None,
        };
        let discovery_joined_before_dpu_release = if !deletion_first && readiness.is_ok() {
            Some(
                timeout(Duration::from_secs(30), async {
                    tokio::select! {
                        () = &mut discovery => {},
                        () = &mut update => { (&mut discovery).await; }
                    }
                })
                .await,
            )
        } else {
            None
        };
        let after_rebind = if !deletion_first
            && discovery_joined_before_dpu_release
                .as_ref()
                .is_some_and(Result::is_ok)
        {
            Some(observe(&env.pool, host_id, &ids, host_link.id).await)
        } else {
            None
        };
        let queried_before_release = if queried_dpu {
            Some((
                query_scope(&env.pool, host_id, dpu2_id, instance.id, tenant_segment).await,
                queried_ipv6_allocation(&env.pool, dpu1_id).await,
            ))
        } else {
            None
        };
        let external_checkpoint = queried_dpu.then(|| env.redfish_sim.timepoint());
        let clients_before_release = queried_dpu.then(|| env.redfish_sim.create_client_calls());
        let dpu_release = dpu_gate.commit().await;
        let finished = {
            let joined = async {
                tokio::join!(&mut update, &mut discovery);
            };
            tokio::pin!(joined);
            let finished = timeout(Duration::from_secs(30), &mut joined).await.is_ok();
            if !finished {
                for pid in update_pid.into_iter().chain(discovery_pid) {
                    sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                        .bind(pid)
                        .fetch_one(&env.pool)
                        .await
                        .expect("cancel observed cohort writer");
                }
                timeout(Duration::from_secs(30), &mut joined)
                    .await
                    .expect("cancelled force-delete/discovery writers must join");
            }
            finished
        };
        let update_result = update.as_mut().take_output().expect("force-delete joined");
        let discovery_result = discovery.as_mut().take_output().expect("discovery joined");
        let group_gate_cleanup = if deletion_first {
            Some(remove_force_delete_group_write_gate(&env.pool).await)
        } else {
            None
        };
        let after = observe(&env.pool, host_id, &ids, host_link.id)
            .await
            .unwrap();
        tracing::info!(scenario, ?update_pid, ?discovery_pid, ?wait_arm, ?readiness,
            discovery_blocked, ?released_prefix, ?group_gate_cleanup, ?update_result, ?discovery_result,
            before_cohort = ?at_gate.as_ref().map(|value| &value.dpu_ids), after_cohort = ?after.dpu_ids,
            "Joined actual force-delete/discovery cohort participants");
        let mut retained = env.db_txn().await;
        let stored_instance = db::instance::find_by_id(retained.as_mut(), instance.id).await;
        let retained_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
            retained.as_mut(),
            &instance.id,
            &tenant_segment,
        )
        .await;
        retained.commit().await.unwrap();
        let queried_after = if queried_dpu {
            let mut txn = env.db_txn().await;
            let history = db::state_history::for_object(
                txn.as_mut(),
                db::state_history::StateHistoryTableId::Machine,
                &dpu1_id,
            )
            .await;
            let topology =
                db::machine_topology::find_by_machine_ids(txn.as_mut(), &[dpu1_id]).await;
            let closed = txn.commit().await;
            Some((
                query_scope(&env.pool, host_id, dpu2_id, instance.id, tenant_segment).await,
                queried_ipv6_allocation(&env.pool, dpu1_id).await,
                history,
                topology,
                closed,
            ))
        } else {
            None
        };
        tracing::info!(scenario, %query_id, ?queried_discovery_wait,
            host_state = ?after.machines.get(&MachineId::from(host_id)).map(|m| &m.state),
            queried_dpu_present = after.machines.contains_key(&MachineId::from(dpu1_id)),
            current_dpu_state = ?after.machines.get(&dpu2_machine_id).map(|m| &m.state),
            "Joined force-delete query-target observations");
        let checked = catch_unwind(AssertUnwindSafe(|| {
            if let Some(release) = link_release {
                release.expect("release the interface gate");
            }
            dpu_release.expect("release the D1 gate");
            if let Some(cleanup) = group_gate_cleanup {
                cleanup.expect("remove the joined FD group-write gate");
            }
            readiness.expect("observe the actual owner/discovery query arms");
            assert!(finished, "{scenario}: requests required cancellation");
            if queried_dpu {
                assert_eq!(wait_arm, Some("queried D1 resolved before host wait"));
                assert!(
                    queried_discovery_wait.is_none(),
                    "{queried_discovery_wait:?}"
                );
                discovery_joined_before_dpu_release
                    .unwrap()
                    .expect("real Discovery must commit while H remains gated");
                let discovered = discovery_result
                    .expect("existing-D Discovery succeeds")
                    .into_inner();
                assert_eq!(discovered.machine_id, Some(dpu2_machine_id));
                let rebound = after_rebind
                    .unwrap()
                    .expect("capture post-discovery targets before H release");
                assert_eq!(rebound.dpu_ids, vec![dpu2_id]);
                assert_eq!(rebound.link.machine_id, Some(host_id.into()));
                assert_eq!(rebound.link.attached_dpu_machine_id, Some(dpu2_id));
                // Only the queried DPU follows orphan cleanup. Compare its former
                // host's ownership with the checkpoint after discovery committed.
                let (preserved_before, owned_before) = queried_before_release.unwrap();
                let preserved_before =
                    preserved_before.expect("checkpoint former H/D2/Instance ownership");
                assert!(
                    preserved_before.instance.is_some() && !preserved_before.addresses.is_empty()
                );
                let owned_before = owned_before.expect("checkpoint D1 IPv6 owner");
                assert_eq!(
                    owned_before,
                    rebound.machines[&MachineId::from(dpu1_id)]
                        .network_config
                        .loopback_ip_v6
                        .map(|ip| ip.to_string())
                );
                assert!(
                    owned_before.is_some(),
                    "D1 owns its original IPv6 reservation"
                );
                let (preserved_after, owned_after, history, topology, closed) =
                    queried_after.unwrap();
                closed.expect("close joined target inspection");
                let preserved_after =
                    preserved_after.expect("reload former H/D2/Instance ownership");
                let owned_after = owned_after.expect("reload original D1 reservation");
                let history = history.expect("reload deleted D1 history");
                let topology = topology.expect("reload queried D1 topology");
                let response = update_result
                    .expect("orphan D1 deletion succeeds")
                    .into_inner();
                assert!(response.all_done, "orphan path must complete: {response:?}");
                assert!(response.managed_host_machine_id.is_empty());
                assert!(response.managed_host_machine_interface_id.is_empty());
                assert!(response.managed_host_bmc_ip.is_empty());
                assert!(response.instance_id.is_empty());
                assert_eq!(response.dpu_machine_ids, vec![dpu1_id.to_string()]);
                assert_eq!(response.dpu_machine_id, dpu1_id.to_string());
                let expected_interfaces = rebound.machines[&MachineId::from(dpu1_id)]
                    .status
                    .interfaces
                    .iter()
                    .map(|i| i.id.to_string())
                    .collect::<HashSet<_>>();
                assert!(!expected_interfaces.is_empty());
                assert_eq!(
                    response.dpu_machine_interface_ids.len(),
                    expected_interfaces.len()
                );
                assert_eq!(
                    response
                        .dpu_machine_interface_ids
                        .iter()
                        .cloned()
                        .collect::<HashSet<_>>(),
                    expected_interfaces
                );
                assert_eq!(
                    response.dpu_machine_interface_id,
                    response.dpu_machine_interface_ids[0]
                );
                assert!(
                    !after.machines.contains_key(&MachineId::from(dpu1_id)),
                    "the originally queried D1 must be deleted"
                );
                assert!(topology.is_empty());
                assert_eq!(
                    owned_after, None,
                    "only the queried D1 reservation is released"
                );
                assert!(
                    preserved_after == preserved_before,
                    "old H/current D2 Machine rows, Instance, addresses and allocation rows must be unchanged"
                );
                for id in [MachineId::from(host_id), dpu2_machine_id] {
                    let old = &rebound.machines[&id];
                    let current = &after.machines[&id];
                    assert_eq!(current.state.value, old.state.value);
                    assert_eq!(current.state.version, old.state.version);
                    assert_eq!(
                        serde_json::to_value(&current.history).unwrap(),
                        serde_json::to_value(&old.history).unwrap()
                    );
                }
                assert_eq!(after.link.machine_id, Some(host_id.into()));
                assert_eq!(after.link.attached_dpu_machine_id, Some(dpu2_id));
                assert_eq!(after.dpu_ids, vec![dpu2_id]);
                let original = &rebound.machines[&MachineId::from(dpu1_id)];
                assert_eq!(history.len(), original.history.len() + 1);
                assert_eq!(
                    serde_json::to_value(&history[..original.history.len()]).unwrap(),
                    serde_json::to_value(&original.history).unwrap()
                );
                let last = history.last().unwrap();
                assert_eq!(
                    last.state_version.version_nr(),
                    original.state.version.version_nr() + 1
                );
                assert_eq!(
                    serde_json::from_str::<ManagedHostState>(&last.state).unwrap(),
                    ManagedHostState::ForceDeletion
                );
                assert_eq!(
                    env.redfish_sim.create_client_calls(),
                    clients_before_release.unwrap()
                );
                assert!(
                    env.redfish_sim
                        .actions_since(&external_checkpoint.unwrap())
                        .all_hosts()
                        .is_empty()
                );
                return;
            }
            let response = update_result
                .expect("the public force-delete phase succeeds")
                .into_inner();
            assert!(
                !response.all_done,
                "Admin acknowledgement must withhold later cleanup"
            );
            assert_eq!(response.managed_host_machine_id, host_id.to_string());
            assert_eq!(response.instance_id, instance.id.to_string());
            let admitted_dpu = if deletion_first { dpu1_id } else { dpu2_id };
            assert_eq!(response.dpu_machine_ids, vec![admitted_dpu.to_string()]);
            assert_eq!(response.dpu_machine_id, admitted_dpu.to_string());
            let discovered = discovery_result
                .expect("the accepted existing-D discovery succeeds")
                .into_inner();
            assert_eq!(discovered.machine_id, Some(dpu2_machine_id));
            let at_gate = at_gate.expect("observe before the D1 gate releases");
            if deletion_first {
                assert!(
                    discovery_blocked,
                    "{scenario}: discovery committed an attachment change inside force-delete's first captured-cohort phase"
                );
                assert_eq!(at_gate.link.attached_dpu_machine_id, Some(dpu1_id));
                assert_eq!(at_gate.dpu_ids, vec![dpu1_id]);
            } else {
                assert_eq!(wait_arm, Some("isolated interface wait"));
                released_prefix
                    .unwrap()
                    .map(|(history, routing)| {
                        assert!(
                            history && routing,
                            "the rejected attempt must release routing and A(H) before waiting"
                        )
                    })
                    .expect("the rejected attempt releases strong H before waiting on I");
                discovery_joined_before_dpu_release
                    .unwrap()
                    .expect("discovery joins while old D1 remains gated");
                let rebound = after_rebind
                    .unwrap()
                    .expect("observe the committed rebind before releasing old D1");
                assert_eq!(rebound.link.attached_dpu_machine_id, Some(dpu2_id));
                assert_eq!(rebound.dpu_ids, vec![dpu2_id]);
            }
            assert_eq!(after.link.machine_id, Some(host_id.into()));
            assert_eq!(after.link.attached_dpu_machine_id, Some(dpu2_id));
            assert_eq!(after.dpu_ids, vec![dpu2_id]);
            for id in ids {
                let old = &before.machines[&id];
                let current = &after.machines[&id];
                let admitted =
                    id == MachineId::from(host_id) || id == MachineId::from(admitted_dpu);
                assert_eq!(current.config.dpf, old.config.dpf);
                if admitted {
                    assert_eq!(current.state.value, ManagedHostState::ForceDeletion);
                    assert_eq!(
                        current.state.version.version_nr(),
                        old.state.version.version_nr() + 1
                    );
                    assert_eq!(current.history.len(), old.history.len() + 1);
                    assert_eq!(
                        serde_json::to_value(&current.history[..old.history.len()]).unwrap(),
                        serde_json::to_value(&old.history).unwrap()
                    );
                    let last = current.history.last().expect("admitted state history");
                    assert_eq!(last.state_version, current.state.version);
                    assert_eq!(
                        serde_json::from_str::<ManagedHostState>(&last.state).unwrap(),
                        ManagedHostState::ForceDeletion
                    );
                    assert_eq!(
                        current.network_config.version,
                        after.machines[&ids[0]].network_config.version
                    );
                } else {
                    assert_eq!(current.state.value, old.state.value);
                    assert_eq!(current.state.version, old.state.version);
                    assert_eq!(
                        serde_json::to_value(&current.history).unwrap(),
                        serde_json::to_value(&old.history).unwrap()
                    );
                    assert_eq!(current.network_config.value, old.network_config.value);
                    assert_eq!(current.network_config.version, old.network_config.version);
                }
            }
            assert!(
                after.machines[&ids[0]]
                    .network_config
                    .use_admin_network
                    .unwrap_or(false)
            );
            assert!(stored_instance.expect("reload retained Instance").is_some());
            let current_addresses = retained_addresses
                .expect("reload retained tenant addresses")
                .into_iter()
                .map(|value| (value.address, value.prefix, value.vpc_id))
                .collect::<Vec<_>>();
            assert_eq!(current_addresses, addresses);
        }));
        if let Err(panic) = checked {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("cohort assertion failed");
            failures.push(format!("{scenario}: {message}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
