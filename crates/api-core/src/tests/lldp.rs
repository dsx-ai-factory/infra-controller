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
use carbide_uuid::machine::MachineId;
use common::api_fixtures::create_test_env;
use common::api_fixtures::dpu::create_dpu_machine;
use itertools::Itertools;
use rpc::forge::LldpReportResult::*;
use rpc::forge::forge_server::Forge;

use crate::tests::common;
use crate::tests::common::api_fixtures::dpu::dpu_discover_machine;
use crate::tests::common::api_fixtures::{
    create_managed_host, network_configured_with_lldp, try_network_configured_with_lldp,
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

#[crate::sqlx_test]
async fn test_dpu_lldp_report_rejection_fails_network_status(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_id = create_managed_host(&env).await.dpu().id;
    network_configured_with_lldp(
        &env,
        &dpu_id,
        lldp_report(
            Updated,
            vec![neighbor("B0:00:00:00:00:01", "bb:bb:bb:bb:bb:bb", "swp9")],
        ),
    )
    .await;
    let stored_observation = || async {
        sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT network_status_observation FROM machines WHERE id = $1",
        )
        .bind(dpu_id)
        .fetch_one(&env.pool)
        .await
        .unwrap()
    };
    let observation_before = stored_observation().await;
    let links_before = stored_links(&env, &dpu_id).await;

    let status = try_network_configured_with_lldp(
        &env,
        &dpu_id,
        lldp_report(Updated, vec![neighbor("not-a-mac", "aa", "swp1")]),
    )
    .await
    .expect_err("rejected LLDP report");

    // The LLDP write shares the status transaction, so the network status rolls back with it.
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert_eq!(stored_observation().await, observation_before);
    assert_eq!(stored_links(&env, &dpu_id).await, links_before);
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
