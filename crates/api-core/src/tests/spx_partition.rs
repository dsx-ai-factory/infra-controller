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

use db::ObjectColumnFilter;
use model::resource_pool::{OwnerType, ResourcePoolEntryState};
use rpc::forge::forge_server::Forge;
use rpc::forge::{SpxPartitionCreationRequest, SpxPartitionDeletionRequest};
use tonic::Request;

use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common::api_fixtures::create_test_env;

#[crate::sqlx_test]
async fn deleting_spx_partition_frees_its_dpa_vni(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let partition = env
        .api
        .create_spx_partition(Request::new(SpxPartitionCreationRequest {
            metadata: Some(rpc::forge::Metadata {
                name: "release DPA VNI".to_string(),
                ..Default::default()
            }),
            tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
            ..Default::default()
        }))
        .await
        .expect("create SPX partition")
        .into_inner();
    let id = partition.id.expect("SPX partition ID");
    let vni = partition.vni.to_string();
    let mut txn = env.db_txn().await;
    let allocated_entry = db::resource_pool::find_value(&mut *txn, &vni)
        .await
        .expect("find SPX allocation")
        .into_iter()
        .find(|entry| entry.pool_name == env.common_pools.ethernet.pool_dpa_vni.name())
        .expect("SPX VNI pool entry");
    assert_eq!(
        allocated_entry.state.0,
        ResourcePoolEntryState::Allocated {
            owner: id.to_string(),
            owner_type: OwnerType::SpxPartition.to_string(),
        }
    );
    txn.commit().await.expect("finish allocation read");

    env.api
        .delete_spx_partition(Request::new(SpxPartitionDeletionRequest { id: Some(id) }))
        .await
        .expect("delete SPX partition");

    let mut txn = env.db_txn().await;
    let partitions = db::spx_partition::find_by(
        &mut *txn,
        ObjectColumnFilter::One(db::spx_partition::IdColumn, &id),
    )
    .await
    .expect("find deleted SPX partition");
    let [deleted_partition] = partitions.as_slice() else {
        panic!("SPX partition must remain as a soft-deleted row");
    };
    assert!(deleted_partition.deleted.is_some());
    let released_entry = db::resource_pool::find_value(&mut *txn, &vni)
        .await
        .expect("find released SPX allocation")
        .into_iter()
        .find(|entry| entry.pool_name == env.common_pools.ethernet.pool_dpa_vni.name())
        .expect("released SPX VNI must remain in its pool");
    assert_eq!(released_entry.state.0, ResourcePoolEntryState::Free);
}
