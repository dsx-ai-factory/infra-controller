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

use carbide_uuid::spx::SpxPartitionId;
use db::{ConditionalWrite, ObjectColumnFilter};
use model::resource_pool::common::DPA_VNI;
use model::resource_pool::{OwnerType, ResourcePoolEntryState};
use rpc::forge::forge_server::Forge;
use rpc::forge::{SpxPartitionCreationRequest, SpxPartitionDeletionRequest};
use tonic::Request;

use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common::api_fixtures::{TestEnv, create_test_env};

async fn create_partition(env: &TestEnv, name: &str) -> rpc::forge::SpxPartition {
    env.api
        .create_spx_partition(Request::new(SpxPartitionCreationRequest {
            metadata: Some(rpc::forge::Metadata {
                name: name.to_string(),
                ..Default::default()
            }),
            tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
            ..Default::default()
        }))
        .await
        .expect("create SPX partition")
        .into_inner()
}

async fn deletion_records(env: &TestEnv, id: SpxPartitionId) -> serde_json::Value {
    sqlx::query_scalar(
        "SELECT jsonb_build_object('partition', to_jsonb(p), 'allocation', to_jsonb(r))
         FROM spx_partitions p JOIN resource_pool r ON r.value=p.vni::text AND r.name=$2
         WHERE p.id=$1",
    )
    .bind(id)
    .bind(env.common_pools.ethernet.pool_dpa_vni.name())
    .fetch_one(&env.pool)
    .await
    .expect("read partition and allocation")
}

#[crate::sqlx_test]
async fn deleting_spx_partition_releases_vni_only_once(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    // One allocatable value makes the next partition reuse this VNI.
    sqlx::query(
        "DELETE FROM resource_pool WHERE name=$1
         AND value <> (SELECT min(value) FROM resource_pool WHERE name=$1)",
    )
    .bind(env.common_pools.ethernet.pool_dpa_vni.name())
    .execute(&env.pool)
    .await
    .expect("keep one DPA VNI");
    let partition = create_partition(&env, "first partition").await;
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
    txn.commit().await.expect("finish deletion read");

    let replacement = create_partition(&env, "replacement partition").await;
    assert_eq!(replacement.vni, partition.vni);
    let before = deletion_records(&env, id).await;
    assert_eq!(
        before["allocation"]["state"]["owner"],
        replacement
            .id
            .expect("replacement partition ID")
            .to_string()
    );

    env.api
        .delete_spx_partition(Request::new(SpxPartitionDeletionRequest { id: Some(id) }))
        .await
        .expect("repeated deletion succeeds");
    assert_eq!(deletion_records(&env, id).await, before);
}

#[crate::sqlx_test]
async fn failed_vni_release_preserves_active_partition(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let partition = create_partition(&env, "release query failure").await;
    let id = partition.id.expect("SPX partition ID");
    let vni = i32::try_from(partition.vni).expect("VNI fits pool type");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE resource_pool ADD CONSTRAINT reject_spx_vni_release
         CHECK (name <> '{DPA_VNI}' OR value <> '{vni}' OR allocated IS NOT NULL)"
    )))
    .execute(&env.pool)
    .await
    .expect("reject this allocation's release");
    let before = deletion_records(&env, id).await;
    assert!(before["partition"]["deleted"].is_null());

    let error = env
        .api
        .delete_spx_partition(Request::new(SpxPartitionDeletionRequest { id: Some(id) }))
        .await
        .expect_err("release failure prevents deletion");
    assert_eq!(error.code(), tonic::Code::Internal);
    assert!(
        error.message().contains("reject_spx_vni_release"),
        "{error}"
    );
    assert_eq!(deletion_records(&env, id).await, before);

    sqlx::query("ALTER TABLE resource_pool DROP CONSTRAINT reject_spx_vni_release")
        .execute(&env.pool)
        .await
        .expect("remove injected release failure");
    env.api
        .delete_spx_partition(Request::new(SpxPartitionDeletionRequest { id: Some(id) }))
        .await
        .expect("retry completes deletion");
    let after_retry = deletion_records(&env, id).await;
    assert!(after_retry["partition"]["deleted"].is_string());
    assert_eq!(after_retry["allocation"]["state"]["state"], "free");
    assert!(after_retry["allocation"]["allocated"].is_null());
}

#[crate::sqlx_test]
async fn repeated_deletion_releases_a_still_owned_vni(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let partition = create_partition(&env, "deleted partition with unreleased VNI").await;
    let id = partition.id.expect("SPX partition ID");

    // Persist a partial deletion with its VNI reservation still allocated.
    let mut txn = env.db_txn().await;
    assert!(matches!(
        db::spx_partition::mark_as_deleted(id, &mut txn)
            .await
            .expect("record deletion before release"),
        ConditionalWrite::Applied(_)
    ));
    txn.commit().await.expect("commit deletion without release");
    let before = deletion_records(&env, id).await;

    env.api
        .delete_spx_partition(Request::new(SpxPartitionDeletionRequest { id: Some(id) }))
        .await
        .expect("complete deletion");
    let after = deletion_records(&env, id).await;
    assert!(after["partition"]["deleted"].is_string());
    assert_eq!(after["allocation"]["state"]["state"], "free");
    assert!(after["allocation"]["allocated"].is_null());
    assert_eq!(after["partition"], before["partition"]);
}
