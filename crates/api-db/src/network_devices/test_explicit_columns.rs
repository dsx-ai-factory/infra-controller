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

use carbide_uuid::machine::{MachineIdSource, MachineType};
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use model::network_devices::{NetworkDeviceDiscoveredVia, NetworkDeviceType};
use sqlx::Connection;

use super::*;

#[crate::sqlx_test]
async fn network_device_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_network_device_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API's prepared statements while another connection commits DDL.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE network_devices ADD COLUMN test_added_column text;
         ALTER TABLE port_to_network_device_map ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_network_device_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_network_device_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let dpu_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x79; 32],
        MachineType::Dpu,
    );
    crate::machine::create(
        txn.as_mut(),
        None,
        &dpu_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;

    let lldp = LldpSwitchData {
        id: "mac=02:00:00:00:00:79".to_string(),
        name: "projection-switch".to_string(),
        description: "LLDP switch description".to_string(),
        ip_address: vec!["192.0.2.79".parse()?, "2001:db8::79".parse()?],
        local_port: "p0".to_string(),
        remote_port: "Ethernet1/7".to_string(),
    };
    let mut expected = NetworkDevice {
        id: lldp.id.clone(),
        name: lldp.name.clone(),
        description: Some(lldp.description.clone()),
        ip_addresses: lldp.ip_address.clone(),
        device_type: NetworkDeviceType::Ethernet,
        discovered_via: NetworkDeviceDiscoveredVia::Lldp,
        dpus: vec![],
    };
    let created = get_or_create_network_device(txn.as_mut(), &lldp).await?;
    assert_device(&created, &expected);

    let orphan_lldp = LldpSwitchData {
        id: "mac=02:00:00:00:00:80".to_string(),
        name: "projection-unused-switch".to_string(),
        ip_address: vec![],
        ..lldp.clone()
    };
    get_or_create_network_device(txn.as_mut(), &orphan_lldp).await?;
    sqlx::query("UPDATE network_devices SET description = NULL WHERE id = $1")
        .bind(&orphan_lldp.id)
        .execute(txn.as_mut())
        .await?;
    let orphan = NetworkDevice {
        id: orphan_lldp.id,
        name: orphan_lldp.name,
        description: None,
        ip_addresses: vec![],
        device_type: NetworkDeviceType::Ethernet,
        discovered_via: NetworkDeviceDiscoveredVia::Lldp,
        dpus: vec![],
    };

    let mut expected_map = DpuToNetworkDeviceMap {
        dpu_id: dpu_id.try_into()?,
        local_port: DpuLocalPorts::P0,
        remote_port: "Ethernet1/1".to_string(),
        network_device_id: orphan.id.clone(),
    };
    let inserted = dpu_to_network_device_map::create(
        txn.as_mut(),
        &lldp.local_port,
        &expected_map.remote_port,
        &dpu_id,
        &orphan.id,
    )
    .await?;
    assert_port_map(&inserted, &expected_map);
    expected_map.remote_port = lldp.remote_port;
    expected_map.network_device_id = expected.id.clone();
    let updated = dpu_to_network_device_map::create(
        txn.as_mut(),
        &lldp.local_port,
        &expected_map.remote_port,
        &dpu_id,
        &expected.id,
    )
    .await?;
    assert_port_map(&updated, &expected_map);

    for found in [
        dpu_to_network_device_map::find_by_network_device_id(txn.as_mut(), &expected.id).await?,
        dpu_to_network_device_map::find_by_dpu_ids(txn.as_mut(), &[dpu_id]).await?,
    ] {
        assert_eq!(found.len(), 1);
        assert_port_map(&found[0], &expected_map);
    }

    expected.dpus = vec![expected_map];
    let mut topology = get_topology(txn.as_mut(), ObjectFilter::All).await?;
    topology.network_devices.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(topology.network_devices.len(), 2);
    assert_device(&topology.network_devices[0], &expected);
    assert_device(&topology.network_devices[1], &orphan);

    cleanup_unused_switches(&mut txn).await?;
    let topology = get_topology(txn.as_mut(), ObjectFilter::All).await?;
    assert_eq!(topology.network_devices.len(), 1);
    assert_device(&topology.network_devices[0], &expected);
    dpu_to_network_device_map::delete(&mut txn, &dpu_id).await?;
    assert!(
        dpu_to_network_device_map::find_by_dpu_ids(txn.as_mut(), &[dpu_id])
            .await?
            .is_empty()
    );
    assert!(
        get_topology(txn.as_mut(), ObjectFilter::All)
            .await?
            .network_devices
            .is_empty()
    );

    txn.rollback().await?;
    Ok(())
}

fn assert_device(actual: &NetworkDevice, expected: &NetworkDevice) {
    assert_eq!(actual.id, expected.id);
    assert_eq!(actual.name, expected.name);
    assert_eq!(actual.description, expected.description);
    assert_eq!(actual.ip_addresses, expected.ip_addresses);
    assert!(matches!(actual.device_type, NetworkDeviceType::Ethernet));
    assert!(matches!(
        actual.discovered_via,
        NetworkDeviceDiscoveredVia::Lldp
    ));
    assert_eq!(actual.dpus.len(), expected.dpus.len());
    for (actual, expected) in actual.dpus.iter().zip(&expected.dpus) {
        assert_port_map(actual, expected);
    }
}

fn assert_port_map(actual: &DpuToNetworkDeviceMap, expected: &DpuToNetworkDeviceMap) {
    assert_eq!(actual.dpu_id, expected.dpu_id);
    assert_eq!(
        actual.local_port.to_string(),
        expected.local_port.to_string()
    );
    assert_eq!(actual.remote_port, expected.remote_port);
    assert_eq!(actual.network_device_id, expected.network_device_id);
}
