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
use std::ops::DerefMut;

use carbide_uuid::machine::MachineId;
use itertools::Itertools;
use model::hardware_info::LldpSwitchData;
use model::network_devices::{
    DpuLocalPorts, DpuToNetworkDeviceMap, LldpError, NetworkDevice, NetworkTopologyData,
};
use sqlx::{PgConnection, PgTransaction};

use super::{DatabaseError, ObjectFilter};
use crate::db_read::DbReader;

#[cfg(test)]
mod test_explicit_columns;

pub struct NetworkDeviceSearchConfig {
    include_dpus: bool,
}

impl NetworkDeviceSearchConfig {
    pub fn new(include_dpus: bool) -> Self {
        NetworkDeviceSearchConfig { include_dpus }
    }
}

fn get_port_data<'a>(
    data: &'a [LldpSwitchData],
    port: &DpuLocalPorts,
) -> Result<&'a LldpSwitchData, LldpError> {
    let port_data = data
        .iter()
        .filter(|x| x.local_port == port.to_string())
        .collect::<Vec<&LldpSwitchData>>();

    let port_data = port_data
        .first()
        .ok_or(LldpError::MissingPort(port.to_string()))?;

    Ok(*port_data)
}

pub async fn find<DB>(
    txn: &mut DB,
    filter: ObjectFilter<'_, &str>,
    search_config: &NetworkDeviceSearchConfig,
) -> Result<Vec<NetworkDevice>, DatabaseError>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let mut query = sqlx::QueryBuilder::new(
        "SELECT l.id, l.name, l.description, l.ip_addresses, l.device_type, l.discovered_via
         FROM network_devices l",
    );

    let mut devices: Vec<NetworkDevice> = match filter {
        ObjectFilter::All => query
            .build_query_as::<NetworkDevice>()
            .fetch_all(&mut *txn)
            .await
            .map_err(|e| DatabaseError::new("network_devices All", e)),
        ObjectFilter::One(id) => {
            query.push(" WHERE l.id = ");
            query.push_bind(id.to_string());
            query
                .build_query_as::<NetworkDevice>()
                .fetch_all(&mut *txn)
                .await
                .map_err(|e| DatabaseError::new("network_devices One", e))
        }
        ObjectFilter::List(list) => {
            let str_list: Vec<String> = list.iter().map(|id| id.to_string()).collect();
            query.push(" WHERE l.id = ANY(");
            query.push_bind(str_list);
            query.push(")");
            query
                .build_query_as::<NetworkDevice>()
                .fetch_all(&mut *txn)
                .await
                .map_err(|e| DatabaseError::new("network_devices List", e))
        }
    }?;

    if search_config.include_dpus {
        for device in &mut devices {
            device.dpus =
                dpu_to_network_device_map::find_by_network_device_id(&mut *txn, device.id())
                    .await?;
        }
    }

    Ok(devices)
}

