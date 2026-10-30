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

use carbide_authn::middleware::Principal;
use carbide_uuid::machine::{DpuMachineId, MachineId};
use common::api_fixtures::create_test_env;
use common::api_fixtures::dpu::create_dpu_machine;
use itertools::Itertools;
use rpc::forge::LldpReportResult::*;
use rpc::forge::forge_server::Forge;

use crate::tests::common;
use crate::tests::common::api_fixtures::dpu::dpu_discover_machine;
use crate::tests::common::api_fixtures::{
    create_managed_host, network_configured_with_health, network_configured_with_lldp,
    try_legacy_network_configured_with_lldp, try_network_configured_with_lldp,
    try_network_configured_with_lldp_fingerprint,
};

#[crate::sqlx_test]
async fn test_lldp_topology(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let _dpu_rpc_machine_id = create_dpu_machine(&env, &host_config).await;

    let topology = env
        .api
        .get_network_topology(tonic::Request::new(rpc::forge::NetworkTopologyRequest {
            id: None,
        }))
        .await?
        .into_inner();

    // values are mentioned at api/src/model/hardware_info/test_data/
    // 3 tors oob_net0, p0, p1
    assert_eq!(topology.network_devices.len(), 3);

    let ids: HashSet<String> = topology
        .network_devices
        .iter()
        .map(|x| x.id.clone())
        .collect();
    let expected_ids = HashSet::from(
        [
            "mac=a1:b1:c1:00:00:01",
            "mac=a2:b2:c2:00:00:02",
            "mac=a3:b3:c3:00:00:03",
        ]
        .map(|x| x.to_string()),
    );

    assert_eq!(ids, expected_ids);

    assert!(!topology.network_devices[0].mgmt_ip.is_empty());
    assert!(!topology.network_devices[1].mgmt_ip.is_empty());
    assert!(!topology.network_devices[2].mgmt_ip.is_empty());

    assert_eq!(topology.network_devices[0].devices.len(), 1);
    assert_eq!(topology.network_devices[1].devices.len(), 1);
    assert_eq!(topology.network_devices[2].devices.len(), 1);

    let ports: HashSet<String> = topology
        .network_devices
        .iter()
        .map(|x| x.devices[0].local_port.clone())
        .collect();
    let expected_ports = HashSet::from(["oob_net0", "p0", "p1"].map(|x| x.to_string()));
    assert_eq!(ports, expected_ports);

    Ok(())
}

