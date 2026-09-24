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

use common::api_fixtures::create_test_env;
use common::api_fixtures::dpu::create_dpu_machine;
use itertools::Itertools;
use rpc::forge::forge_server::Forge;

use crate::tests::common;
use crate::tests::common::api_fixtures::create_managed_host;
use crate::tests::common::api_fixtures::dpu::dpu_discover_machine;

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
                allow_delete_with_instance_type: false,
                allow_delete_with_instance: false,
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