async fn create(
    txn: &mut PgConnection,
    data: &LldpSwitchData,
) -> Result<NetworkDevice, DatabaseError> {
    let query = "INSERT INTO network_devices(id, name, description, ip_addresses)
                 VALUES($1, $2, $3, $4::inet[])
                 RETURNING id, name, description, ip_addresses, device_type, discovered_via";

    sqlx::query_as(query)
        .bind(&data.id)
        .bind(&data.name)
        .bind(&data.description)
        .bind(&data.ip_address)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn get_or_create_network_device(
    txn: &mut PgConnection,
    data: &LldpSwitchData,
) -> Result<NetworkDevice, DatabaseError> {
    let network_device = find(
        &mut *txn,
        ObjectFilter::One(&data.id),
        &NetworkDeviceSearchConfig::new(false),
    )
    .await?;

    if !network_device.is_empty() {
        return Ok(network_device[0].clone());
    }

    create(txn, data).await
}

pub async fn lock_network_device_table(txn: &mut PgTransaction<'_>) -> Result<(), DatabaseError> {
    let query = "LOCK TABLE network_device_lock IN EXCLUSIVE MODE";
    sqlx::query(query)
        .execute(txn.deref_mut())
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// This function should be called whenever a entry is updated/deleted in
/// port_to_network_device_map table. Problem with update is that it can create extra load on
/// db as there are very few chances of port update. So this function is called only from
/// delete port_to_network_device_map call.
pub async fn cleanup_unused_switches(txn: &mut PgTransaction<'_>) -> Result<(), DatabaseError> {
    lock_network_device_table(txn).await?;
    let query = "DELETE FROM network_devices
                 WHERE id NOT IN (SELECT network_device_id FROM port_to_network_device_map)
                 RETURNING id, name, description, ip_addresses, device_type, discovered_via";

    let result = sqlx::query_as::<_, NetworkDevice>(query)
        .fetch_all(txn.deref_mut())
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    if !result.is_empty() {
        let ids = result.iter().map(|x| x.id()).join(",");
        tracing::info!(ids, "Network devices cleaned up as no attached DPU found.")
    }

    Ok(())
}

pub async fn get_topology<DB>(
    txn: &mut DB,
    filter: ObjectFilter<'_, &str>,
) -> Result<NetworkTopologyData, DatabaseError>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    Ok(NetworkTopologyData {
        network_devices: find(txn, filter, &NetworkDeviceSearchConfig::new(true)).await?,
    })
}

pub mod dpu_to_network_device_map {
    use super::*;
    use crate::db_read::DbReader;

    pub async fn create(
        txn: &mut PgConnection,
        local_port: &str,
        remote_port: &str,
        dpu_id: &MachineId,
        network_device_id: &str,
    ) -> Result<DpuToNetworkDeviceMap, DatabaseError> {
        // Update the association if already exists, else just insert into table.
        let query = r#"INSERT INTO port_to_network_device_map(dpu_id, local_port, remote_port, network_device_id)
                         VALUES($1, $2::dpu_local_ports, $3, $4)
                         ON CONFLICT ON CONSTRAINT network_device_dpu_associations_primary
                         DO UPDATE SET
                            network_device_id=EXCLUDED.network_device_id,
                            local_port=EXCLUDED.local_port,
                            remote_port=EXCLUDED.remote_port
                       RETURNING dpu_id, local_port, remote_port, network_device_id"#;

        sqlx::query_as(query)
            .bind(dpu_id)
            .bind(local_port)
            .bind(remote_port)
            .bind(network_device_id)
            .fetch_one(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))
    }

    pub async fn delete(
        // Note: This is a PgTransaction, not a PgConnection, cleaning up switches locks a table,
        // which must happen inside a transaction.
        txn: &mut PgTransaction<'_>,
        dpu_id: &MachineId,
    ) -> Result<(), DatabaseError> {
        // delete the association.
        let query = r#"DELETE from port_to_network_device_map WHERE dpu_id=$1 RETURNING dpu_id"#;

        let _ids = sqlx::query_as::<_, MachineId>(query)
            .bind(dpu_id)
            .fetch_all(txn.deref_mut())
            .await
            .map_err(|e| DatabaseError::query(query, e))?;

        cleanup_unused_switches(txn).await
    }

    pub async fn create_dpu_network_device_association(
        // Note: This is a PgTransaction, not a PgConnection, because we lock the table, which must
        // happen inside a transaction.
        txn: &mut PgTransaction<'_>,
        device_data: &[LldpSwitchData],
        dpu_id: &MachineId,
    ) -> Result<(), DatabaseError> {
        // It is possible that due to older dpu_agent, this data is null for now. In any case,
        // discovery functionality should not be broken.
        // TODO: This check should be removed after sometime.
        if device_data.is_empty() {
            tracing::debug!(machine_id=%dpu_id, "LLDP data is empty for DPU.");
            return Ok(());
        }

        lock_network_device_table(txn).await?;

        // Need to create 3 associations: oob_net0, p0 and p1
        for port in &[DpuLocalPorts::OobNet0, DpuLocalPorts::P0, DpuLocalPorts::P1] {
            // In case any port is missing, print error and continue to avoid discovery failure.
            match get_port_data(device_data, port) {
                Ok(data) => {
                    let tor = get_or_create_network_device(txn, data).await?;
                    create(txn, &data.local_port, &data.remote_port, dpu_id, tor.id()).await?;
                }
                Err(err) => {
                    tracing::warn!(%port, error=format!("{err:#}"), "LLDP data missing");
                }
            }
        }

        Ok(())
    }

    pub async fn find_by_network_device_id(
        txn: impl DbReader<'_>,
        device_id: &str,
    ) -> Result<Vec<DpuToNetworkDeviceMap>, DatabaseError> {
        let base_query = "SELECT l.dpu_id, l.local_port, l.remote_port, l.network_device_id
                          FROM port_to_network_device_map l WHERE network_device_id=$1";

        sqlx::query_as(base_query)
            .bind(device_id)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::new("network_device_id", e))
    }

    pub async fn find_by_dpu_ids(
        txn: impl DbReader<'_>,
        dpu_ids: &[MachineId],
    ) -> Result<Vec<DpuToNetworkDeviceMap>, DatabaseError> {
        let base_query = "SELECT l.dpu_id, l.local_port, l.remote_port, l.network_device_id
                          FROM port_to_network_device_map l WHERE dpu_id=ANY($1)";

        sqlx::query_as(base_query)
            .bind(dpu_ids)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::new("port_to_network_device_map", e))
    }
}