#[crate::sqlx_test]
async fn test_lldp_topology_force_delete(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let (dpu_machine_id, _host_machine_id) = create_managed_host(&env).await.into();

    let topology = env
        .api
        .get_network_topology(tonic::Request::new(rpc::forge::NetworkTopologyRequest {
            id: None,
        }))
        .await?
        .into_inner();

    assert_eq!(topology.network_devices[0].devices.len(), 1);
    assert_eq!(topology.network_devices[1].devices.len(), 1);
    assert_eq!(topology.network_devices[2].devices.len(), 1);

    env.api
        .admin_force_delete_machine(tonic::Request::new(
            rpc::forge::AdminForceDeleteMachineRequest {
                host_query: dpu_machine_id.to_string(),
                delete_interfaces: true,
                delete_bmc_interfaces: true,
                delete_bmc_credentials: false,
                allow_delete_with_orphaned_dpf_crds: false,
                delete_bmc_suppressions: false,
                delete_retained_boot_interfaces: false,
                release_preserved_addresses: false,
                wait_for_instance_dpu: false,
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let topology = env
        .api
        .get_network_topology(tonic::Request::new(rpc::forge::NetworkTopologyRequest {
            id: None,
        }))
        .await?
        .into_inner();

    assert!(topology.network_devices.is_empty());

    Ok(())
}

#[crate::sqlx_test]
async fn test_lldp_topology_update(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

    let topology = env
        .api
        .get_network_topology(tonic::Request::new(rpc::forge::NetworkTopologyRequest {
            id: None,
        }))
        .await?
        .into_inner();

    // Verify that there is a valid value before test.
    assert!(
        !topology
            .network_devices
            .iter()
            .filter(|x| x.id == "mac=a1:b1:c1:00:00:01")
            .collect_vec()[0]
            .devices
            .is_empty()
    );

    let mut txn = env.pool.begin().await.unwrap();

    let machine_interface_id =
        db::machine_interface::find_by_machine_ids(&mut txn, &[dpu_machine_id])
            .await
            .unwrap()
            .get(&dpu_machine_id)
            .unwrap()[0]
            .id;

    let query =
        "UPDATE port_to_network_device_map SET network_device_id=NULL WHERE local_port='oob_net0'";
    sqlx::query(query).execute(&mut *txn).await.unwrap();
    let query = "UPDATE network_devices SET id='mac=a1:b1:c1:00:00:11', name='Test' WHERE id='mac=a1:b1:c1:00:00:01'";
    sqlx::query(query).execute(&mut *txn).await.unwrap();
    let query = "UPDATE port_to_network_device_map SET network_device_id='mac=a1:b1:c1:00:00:11' WHERE local_port='oob_net0'";
    sqlx::query(query).execute(&mut *txn).await.unwrap();
    txn.commit().await.unwrap();

    let topology = env
        .api
        .get_network_topology(tonic::Request::new(rpc::forge::NetworkTopologyRequest {
            id: None,
        }))
        .await?
        .into_inner();

    // Verify that db entries are updated with some new values.
    assert!(
        topology
            .network_devices
            .iter()
            .filter(|x| x.id == "mac=a1:b1:c1:00:00:01")
            .collect_vec()
            .is_empty()
    );

    assert!(
        !topology
            .network_devices
            .iter()
            .filter(|x| x.id == "mac=a1:b1:c1:00:00:11")
            .collect_vec()[0]
            .devices
            .is_empty()
    );

    let _dpu_rpc_machine_id = dpu_discover_machine(
        &env,
        host_config.get_and_assert_single_dpu(),
        machine_interface_id,
    )
    .await;

    let topology = env
        .api
        .get_network_topology(tonic::Request::new(rpc::forge::NetworkTopologyRequest {
            id: None,
        }))
        .await?
        .into_inner();

    // Verify that after topology update, everything is proper as it should be.
    assert!(
        !topology
            .network_devices
            .iter()
            .filter(|x| x.id == "mac=a1:b1:c1:00:00:01")
            .collect_vec()[0]
            .devices
            .is_empty()
    );

    assert!(
        topology
            .network_devices
            .iter()
            .filter(|x| x.id == "mac=a1:b1:c1:00:00:11")
            .collect_vec()[0]
            .devices
            .is_empty()
    );

    Ok(())
}

fn neighbor(mac: &str, chassis_mac: &str, remote_port: &str) -> rpc::forge::InterfaceLldp {
    rpc::forge::InterfaceLldp {
        mac_address: mac.to_string(),
        lldp: Some(rpc::machine_discovery::LldpSwitchData {
            name: format!("switch-{chassis_mac}"),
            description: "Example Switch OS".to_string(),
            local_port: "eth0".to_string(),
            ip_address: vec!["192.0.2.10".to_string(), "not-an-ip".to_string()],
            id_type: "mac".to_string(),
            id_value: chassis_mac.to_string(),
            remote_port_type: "ifname".to_string(),
            remote_port_value: remote_port.to_string(),
            ..Default::default()
        }),
    }
}

fn lldp_report(
    result: rpc::forge::LldpReportResult,
    interfaces: Vec<rpc::forge::InterfaceLldp>,
) -> rpc::forge::LldpReport {
    rpc::forge::LldpReport {
        result: result.into(),
        interfaces,
    }
}

/// Send `report` for `machine_id` over the scout RPC, authenticated as `caller`.
async fn scout_report(
    env: &common::api_fixtures::TestEnv,
    caller: Option<&MachineId>,
    machine_id: &MachineId,
    report: rpc::forge::LldpReport,
) -> Result<(), tonic::Status> {
    let mut request = tonic::Request::new(rpc::forge::LldpNeighborReport {
        machine_id: Some(*machine_id),
        report: Some(report),
    });
    if let Some(caller) = caller {
        let mut auth_context = crate::auth::AuthContext::default();
        auth_context
            .principals
            .push(Principal::SpiffeMachineIdentifier(caller.to_string()));
        request.extensions_mut().insert(auth_context);
    }
    env.api.report_lldp_neighbors(request).await.map(|_| ())
}

/// The stored neighbors of `machine_id`, ordered by link.
async fn stored_neighbors(
    env: &common::api_fixtures::TestEnv,
    machine_id: &MachineId,
) -> Vec<model::lldp::LldpNeighbor> {
    db::machine_lldp_neighbor::find_by_machine_ids(&env.pool, &[*machine_id])
        .await
        .unwrap()
        .remove(machine_id)
        .unwrap_or_default()
}

/// The stored links of `machine_id` as `(local MAC, chassis id, remote port)`.
async fn stored_links(
    env: &common::api_fixtures::TestEnv,
    machine_id: &MachineId,
) -> Vec<(String, String, String)> {
    stored_neighbors(env, machine_id)
        .await
        .into_iter()
        .map(|n| {
            (
                n.local_mac_address.to_string(),
                n.chassis_id_value,
                n.remote_port_value,
            )
        })
        .sorted()
        .collect()
}

/// Returns the exact LLDP snapshot identifier exposed to the DPU agent.
async fn accepted_lldp_fingerprint(
    env: &common::api_fixtures::TestEnv,
    dpu_machine_id: DpuMachineId,
) -> Option<Vec<u8>> {
    env.api
        .get_managed_host_network_config(tonic::Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(dpu_machine_id),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .accepted_lldp_snapshot_fingerprint
}

#[crate::sqlx_test]
async fn test_scout_lldp_report_reconciles_stored_neighbors(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host_id: MachineId = **create_managed_host(&env).await.host().id;
    let link = |mac: &str, chassis: &str, port: &str| {
        (mac.to_string(), chassis.to_string(), port.to_string())
    };
    let tor_a = neighbor("A0:00:00:00:00:01", "aa:aa:aa:aa:aa:aa", "swp1");
    let tor_b = neighbor("A0:00:00:00:00:01", "bb:bb:bb:bb:bb:bb", "swp2");
    let tor_c = neighbor("A0:00:00:00:00:02", "cc:cc:cc:cc:cc:cc", "swp3");
    let both_a_b = vec![
        link("A0:00:00:00:00:01", "aa:aa:aa:aa:aa:aa", "swp1"),
        link("A0:00:00:00:00:01", "bb:bb:bb:bb:bb:bb", "swp2"),
        link("A0:00:00:00:00:02", "cc:cc:cc:cc:cc:cc", "swp3"),
    ];

    // Successive reports from one scout, each checked against what is stored afterwards.
    let steps = [
        (
            "two neighbors on one port are both stored",
            lldp_report(Updated, vec![tor_a.clone(), tor_b.clone(), tor_c.clone()]),
            both_a_b.clone(),
        ),
        (
            "unchanged keeps the stored neighbors",
            lldp_report(Unchanged, vec![]),
            both_a_b.clone(),
        ),
        (
            "failed collection keeps the last known neighbors",
            lldp_report(CollectionFailed, vec![]),
            both_a_b.clone(),
        ),
        (
            "a changed snapshot replaces the stored set",
            lldp_report(Updated, vec![tor_c.clone()]),
            vec![link("A0:00:00:00:00:02", "cc:cc:cc:cc:cc:cc", "swp3")],
        ),
        (
            "an empty snapshot clears the stored set",
            lldp_report(Updated, vec![]),
            vec![],
        ),
    ];
    for (name, report, expected) in steps {
        scout_report(&env, Some(&host_id), &host_id, report)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(stored_links(&env, &host_id).await, expected, "{name}");
    }

    // A change to a non-key field alone is a changed snapshot, so the stored row is rewritten.
    scout_report(
        &env,
        Some(&host_id),
        &host_id,
        lldp_report(Updated, vec![tor_c.clone()]),
    )
    .await
    .unwrap();
    let mut renamed_tor_c = tor_c.clone();
    renamed_tor_c.lldp.as_mut().unwrap().name = "tor-c-renamed".to_string();
    scout_report(
        &env,
        Some(&host_id),
        &host_id,
        lldp_report(Updated, vec![renamed_tor_c]),
    )
    .await
    .unwrap();
    let stored = stored_neighbors(&env, &host_id).await;
    assert_eq!(
        stored
            .iter()
            .map(|n| n.system_name.as_str())
            .collect::<Vec<_>>(),
        ["tor-c-renamed"]
    );

    // A rejected report leaves the stored neighbors untouched.
    let before_rejected = stored_neighbors(&env, &host_id).await;
    let rejected = [
        ("unspecified result", lldp_report(Unspecified, vec![])),
        (
            "invalid local MAC",
            lldp_report(Updated, vec![neighbor("not-a-mac", "aa", "swp1")]),
        ),
        (
            "interface without LLDP data",
            lldp_report(
                Updated,
                vec![rpc::forge::InterfaceLldp {
                    mac_address: "A0:00:00:00:00:01".to_string(),
                    lldp: None,
                }],
            ),
        ),
    ];
    for (name, report) in rejected {
        let status = scout_report(&env, Some(&host_id), &host_id, report)
            .await
            .expect_err(name);
        assert_eq!(status.code(), tonic::Code::InvalidArgument, "{name}");
        assert_eq!(
            stored_neighbors(&env, &host_id).await,
            before_rejected,
            "{name}"
        );
    }

    // A repeated link is not validated up front: the primary key rejects it and the
    // transaction rolls back, so the stored neighbors survive the failed replace.
    scout_report(
        &env,
        Some(&host_id),
        &host_id,
        lldp_report(Updated, vec![tor_a.clone(), tor_a.clone()]),
    )
    .await
    .expect_err("repeated link");
    assert_eq!(stored_neighbors(&env, &host_id).await, before_rejected);

    // Bond slaves share a MAC, so links that differ only by local port are distinct.
    let mut tor_a_eth1 = tor_a.clone();
    tor_a_eth1.lldp.as_mut().unwrap().local_port = "eth1".to_string();
    scout_report(
        &env,
        Some(&host_id),
        &host_id,
        lldp_report(Updated, vec![tor_a.clone(), tor_a_eth1]),
    )
    .await
    .unwrap();
    assert_eq!(
        stored_neighbors(&env, &host_id)
            .await
            .iter()
            .map(|n| n.local_port.as_str())
            .collect::<Vec<_>>(),
        ["eth0", "eth1"]
    );
}

/// Proves rejected LLDP leaves Core's accepted snapshot unchanged while newer
/// operational status commits.
#[crate::sqlx_test]
async fn test_dpu_lldp_rejection_commits_status_and_preserves_neighbors(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_id = create_managed_host(&env).await.dpu().id;
    let prior_neighbor = neighbor("B0:00:00:00:00:01", "bb:bb:bb:bb:bb:bb", "swp9");

    // Seed an accepted LLDP snapshot.
    network_configured_with_lldp(
        &env,
        &dpu_id,
        lldp_report(Updated, vec![prior_neighbor.clone()]),
    )
    .await;
    let accepted_before = accepted_lldp_fingerprint(&env, dpu_id)
        .await
        .expect("seeded snapshot must be accepted");
    let links_before = stored_links(&env, &dpu_id).await;

    let cases = [
        // Invalid data fails before SQL and must not poison the outer transaction.
        (
            "validation rejection",
            lldp_report(Updated, vec![neighbor("not-a-mac", "aa", "swp1")]),
        ),
        // A duplicate key fails after replacement starts and must restore deleted rows.
        (
            "SQL rejection after replacement starts",
            lldp_report(
                Updated,
                vec![prior_neighbor.clone(), prior_neighbor.clone()],
            ),
        ),
    ];
    for (name, report) in cases {
        // Make each attempted status update independently observable.
        sqlx::query(
            "UPDATE machines
             SET network_status_observation =
                 jsonb_set(
                     network_status_observation,
                     '{agent_version}',
                     to_jsonb('older-test-agent'::text)
                 )
             WHERE id = $1",
        )
        .bind(dpu_id)
        .execute(&env.pool)
        .await
        .unwrap();

        // A fingerprint-aware agent may receive RPC success after LLDP rollback.
        try_network_configured_with_lldp(&env, &dpu_id, report)
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"));

        // The outer status committed, while savepoint rollback preserved both
        // neighbors and the exact accepted-snapshot identifier.
        let agent_version: Option<String> = sqlx::query_scalar(
            "SELECT network_status_observation->>'agent_version' FROM machines WHERE id = $1",
        )
        .bind(dpu_id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
        assert_eq!(
            agent_version.as_deref(),
            Some(crate::tests::common::api_fixtures::dpu::TEST_DPU_AGENT_VERSION),
            "{name}"
        );
        assert_eq!(stored_links(&env, &dpu_id).await, links_before, "{name}");
        assert_eq!(
            accepted_lldp_fingerprint(&env, dpu_id).await,
            Some(accepted_before.clone()),
            "{name}"
        );
    }
}

/// Proves an ordinary newer status report cannot clear accepted LLDP state
/// when it carries no LLDP attachment.
#[crate::sqlx_test]
async fn test_dpu_status_without_lldp_preserves_accepted_snapshot(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_id = create_managed_host(&env).await.dpu().id;
    let snapshot_a = neighbor("B0:00:00:00:00:01", "bb:bb:bb:bb:bb:bb", "swp9");

    // Accept snapshot A and retain both forms of its persisted state.
    network_configured_with_lldp(
        &env,
        &dpu_id,
        lldp_report(Updated, vec![snapshot_a.clone()]),
    )
    .await;
    let fingerprint_a = ::rpc::lldp_snapshot::fingerprint(std::slice::from_ref(&snapshot_a));
    let links_a = stored_links(&env, &dpu_id).await;
    assert_eq!(
        accepted_lldp_fingerprint(&env, dpu_id).await,
        Some(fingerprint_a.clone())
    );

    // Make the next ordinary status update distinguishable, then submit it
    // without an LLDP attachment.
    sqlx::query(
        "UPDATE machines
         SET network_status_observation =
             jsonb_set(
                 network_status_observation,
                 '{agent_version}',
                 to_jsonb('older-test-agent'::text)
             )
         WHERE id = $1",
    )
    .bind(dpu_id)
    .execute(&env.pool)
    .await
    .unwrap();
    network_configured_with_health(&env, &dpu_id, None).await;

    // Operational status advances while snapshot A remains accepted intact.
    let agent_version: Option<String> = sqlx::query_scalar(
        "SELECT network_status_observation->>'agent_version' FROM machines WHERE id = $1",
    )
    .bind(dpu_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        agent_version.as_deref(),
        Some(crate::tests::common::api_fixtures::dpu::TEST_DPU_AGENT_VERSION)
    );
    assert_eq!(stored_links(&env, &dpu_id).await, links_a);
    assert_eq!(
        accepted_lldp_fingerprint(&env, dpu_id).await,
        Some(fingerprint_a)
    );
}

/// Proves a mismatched identifier rejects snapshot B without losing status or
/// snapshot A, and that a correctly identified retry can subsequently commit B.
#[crate::sqlx_test]
async fn test_dpu_lldp_fingerprint_mismatch_preserves_a_until_b_retries(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_id = create_managed_host(&env).await.dpu().id;
    let snapshot_a = neighbor("B0:00:00:00:00:01", "aa:aa:aa:aa:aa:aa", "swp1");
    let snapshot_b = neighbor("B0:00:00:00:00:02", "bb:bb:bb:bb:bb:bb", "swp2");

    // Establish snapshot A as the accepted baseline.
    network_configured_with_lldp(
        &env,
        &dpu_id,
        lldp_report(Updated, vec![snapshot_a.clone()]),
    )
    .await;
    let fingerprint_a = ::rpc::lldp_snapshot::fingerprint(std::slice::from_ref(&snapshot_a));
    let fingerprint_b = ::rpc::lldp_snapshot::fingerprint(std::slice::from_ref(&snapshot_b));
    let links_a = stored_links(&env, &dpu_id).await;

    // Make the operational write observable and send valid B under a different
    // synthetic identifier.
    sqlx::query(
        "UPDATE machines
         SET network_status_observation =
             jsonb_set(
                 network_status_observation,
                 '{agent_version}',
                 to_jsonb('older-test-agent'::text)
             )
         WHERE id = $1",
    )
    .bind(dpu_id)
    .execute(&env.pool)
    .await
    .unwrap();
    let incorrect_fingerprint = vec![0x5a; fingerprint_b.len()];
    assert_ne!(incorrect_fingerprint, fingerprint_b);
    try_network_configured_with_lldp_fingerprint(
        &env,
        &dpu_id,
        lldp_report(Updated, vec![snapshot_b.clone()]),
        incorrect_fingerprint,
    )
    .await
    .unwrap();

    // The outer status commits, but rejected B cannot replace A or its
    // accepted fingerprint.
    let agent_version: Option<String> = sqlx::query_scalar(
        "SELECT network_status_observation->>'agent_version' FROM machines WHERE id = $1",
    )
    .bind(dpu_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        agent_version.as_deref(),
        Some(crate::tests::common::api_fixtures::dpu::TEST_DPU_AGENT_VERSION)
    );
    assert_eq!(stored_links(&env, &dpu_id).await, links_a);
    assert_eq!(
        accepted_lldp_fingerprint(&env, dpu_id).await,
        Some(fingerprint_a)
    );

    // Retry B with the production-computed fingerprint.
    try_network_configured_with_lldp(
        &env,
        &dpu_id,
        lldp_report(Updated, vec![snapshot_b.clone()]),
    )
    .await
    .unwrap();

    // Correctly identified B now owns both neighbors and acceptance.
    assert_eq!(
        stored_links(&env, &dpu_id).await,
        vec![(
            "B0:00:00:00:00:02".to_string(),
            "bb:bb:bb:bb:bb:bb".to_string(),
            "swp2".to_string(),
        )]
    );
    assert_eq!(
        accepted_lldp_fingerprint(&env, dpu_id).await,
        Some(fingerprint_b)
    );
}

/// Proves a successful empty snapshot clears neighbors and is acknowledged
/// with its own stable identifier.
#[crate::sqlx_test]
async fn test_dpu_empty_lldp_snapshot_is_acknowledged(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_id = create_managed_host(&env).await.dpu().id;

    // Seed one neighbor, then successfully replace it with an empty snapshot.
    network_configured_with_lldp(
        &env,
        &dpu_id,
        lldp_report(
            Updated,
            vec![neighbor("B0:00:00:00:00:01", "bb:bb:bb:bb:bb:bb", "swp9")],
        ),
    )
    .await;
    network_configured_with_lldp(&env, &dpu_id, lldp_report(Updated, vec![])).await;

    // Empty is accepted as a real topology, not confused with no report.
    assert!(stored_links(&env, &dpu_id).await.is_empty());
    assert_eq!(
        accepted_lldp_fingerprint(&env, dpu_id).await,
        Some(::rpc::lldp_snapshot::fingerprint(&[]))
    );
}

/// Proves collection failure retains the accepted snapshot so recovery with
/// the same topology remains safely unchanged.
#[crate::sqlx_test]
async fn test_dpu_lldp_collection_failure_and_recovery_preserve_acceptance(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_id = create_managed_host(&env).await.dpu().id;
    let prior_neighbor = neighbor("B0:00:00:00:00:01", "bb:bb:bb:bb:bb:bb", "swp9");

    // Commit one snapshot, then report that the next collection failed.
    network_configured_with_lldp(
        &env,
        &dpu_id,
        lldp_report(Updated, vec![prior_neighbor.clone()]),
    )
    .await;
    let accepted = ::rpc::lldp_snapshot::fingerprint(std::slice::from_ref(&prior_neighbor));
    assert_eq!(
        accepted_lldp_fingerprint(&env, dpu_id).await,
        Some(accepted.clone())
    );
    network_configured_with_lldp(&env, &dpu_id, lldp_report(CollectionFailed, vec![])).await;

    // Recover with a matching UNCHANGED fingerprint, as the agent does when
    // Core's retained acceptance still identifies its current topology.
    try_network_configured_with_lldp(&env, &dpu_id, lldp_report(Unchanged, vec![]))
        .await
        .unwrap();

    // Failure and recovery neither erase nor rewrite the retained topology.
    assert_eq!(
        accepted_lldp_fingerprint(&env, dpu_id).await,
        Some(accepted)
    );
    assert_eq!(
        stored_links(&env, &dpu_id).await,
        vec![(
            "B0:00:00:00:00:01".to_string(),
            "bb:bb:bb:bb:bb:bb".to_string(),
            "swp9".to_string(),
        )]
    );
}

/// Proves a mismatched UNCHANGED fingerprint rejects LLDP without rolling back
/// newer operational status or disturbing Core's accepted snapshot.
#[crate::sqlx_test]
async fn test_dpu_mismatched_unchanged_fingerprint_preserves_status_and_acceptance(
    pool: sqlx::PgPool,
) {
    let env = create_test_env(pool).await;
    let dpu_id = create_managed_host(&env).await.dpu().id;
    let prior_neighbor = neighbor("B0:00:00:00:00:01", "bb:bb:bb:bb:bb:bb", "swp9");

    // Establish the accepted topology and make the next status write observable.
    network_configured_with_lldp(
        &env,
        &dpu_id,
        lldp_report(Updated, vec![prior_neighbor.clone()]),
    )
    .await;
    let accepted = ::rpc::lldp_snapshot::fingerprint(std::slice::from_ref(&prior_neighbor));
    let links_before = stored_links(&env, &dpu_id).await;
    sqlx::query(
        "UPDATE machines
         SET network_status_observation =
             jsonb_set(
                 network_status_observation,
                 '{agent_version}',
                 to_jsonb('older-test-agent'::text)
             )
         WHERE id = $1",
    )
    .bind(dpu_id)
    .execute(&env.pool)
    .await
    .unwrap();

    // A fingerprint-aware caller receives RPC success even though the
    // mismatched LLDP portion rolls back to its savepoint.
    let mut mismatched = accepted.clone();
    mismatched[0] ^= 0xff;
    try_network_configured_with_lldp_fingerprint(
        &env,
        &dpu_id,
        lldp_report(Unchanged, vec![]),
        mismatched,
    )
    .await
    .unwrap();

    // The outer status commits while topology and acceptance retain their
    // previously committed values.
    let agent_version: Option<String> = sqlx::query_scalar(
        "SELECT network_status_observation->>'agent_version' FROM machines WHERE id = $1",
    )
    .bind(dpu_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        agent_version.as_deref(),
        Some(crate::tests::common::api_fixtures::dpu::TEST_DPU_AGENT_VERSION)
    );
    assert_eq!(stored_links(&env, &dpu_id).await, links_before);
    assert_eq!(
        accepted_lldp_fingerprint(&env, dpu_id).await,
        Some(accepted)
    );
}

/// Proves a supplied UNCHANGED fingerprint cannot create acceptance when Core
/// has never committed an LLDP snapshot for the DPU.
#[crate::sqlx_test]
async fn test_dpu_unchanged_fingerprint_without_prior_acceptance_is_rejected(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_id = create_managed_host(&env).await.dpu().id;

    // Submit a fingerprint-aware UNCHANGED claim before any snapshot exists.
    try_network_configured_with_lldp_fingerprint(
        &env,
        &dpu_id,
        lldp_report(Unchanged, vec![]),
        vec![0x0a; 32],
    )
    .await
    .unwrap();

    // Rejection leaves LLDP state absent while the surrounding operational
    // status still commits.
    let agent_version: Option<String> = sqlx::query_scalar(
        "SELECT network_status_observation->>'agent_version' FROM machines WHERE id = $1",
    )
    .bind(dpu_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        agent_version.as_deref(),
        Some(crate::tests::common::api_fixtures::dpu::TEST_DPU_AGENT_VERSION)
    );
    assert!(stored_links(&env, &dpu_id).await.is_empty());
    assert_eq!(accepted_lldp_fingerprint(&env, dpu_id).await, None);
}

/// Proves legacy UNCHANGED remains compatible when the older agent omits the
/// fingerprint that newer agents use for exact snapshot validation.
#[crate::sqlx_test]
async fn test_legacy_dpu_unchanged_without_fingerprint_remains_compatible(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_id = create_managed_host(&env).await.dpu().id;
    let prior_neighbor = neighbor("B0:00:00:00:00:01", "bb:bb:bb:bb:bb:bb", "swp9");

    // Establish one accepted snapshot before exercising the legacy omission.
    network_configured_with_lldp(
        &env,
        &dpu_id,
        lldp_report(Updated, vec![prior_neighbor.clone()]),
    )
    .await;
    let accepted = ::rpc::lldp_snapshot::fingerprint(std::slice::from_ref(&prior_neighbor));
    let links_before = stored_links(&env, &dpu_id).await;

    // The original client shape sends UNCHANGED without a fingerprint.
    try_legacy_network_configured_with_lldp(&env, &dpu_id, lldp_report(Unchanged, vec![]))
        .await
        .unwrap();

    // Legacy acceptance remains a no-op that preserves the committed snapshot.
    assert_eq!(stored_links(&env, &dpu_id).await, links_before);
    assert_eq!(
        accepted_lldp_fingerprint(&env, dpu_id).await,
        Some(accepted)
    );
}

/// Proves a legacy agent still receives a non-OK result after operational
/// status commits, preventing it from treating rejected LLDP as accepted.
#[crate::sqlx_test]
async fn test_legacy_dpu_lldp_rejection_returns_error_after_status_commit(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_id = create_managed_host(&env).await.dpu().id;
    let prior_neighbor = neighbor("B0:00:00:00:00:01", "bb:bb:bb:bb:bb:bb", "swp9");

    // Seed last-known-good neighbors and mark the stored status as older.
    network_configured_with_lldp(
        &env,
        &dpu_id,
        lldp_report(Updated, vec![prior_neighbor.clone()]),
    )
    .await;
    sqlx::query(
        "UPDATE machines
         SET network_status_observation =
             jsonb_set(
                 network_status_observation,
                 '{agent_version}',
                 to_jsonb('older-test-agent'::text)
             )
         WHERE id = $1",
    )
    .bind(dpu_id)
    .execute(&env.pool)
    .await
    .unwrap();
    let links_before = stored_links(&env, &dpu_id).await;

    // Use the original RPC and reject malformed LLDP.
    let status = try_legacy_network_configured_with_lldp(
        &env,
        &dpu_id,
        lldp_report(Updated, vec![neighbor("not-a-mac", "aa", "swp1")]),
    )
    .await
    .expect_err("legacy client must receive LLDP rejection");

    // The legacy error preserves cache safety without rolling back status.
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    let agent_version: Option<String> = sqlx::query_scalar(
        "SELECT network_status_observation->>'agent_version' FROM machines WHERE id = $1",
    )
    .bind(dpu_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        agent_version.as_deref(),
        Some(crate::tests::common::api_fixtures::dpu::TEST_DPU_AGENT_VERSION)
    );
    assert_eq!(stored_links(&env, &dpu_id).await, links_before);
    assert_eq!(
        accepted_lldp_fingerprint(&env, dpu_id).await,
        Some(::rpc::lldp_snapshot::fingerprint(&[prior_neighbor]))
    );
}

#[crate::sqlx_test]
async fn test_scout_lldp_report_is_bound_to_caller_identity(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let host_id: MachineId = **mh.host().id;
    let dpu_id: MachineId = mh.dpu().id.into();
    let report = || {
        lldp_report(
            rpc::forge::LldpReportResult::Updated,
            vec![neighbor("A0:00:00:00:00:01", "aa:aa:aa:aa:aa:aa", "swp1")],
        )
    };

    let cases = [
        (
            "another machine's identity",
            Some(&dpu_id),
            tonic::Code::PermissionDenied,
        ),
        ("no machine identity", None, tonic::Code::Unauthenticated),
    ];
    for (name, caller, code) in cases {
        let status = scout_report(&env, caller, &host_id, report())
            .await
            .expect_err(name);
        assert_eq!(status.code(), code, "{name}");
    }
    assert!(stored_links(&env, &host_id).await.is_empty());
}

#[crate::sqlx_test]
async fn test_lldp_neighbors_are_returned_by_find_machines_by_ids(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let host_id: MachineId = **mh.host().id;
    let dpu_id = mh.dpu().id;

    // MED inventory and a MAC that discovery never listed must both survive the round trip.
    let mut host_neighbor = neighbor("A0:00:00:00:00:01", "aa:aa:aa:aa:aa:aa", "swp1");
    host_neighbor.lldp.as_mut().unwrap().med_inventory =
        Some(rpc::machine_discovery::LldpMedInventory {
            serial: Some("SN123".to_string()),
            manufacturer: Some("Example".to_string()),
            model: None,
        });
    let dpu_neighbor = neighbor("B0:00:00:00:00:01", "bb:bb:bb:bb:bb:bb", "swp9");

    scout_report(
        &env,
        Some(&host_id),
        &host_id,
        lldp_report(
            rpc::forge::LldpReportResult::Updated,
            vec![host_neighbor.clone()],
        ),
    )
    .await
    .unwrap();
    network_configured_with_lldp(
        &env,
        &dpu_id,
        lldp_report(
            rpc::forge::LldpReportResult::Updated,
            vec![dpu_neighbor.clone()],
        ),
    )
    .await;

    let machines = env
        .api
        .find_machines_by_ids(tonic::Request::new(rpc::forge::MachinesByIdsRequest {
            machine_ids: vec![host_id, dpu_id.into()],
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .machines;
    let neighbors_of = |id: MachineId| {
        machines
            .iter()
            .find(|m| m.id == Some(id))
            .and_then(|m| m.status.as_ref())
            .map(|s| s.lldp_neighbors.clone())
            .unwrap()
    };
    assert_eq!(neighbors_of(host_id), vec![host_neighbor]);
    assert_eq!(neighbors_of(dpu_id.into()), vec![dpu_neighbor]);
}

#[crate::sqlx_test]
async fn test_lldp_neighbors_force_delete(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let host_id: MachineId = **mh.host().id;
    network_configured_with_lldp(
        &env,
        &mh.dpu().id,
        lldp_report(
            rpc::forge::LldpReportResult::Updated,
            vec![neighbor("B0:00:00:00:00:01", "bb:bb:bb:bb:bb:bb", "swp9")],
        ),
    )
    .await;
    scout_report(
        &env,
        Some(&host_id),
        &host_id,
        lldp_report(
            rpc::forge::LldpReportResult::Updated,
            vec![neighbor("A0:00:00:00:00:01", "aa:aa:aa:aa:aa:aa", "swp1")],
        ),
    )
    .await
    .unwrap();

    env.api
        .admin_force_delete_machine(tonic::Request::new(
            rpc::forge::AdminForceDeleteMachineRequest {
                host_query: host_id.to_string(),
                delete_interfaces: true,
                delete_bmc_interfaces: true,
                delete_bmc_credentials: false,
                allow_delete_with_orphaned_dpf_crds: false,
                delete_bmc_suppressions: false,
                delete_retained_boot_interfaces: false,
                release_preserved_addresses: false,
                wait_for_instance_dpu: false,
            },
        ))
        .await
        .unwrap();

    let remaining =
        db::machine_lldp_neighbor::find_by_machine_ids(&env.pool, &[host_id, mh.dpu().id.into()])
            .await
            .unwrap();
    assert!(remaining.is_empty());
}
