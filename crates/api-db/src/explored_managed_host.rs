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

use std::net::IpAddr;

use model::site_explorer::{ExploredDpu, ExploredManagedHost};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, PgConnection, Row};

use crate::DatabaseError;
use crate::db_read::DbReader;

#[derive(Debug, Clone)]
struct DbExploredManagedHost {
    /// The IP address of the node we explored
    host_bmc_ip: IpAddr,
    /// Information about explored DPUs
    dpus: Vec<ExploredDpu>,
}

impl<'r> FromRow<'r, PgRow> for DbExploredManagedHost {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let explored_dpus: sqlx::types::Json<Vec<ExploredDpu>> = row.try_get("explored_dpus")?;
        Ok(DbExploredManagedHost {
            host_bmc_ip: row.try_get("host_bmc_ip")?,
            dpus: explored_dpus.0,
        })
    }
}

impl From<DbExploredManagedHost> for ExploredManagedHost {
    fn from(host: DbExploredManagedHost) -> Self {
        Self {
            host_bmc_ip: host.host_bmc_ip,
            dpus: host.dpus,
        }
    }
}

pub async fn find_ips(
    txn: impl DbReader<'_>,
    // filter is currently is empty, so it is a placeholder for the future
    _filter: model::site_explorer::ExploredManagedHostSearchFilter,
) -> Result<Vec<IpAddr>, DatabaseError> {
    #[derive(Debug, Clone, Copy, FromRow)]
    struct ExploredManagedHostIp(IpAddr);
    // grab list of IPs
    let mut builder = sqlx::QueryBuilder::new("SELECT host_bmc_ip FROM explored_managed_hosts");
    let query = builder.build_query_as();
    let ids: Vec<ExploredManagedHostIp> = query
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("explored_managed_hosts::find_ips", e))?;
    // Convert to Vec<IpAddr> and return.
    Ok(ids.iter().map(|id| id.0).collect())
}

pub async fn find_by_ips(
    txn: impl DbReader<'_>,
    ips: Vec<IpAddr>,
) -> Result<Vec<ExploredManagedHost>, DatabaseError> {
    let query = "SELECT * FROM explored_managed_hosts WHERE host_bmc_ip=ANY($1)";

    sqlx::query_as::<_, DbExploredManagedHost>(query)
        .bind(ips)
        .fetch_all(txn)
        .await
        .map(|hosts| hosts.into_iter().map(Into::into).collect())
        .map_err(|e| DatabaseError::new("explored_managed_hosts::find_by_ips", e))
}

pub async fn find_all(txn: impl DbReader<'_>) -> Result<Vec<ExploredManagedHost>, DatabaseError> {
    let query = "SELECT * FROM explored_managed_hosts ORDER by host_bmc_ip ASC";

    sqlx::query_as::<_, DbExploredManagedHost>(query)
        .fetch_all(txn)
        .await
        .map(|hosts| hosts.into_iter().map(Into::into).collect())
        .map_err(|e| DatabaseError::new("explored_managed_hosts find_all", e))
}

pub async fn update(
    txn: &mut PgConnection,
    explored_hosts: &[&ExploredManagedHost],
) -> Result<(), DatabaseError> {
    let query = r#"DELETE FROM explored_managed_hosts;"#;
    sqlx::query(query)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    // TODO: Optimize me into a single query
    for host in explored_hosts {
        let query = "
            INSERT INTO explored_managed_hosts (host_bmc_ip, explored_dpus)
            VALUES ($1, $2)";
        sqlx::query(query)
            .bind(host.host_bmc_ip)
            .bind(sqlx::types::Json(&host.dpus))
            .execute(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
    }

    Ok(())
}

/// `lock_by_host_bmc_addr` locks every existing row for this address until
/// the caller's transaction completes. The connection must be in a transaction.
/// It returns whether any rows were locked; `false` does not prevent a later
/// insert. Query failures propagate to the caller.
pub async fn lock_by_host_bmc_addr(
    txn: &mut PgConnection,
    addr: IpAddr,
) -> Result<bool, DatabaseError> {
    let query = "SELECT host_bmc_ip FROM explored_managed_hosts WHERE host_bmc_ip = $1 FOR UPDATE";
    let addresses: Vec<IpAddr> = sqlx::query_scalar(query)
        .bind(addr)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(!addresses.is_empty())
}

pub async fn delete_by_host_bmc_addr(
    txn: &mut PgConnection,
    addr: IpAddr,
) -> Result<(), DatabaseError> {
    let query = "DELETE FROM explored_managed_hosts WHERE host_bmc_ip = $1";
    sqlx::query(query)
        .bind(addr)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}
