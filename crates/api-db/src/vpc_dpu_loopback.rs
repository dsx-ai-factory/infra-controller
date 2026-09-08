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

use carbide_uuid::machine::DpuMachineId;
use carbide_uuid::vpc::VpcId;
use model::vpc::VpcDpuLoopback;
use sqlx::PgConnection;

use crate::DatabaseError;

pub async fn persist(
    value: VpcDpuLoopback,
    txn: &mut PgConnection,
) -> Result<VpcDpuLoopback, DatabaseError> {
    let query = "INSERT INTO vpc_dpu_loopbacks (dpu_id, vpc_id, loopback_ip)
                           VALUES ($1, $2, $3) RETURNING *";
    sqlx::query_as(query)
        .bind(value.dpu_id)
        .bind(value.vpc_id)
        .bind(value.loopback_ip)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn delete_and_deallocate(
    common_pools: &model::resource_pool::common::CommonPools,
    dpu_id: &DpuMachineId,
    txn: &mut PgConnection,
    delete_admin_loopback_also: bool,
) -> Result<(), DatabaseError> {
    let mut query = sqlx::QueryBuilder::new("DELETE FROM vpc_dpu_loopbacks WHERE TRUE ");

    query.push(" AND dpu_id= ");
    query.push_bind(dpu_id);

    if !delete_admin_loopback_also {
        let admin_segments = crate::network_segment::admin(txn).await?;
        let admin_vpcs = admin_segments
            .iter()
            .filter_map(|s| s.config.vpc_id)
            .collect::<Vec<VpcId>>();

        if admin_vpcs.is_empty() {
            tracing::warn!(
                ?admin_segments,
                "No VPC attached to one or more admin segments.",
            );
        } else {
            // We only allow a single admin VPC, so it seems this could easily be
            // vpc_id != admin_vpc_id, but the ALL seems cheap enough based on that
            // and would ensure we don't have to worry later if we introduce multiple
            // admin VPCs.
            query.push(" AND vpc_id != ALL( ");
            query.push_bind(admin_vpcs);
            query.push(" )");
        }
    };

    query.push("  RETURNING * ");

    let deleted_loopbacks: Vec<VpcDpuLoopback> = query
        .build_query_as()
        .fetch_all(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))?;

    for value in deleted_loopbacks {
        // We deleted a IP from vpc_dpu_loopback table. Deallocate this IP from common pool.
        crate::resource_pool::release(
            &common_pools.ethernet.pool_vpc_dpu_loopback_ip,
            txn,
            value.loopback_ip,
        )
        .await?;
    }

    Ok(())
}

/// Deletes and deallocates loopbacks for a DPU scoped to specific VPCs.
pub async fn delete_and_deallocate_for_vpcs(
    common_pools: &model::resource_pool::common::CommonPools,
    dpu_id: &DpuMachineId,
    vpc_ids: &[VpcId],
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    if vpc_ids.is_empty() {
        return Ok(());
    }

    // Delete first so pool release only follows rows that existed.
    let mut query = sqlx::QueryBuilder::new("DELETE FROM vpc_dpu_loopbacks WHERE dpu_id = ");
    query.push_bind(dpu_id);
    query.push(" AND vpc_id = ANY(");
    query.push_bind(vpc_ids);
    query.push(") RETURNING *");

    let deleted_loopbacks: Vec<VpcDpuLoopback> = query
        .build_query_as()
        .fetch_all(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))?;

    for value in deleted_loopbacks {
        // Return each deleted loopback IP to the shared VPC loopback pool.
        crate::resource_pool::release(
            &common_pools.ethernet.pool_vpc_dpu_loopback_ip,
            txn,
            value.loopback_ip,
        )
        .await?;
    }

    Ok(())
}

pub async fn find(
    txn: &mut PgConnection,
    dpu_id: &DpuMachineId,
    vpc_id: &VpcId,
) -> Result<Option<VpcDpuLoopback>, DatabaseError> {
    let query = "SELECT * from vpc_dpu_loopbacks WHERE dpu_id=$1 AND vpc_id=$2";

    sqlx::query_as(query)
        .bind(dpu_id)
        .bind(vpc_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Allocate loopback ip for a vpc and dpu if not allocated yet.
/// If already allocated, return the value.
pub async fn get_or_allocate_loopback_ip_for_vpc(
    common_pools: &model::resource_pool::common::CommonPools,
    txn: &mut PgConnection,
    dpu_id: &DpuMachineId,
    vpc_id: &VpcId,
) -> Result<IpAddr, DatabaseError> {
    let loopback_ip = match find(txn, dpu_id, vpc_id).await? {
        Some(x) => x.loopback_ip,
        None => {
            let loopback_ip =
                crate::machine::allocate_vpc_dpu_loopback(common_pools, txn, &dpu_id.to_string())
                    .await?;
            let vpc_dpu_loopback = VpcDpuLoopback::new(*dpu_id, *vpc_id, loopback_ip);
            persist(vpc_dpu_loopback, txn).await?;

            loopback_ip
        }
    };

    Ok(loopback_ip)
}
