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

use std::sync::Arc;

use config_version::ConfigVersion;
use model::resource_pool::common::{self, CommonPools, EthernetPools, IbPools, VPC_DPU_LOOPBACK};
use model::resource_pool::{
    Range, ResourcePool, ResourcePoolDef, ResourcePoolEntryState, ResourcePoolType, ValueType,
};
use sqlx::{Connection, PgPool};
use tokio::sync::oneshot;

use super::*;

#[crate::sqlx_test]
async fn loopback_queries_survive_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_loopback_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep prepared statements on the API connection while a migration commits.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE vpc_dpu_loopbacks ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_loopback_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_loopback_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let (stop_sender, _stop_receiver) = oneshot::channel();
    let common_pools = CommonPools {
        ethernet: EthernetPools {
            pool_loopback_ip: Arc::new(ResourcePool::new(
                common::LOOPBACK_IP.to_string(),
                ValueType::Ipv4,
            )),
            pool_loopback_ip_v6: Arc::new(ResourcePool::new(
                common::LOOPBACK_IP_V6.to_string(),
                ValueType::Ipv6,
            )),
            pool_vlan_id: Arc::new(ResourcePool::new(
                common::VLANID.to_string(),
                ValueType::Integer,
            )),
            pool_vni: Arc::new(ResourcePool::new(
                common::VNI.to_string(),
                ValueType::Integer,
            )),
            pool_vpc_vni: Arc::new(ResourcePool::new(
                common::VPC_VNI.to_string(),
                ValueType::Integer,
            )),
            pool_external_vpc_vni: Arc::new(ResourcePool::new(
                common::EXTERNAL_VPC_VNI.to_string(),
                ValueType::Integer,
            )),
            pool_dpa_vni: Arc::new(ResourcePool::new(
                common::DPA_VNI.to_string(),
                ValueType::Integer,
            )),
            pool_fnn_asn: Arc::new(ResourcePool::new(
                common::FNN_ASN.to_string(),
                ValueType::Integer,
            )),
            pool_vpc_dpu_loopback_ip: Arc::new(ResourcePool::new(
                VPC_DPU_LOOPBACK.to_string(),
                ValueType::Ipv4,
            )),
        },
        infiniband: IbPools::default(),
        pool_stats: Default::default(),
        _stop_sender: stop_sender,
    };
    crate::resource_pool::define(
        &mut txn,
        VPC_DPU_LOOPBACK,
        &ResourcePoolDef {
            ranges: vec![Range {
                start: "192.0.2.10".to_string(),
                end: "192.0.2.13".to_string(),
                auto_assign: true,
            }],
            prefix: None,
            pool_type: ResourcePoolType::Ipv4,
            delegate_prefix_len: None,
        },
    )
    .await?;

    let dpu_id: DpuMachineId =
        "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng".parse()?;
    sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}')")
        .bind(dpu_id)
        .execute(&mut *txn)
        .await?;
    let admin_vpc_id: VpcId = "28dc4893-c081-4efc-9a63-a82e62ccbe12".parse()?;
    let selected_vpc_id: VpcId = "a2dbd9a4-a0bf-40a3-9329-239d5416c313".parse()?;
    let other_vpc_id: VpcId = "f6c79f8f-d17d-45d7-bf15-3690d4796f14".parse()?;
    let mut loopbacks = Vec::new();
    for (vpc_id, name) in [
        (admin_vpc_id, "admin projection vpc"),
        (selected_vpc_id, "selected projection vpc"),
        (other_vpc_id, "other projection vpc"),
    ] {
        sqlx::query("INSERT INTO vpcs (id, name, version) VALUES ($1, $2, $3)")
            .bind(vpc_id)
            .bind(name)
            .bind(ConfigVersion::initial())
            .execute(&mut *txn)
            .await?;
        let loopback_ip =
            get_or_allocate_loopback_ip_for_vpc(&common_pools, &mut txn, &dpu_id, &vpc_id).await?;
        loopbacks.push(VpcDpuLoopback::new(dpu_id, vpc_id, loopback_ip));
    }
    sqlx::query(
        "INSERT INTO network_segments (name, vpc_id, version, network_segment_type)
         VALUES ('admin projection segment', $1, $2, 'admin')",
    )
    .bind(admin_vpc_id)
    .bind(ConfigVersion::initial())
    .execute(&mut *txn)
    .await?;
    assert_loopback_allocations(
        &mut txn,
        &loopbacks,
        &[admin_vpc_id, selected_vpc_id, other_vpc_id],
    )
    .await?;

    delete_and_deallocate_for_vpcs(&common_pools, &dpu_id, &[selected_vpc_id], &mut txn).await?;
    assert_loopback_allocations(&mut txn, &loopbacks, &[admin_vpc_id, other_vpc_id]).await?;

    delete_and_deallocate(&common_pools, &dpu_id, &mut txn, false).await?;
    assert_loopback_allocations(&mut txn, &loopbacks, &[admin_vpc_id]).await?;

    delete_and_deallocate(&common_pools, &dpu_id, &mut txn, true).await?;
    assert_loopback_allocations(&mut txn, &loopbacks, &[]).await?;

    // Roll back the fixture so both passes allocate from the same pool.
    // The connection retains its prepared statements after the rollback.
    txn.rollback().await?;
    Ok(())
}

async fn assert_loopback_allocations(
    connection: &mut PgConnection,
    loopbacks: &[VpcDpuLoopback],
    retained_vpc_ids: &[VpcId],
) -> Result<(), Box<dyn std::error::Error>> {
    for expected in loopbacks {
        let retained = retained_vpc_ids.contains(&expected.vpc_id);
        let found = find(connection, &expected.dpu_id, &expected.vpc_id).await?;
        assert_eq!(found.is_some(), retained, "{}", expected.vpc_id);
        if let Some(found) = found {
            assert_eq!(found.dpu_id, expected.dpu_id);
            assert_eq!(found.vpc_id, expected.vpc_id);
            assert_eq!(found.loopback_ip, expected.loopback_ip);
        }

        let entries =
            crate::resource_pool::find_value(&mut *connection, &expected.loopback_ip.to_string())
                .await?;
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.pool_name, VPC_DPU_LOOPBACK);
        let expected_state = if retained {
            ResourcePoolEntryState::Allocated {
                owner: expected.dpu_id.to_string(),
                owner_type: OwnerType::Machine.to_string(),
            }
        } else {
            ResourcePoolEntryState::Free
        };
        assert_eq!(entry.state.0, expected_state, "{}", expected.loopback_ip);
        assert_eq!(entry.allocated.is_some(), retained);
    }
    let stats = crate::resource_pool::stats(connection, VPC_DPU_LOOPBACK).await?;
    assert_eq!(stats.used, retained_vpc_ids.len());
    assert_eq!(stats.free, loopbacks.len() - retained_vpc_ids.len());
    Ok(())
}
