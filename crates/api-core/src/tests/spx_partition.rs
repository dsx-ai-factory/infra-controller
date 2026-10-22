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

use std::time::Duration;

use carbide_uuid::spx::SpxPartitionId;
use db::ConditionalWrite;
use model::resource_pool::common::DPA_VNI;
use model::resource_pool::{OwnerType, ResourcePoolEntryState};
use rpc::forge::forge_server::Forge;
use rpc::forge::{SpxPartitionCreationRequest, SpxPartitionDeletionRequest};
use tonic::Request;

use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common::api_fixtures::{TestEnv, create_managed_host, create_test_env};
use crate::tests::common::postgres::wait_for_blocked_query;
use crate::tests::instance_allocate::{
    create_spx_allocation_request, create_spx_allocation_request_for_tenant,
};

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

#[crate::sqlx_test]
async fn spx_partition_metadata_survives_creation_and_lookup(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let metadata = rpc::forge::Metadata {
        name: "labeled partition".to_string(),
        description: "partition metadata round trip".to_string(),
        labels: vec![
            rpc::forge::Label {
                key: "environment".to_string(),
                value: Some("test".to_string()),
            },
            rpc::forge::Label {
                key: "reserved".to_string(),
                value: None,
            },
        ],
    };
    let expected = model::metadata::Metadata::try_from(metadata.clone()).unwrap();
    let created = env
        .api
        .create_spx_partition(Request::new(SpxPartitionCreationRequest {
            metadata: Some(metadata),
            tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
            ..Default::default()
        }))
        .await
        .expect("create labeled SPX partition")
        .into_inner();
    assert_eq!(
        model::metadata::Metadata::try_from(created.metadata.unwrap()).unwrap(),
        expected
    );

    let partitions = env
        .api
        .find_spx_partitions_by_ids(Request::new(rpc::forge::SpxPartitionsByIdsRequest {
            spx_partition_ids: vec![created.id.unwrap()],
        }))
        .await
        .expect("read persisted SPX partition")
        .into_inner()
        .spx_partitions;
    assert_eq!(partitions.len(), 1);
    assert_eq!(
        model::metadata::Metadata::try_from(partitions[0].metadata.clone().unwrap()).unwrap(),
        expected
    );
}

#[crate::sqlx_test]
async fn spx_partition_validation_preserves_existing_partitions(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let partition = create_partition(&env, "existing partition").await;
    let id = partition.id.unwrap();
    // Represent a partition persisted before create-time metadata validation.
    let legacy_name = "分区";
    sqlx::query("UPDATE spx_partitions SET name=$1 WHERE id=$2")
        .bind(legacy_name)
        .bind(id)
        .execute(&env.pool)
        .await
        .unwrap();
    let before = deletion_records(&env, id).await;

    let error = env
        .api
        .create_spx_partition(Request::new(SpxPartitionCreationRequest {
            metadata: Some(rpc::forge::Metadata {
                name: legacy_name.to_string(),
                ..Default::default()
            }),
            tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(error.message().contains("ASCII"));
    let partition_count: i64 = sqlx::query_scalar("SELECT count(*) FROM spx_partitions")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(partition_count, 1);
    assert_eq!(deletion_records(&env, id).await, before);

    let partitions = env
        .api
        .find_spx_partitions_by_ids(Request::new(rpc::forge::SpxPartitionsByIdsRequest {
            spx_partition_ids: vec![id],
        }))
        .await
        .unwrap()
        .into_inner()
        .spx_partitions;
    assert_eq!(partitions.len(), 1);
    assert_eq!(partitions[0].metadata.as_ref().unwrap().name, legacy_name);

    env.api
        .delete_spx_partition(Request::new(SpxPartitionDeletionRequest { id: Some(id) }))
        .await
        .expect("delete partition with legacy metadata");
    let after = deletion_records(&env, id).await;
    assert!(after["partition"]["deleted"].is_string());
    assert_eq!(after["allocation"]["state"]["state"], "free");
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
async fn deleting_referenced_spx_partition_preserves_reservation(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let host = create_managed_host(&env).await;
    let request = create_spx_allocation_request(&env, &host, segment_id).await;
    let partition_id = request
        .config
        .as_ref()
        .unwrap()
        .spxconfig
        .as_ref()
        .unwrap()
        .spx_attachments[0]
        .spx_partition_id
        .unwrap();
    let instance_id = env
        .api
        .allocate_instance(Request::new(request))
        .await
        .expect("allocate an instance with an SPX attachment")
        .into_inner()
        .id
        .unwrap();

    sqlx::query("UPDATE instances SET deleted = NOW() WHERE id = $1")
        .bind(instance_id)
        .execute(&env.pool)
        .await
        .expect("mark instance as terminating");

    let unrelated_partition_id = create_partition(&env, "unreferenced partition")
        .await
        .id
        .unwrap();
    let before_unrelated_deletion = deletion_records(&env, partition_id).await;
    env.api
        .delete_spx_partition(Request::new(SpxPartitionDeletionRequest {
            id: Some(unrelated_partition_id),
        }))
        .await
        .expect("an attachment to another partition does not block deletion");
    let unrelated = deletion_records(&env, unrelated_partition_id).await;
    assert!(unrelated["partition"]["deleted"].is_string());
    assert_eq!(unrelated["allocation"]["state"]["state"], "free");
    assert!(unrelated["allocation"]["allocated"].is_null());
    assert_eq!(
        deletion_records(&env, partition_id).await,
        before_unrelated_deletion,
        "deleting an unrelated partition preserves the referenced partition and VNI"
    );

    for (scenario, partition_deleted) in [
        ("terminating instance retains its attachment", false),
        ("tombstone retry must not release a referenced VNI", true),
    ] {
        sqlx::query(
            "UPDATE spx_partitions SET deleted = CASE WHEN $1 THEN NOW() END WHERE id = $2",
        )
        .bind(partition_deleted)
        .bind(partition_id)
        .execute(&env.pool)
        .await
        .expect("set partition deletion state");
        let before = deletion_records(&env, partition_id).await;

        let error = env
            .api
            .delete_spx_partition(Request::new(SpxPartitionDeletionRequest {
                id: Some(partition_id),
            }))
            .await
            .expect_err(scenario);
        assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{scenario}");
        assert!(
            error.message().contains(&format!(
                "SPX partition {partition_id} still has instance attachments"
            )),
            "{scenario}: {error}"
        );
        assert_eq!(
            deletion_records(&env, partition_id).await,
            before,
            "{scenario}"
        );
    }

    let before_retry = deletion_records(&env, partition_id).await;
    let mut txn = env.db_txn().await;
    db::instance::delete(instance_id, txn.as_mut())
        .await
        .expect("remove the terminating instance and its last partition reference");
    txn.commit().await.expect("commit instance removal");
    assert!(
        db::instance::find_by_id(&env.pool, instance_id)
            .await
            .expect("read removed instance")
            .is_none()
    );
    env.api
        .delete_spx_partition(Request::new(SpxPartitionDeletionRequest {
            id: Some(partition_id),
        }))
        .await
        .expect("retry releases the VNI after the last reference is removed");
    let after_retry = deletion_records(&env, partition_id).await;
    assert_eq!(after_retry["partition"], before_retry["partition"]);
    assert_eq!(after_retry["allocation"]["state"]["state"], "free");
    assert!(after_retry["allocation"]["allocated"].is_null());
}

#[crate::sqlx_test]
async fn spx_attachment_update_before_deletion_preserves_partition(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let host = create_managed_host(&env).await;
    let instance = host
        .instance_builer(&env)
        .tenant_org(FIXTURE_TENANT_ORG_ID)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let allocation = create_spx_allocation_request(&env, &host, segment_id).await;
    let requested_spx = allocation.config.unwrap().spxconfig.unwrap();
    let partition_id = requested_spx.spx_attachments[0].spx_partition_id.unwrap();
    let current = instance.rpc_instance().await.into_inner();
    let mut config = current.config.unwrap();
    config.spxconfig = Some(requested_spx);
    let before = deletion_records(&env, partition_id).await;

    // Let validation and Instance row reads proceed, but stop the config write.
    // The updater must retain its partition lock throughout this wait.
    let mut blocker = env.db_txn().await;
    sqlx::query("LOCK TABLE instances IN SHARE MODE")
        .execute(blocker.as_mut())
        .await
        .expect("hold instance writes after attachment validation");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await
        .expect("read instance-write blocker PID");
    let update =
        env.api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(instance.id),
                if_version_match: Some(current.config_version),
                config: Some(config),
                metadata: current.metadata,
            }));
    tokio::pin!(update);
    let update_pid = tokio::select! {
        result = &mut update => panic!("update did not wait to persist its attachment: {result:?}"),
        pid = wait_for_blocked_query(&env.pool, blocker_pid, "spx_config_version = updates.new_version") => pid,
    };

    let deletion = env
        .api
        .delete_spx_partition(Request::new(SpxPartitionDeletionRequest {
            id: Some(partition_id),
        }));
    tokio::pin!(deletion);
    tokio::select! {
        result = &mut update => panic!("update passed the blocked instance write: {result:?}"),
        result = &mut deletion => panic!("deletion passed the admitted attachment: {result:?}"),
        _ = wait_for_blocked_query(&env.pool, update_pid, "FROM spx_partitions") => {}
    }
    blocker.commit().await.expect("allow attachment to commit");
    let (updated, deleted) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(update, deletion)
    })
    .await
    .expect("attachment and deletion finish after releasing the write lock");
    updated.expect("attachment commits before deletion checks references");
    let error = deleted.expect_err("deletion observes the committed attachment");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(
        error.message().contains(&format!(
            "SPX partition {partition_id} still has instance attachments"
        )),
        "{error}"
    );
    assert_eq!(deletion_records(&env, partition_id).await, before);
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await
        .expect("read attached instance")
        .expect("instance remains present");
    assert_eq!(persisted.config.spxconfig.spx_attachments.len(), 1);
    assert_eq!(
        persisted.config.spxconfig.spx_attachments[0].spx_partition_id,
        partition_id
    );
}

#[crate::sqlx_test]
async fn spx_batch_allocation_holds_admission_lock_through_instance_creation(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let first_host = create_managed_host(&env).await;
    let second_host = create_managed_host(&env).await;
    let first_request = create_spx_allocation_request_for_tenant(
        &env,
        &first_host,
        segment_id,
        FIXTURE_TENANT_ORG_ID,
        "02:11:22:33:44:70",
    )
    .await;
    let second_request = create_spx_allocation_request_for_tenant(
        &env,
        &second_host,
        segment_id,
        FIXTURE_TENANT_ORG_ID,
        "02:11:22:33:44:71",
    )
    .await;
    let [first_partition_id, partition_id] = [&first_request, &second_request].map(|request| {
        request
            .config
            .as_ref()
            .unwrap()
            .spxconfig
            .as_ref()
            .unwrap()
            .spx_attachments[0]
            .spx_partition_id
            .unwrap()
    });
    assert_ne!(first_partition_id, partition_id);
    let before = deletion_records(&env, partition_id).await;

    // Keep the batch insert waiting, then delete the later request's partition
    // to prove every admission lock lasts through the owning writes.
    let mut blocker = env.db_txn().await;
    sqlx::query("LOCK TABLE instances IN SHARE MODE")
        .execute(blocker.as_mut())
        .await
        .expect("hold Instance inserts after batch attachment validation");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await
        .expect("read batch-insert blocker PID");
    let allocation =
        env.api
            .allocate_instances(Request::new(rpc::forge::BatchInstanceAllocationRequest {
                instance_requests: vec![first_request, second_request],
            }));
    tokio::pin!(allocation);
    let allocation_pid = tokio::select! {
        result = &mut allocation => panic!("allocation did not wait for its Instance insert: {result:?}"),
        pid = wait_for_blocked_query(&env.pool, blocker_pid, "INSERT INTO instances") => pid,
    };
    let deletion = env
        .api
        .delete_spx_partition(Request::new(SpxPartitionDeletionRequest {
            id: Some(partition_id),
        }));
    tokio::pin!(deletion);
    tokio::select! {
        result = &mut allocation => panic!("allocation passed the blocked Instance insert: {result:?}"),
        result = &mut deletion => panic!("deletion passed the admitted allocation: {result:?}"),
        _ = wait_for_blocked_query(&env.pool, allocation_pid, "FROM spx_partitions") => {}
    }
    blocker.commit().await.expect("allow the batch to commit");
    let (allocated, deleted) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(allocation, deletion)
    })
    .await
    .expect("allocation and deletion finish after releasing the insert");
    let allocated = allocated
        .expect("the complete batch commits before deletion checks references")
        .into_inner();
    assert_eq!(allocated.instances.len(), 2);
    let error = deleted.expect_err("deletion observes the later request's committed attachment");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(
        error.message().contains(&format!(
            "SPX partition {partition_id} still has instance attachments"
        )),
        "{error}"
    );
    assert_eq!(deletion_records(&env, partition_id).await, before);
    for (host, expected_partition_id, expected_mac) in [
        (&first_host, first_partition_id, "02:11:22:33:44:70"),
        (&second_host, partition_id, "02:11:22:33:44:71"),
    ] {
        let instance_id = db::instance::find_id_by_machine_id(&env.pool, &host.host().id.into())
            .await
            .expect("read allocated instance ID")
            .expect("allocated instance persists");
        let persisted = db::instance::find_by_id(&env.pool, instance_id)
            .await
            .expect("read allocated instance")
            .expect("allocated instance remains present");
        assert_eq!(persisted.config.spxconfig.spx_attachments.len(), 1);
        assert_eq!(
            persisted.config.spxconfig.spx_attachments[0].spx_partition_id,
            expected_partition_id
        );
        assert_eq!(
            persisted.config.spxconfig.spx_attachments[0]
                .mac_address
                .as_deref(),
            Some(expected_mac)
        );
    }
}

#[crate::sqlx_test]
async fn spx_partition_deletion_before_batch_allocation_rejects_attachment(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let host = create_managed_host(&env).await;
    let request = create_spx_allocation_request(&env, &host, segment_id).await;
    let partition_id = request
        .config
        .as_ref()
        .unwrap()
        .spxconfig
        .as_ref()
        .unwrap()
        .spx_attachments[0]
        .spx_partition_id
        .unwrap();
    let before = deletion_records(&env, partition_id).await;

    // Stop deletion at VNI release, after it has locked and marked the partition.
    let mut blocker = env.db_txn().await;
    let blocker_pid: i32 = sqlx::query_scalar(
        "SELECT pg_backend_pid() FROM resource_pool WHERE name = $1 AND value = $2 FOR UPDATE",
    )
    .bind(env.common_pools.ethernet.pool_dpa_vni.name())
    .bind(before["partition"]["vni"].as_i64().unwrap().to_string())
    .fetch_one(blocker.as_mut())
    .await
    .expect("hold the partition VNI reservation");
    let deletion = env
        .api
        .delete_spx_partition(Request::new(SpxPartitionDeletionRequest {
            id: Some(partition_id),
        }));
    tokio::pin!(deletion);
    let deletion_pid = tokio::select! {
        result = &mut deletion => panic!("deletion did not wait for its VNI reservation: {result:?}"),
        pid = wait_for_blocked_query(&env.pool, blocker_pid, "UPDATE resource_pool") => pid,
    };

    let allocation =
        env.api
            .allocate_instances(Request::new(rpc::forge::BatchInstanceAllocationRequest {
                instance_requests: vec![request],
            }));
    tokio::pin!(allocation);
    tokio::select! {
        result = &mut deletion => panic!("deletion passed the blocked VNI release: {result:?}"),
        result = &mut allocation => panic!("allocation did not wait for partition deletion: {result:?}"),
        _ = wait_for_blocked_query(&env.pool, deletion_pid, "FROM spx_partitions") => {}
    }
    blocker
        .commit()
        .await
        .expect("allow partition deletion to commit");
    let (deleted, allocated) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(deletion, allocation)
    })
    .await
    .expect("deletion and allocation finish after releasing the VNI lock");
    deleted.expect("delete the unused partition");
    let error = allocated.expect_err("allocation rejects the committed tombstone");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(
        error
            .message()
            .contains(&format!("SPX partition {partition_id} is not created")),
        "{error}"
    );
    assert!(
        db::instance::find_id_by_machine_id(&env.pool, &host.host().id.into())
            .await
            .expect("read allocation result")
            .is_none()
    );
    let after = deletion_records(&env, partition_id).await;
    assert!(after["partition"]["deleted"].is_string());
    assert_eq!(after["allocation"]["state"]["state"], "free");
    assert!(after["allocation"]["allocated"].is_null());
}

#[crate::sqlx_test]
async fn spx_partition_deletion_before_config_update_rejects_attachment(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let host = create_managed_host(&env).await;
    let instance = host
        .instance_builer(&env)
        .tenant_org(FIXTURE_TENANT_ORG_ID)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let allocation = create_spx_allocation_request(&env, &host, segment_id).await;
    let requested_spx = allocation.config.unwrap().spxconfig.unwrap();
    let partition_id = requested_spx.spx_attachments[0].spx_partition_id.unwrap();
    let current = instance.rpc_instance().await.into_inner();
    let mut config = current.config.unwrap();
    config.spxconfig = Some(requested_spx);
    let before = deletion_records(&env, partition_id).await;
    let before_instance = db::instance::find_by_id(&env.pool, instance.id)
        .await
        .expect("read instance before the rejected attachment update")
        .expect("instance exists before the update");
    assert!(before_instance.config.spxconfig.spx_attachments.is_empty());

    // Stop deletion at VNI release, while it owns the partition's exclusive lock.
    let mut blocker = env.db_txn().await;
    let blocker_pid: i32 = sqlx::query_scalar(
        "SELECT pg_backend_pid() FROM resource_pool WHERE name = $1 AND value = $2 FOR UPDATE",
    )
    .bind(env.common_pools.ethernet.pool_dpa_vni.name())
    .bind(before["partition"]["vni"].as_i64().unwrap().to_string())
    .fetch_one(blocker.as_mut())
    .await
    .expect("hold the partition VNI reservation");
    let deletion = env
        .api
        .delete_spx_partition(Request::new(SpxPartitionDeletionRequest {
            id: Some(partition_id),
        }));
    tokio::pin!(deletion);
    let deletion_pid = tokio::select! {
        result = &mut deletion => panic!("deletion did not wait for its VNI reservation: {result:?}"),
        pid = wait_for_blocked_query(&env.pool, blocker_pid, "UPDATE resource_pool") => pid,
    };

    let update =
        env.api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(instance.id),
                if_version_match: Some(current.config_version),
                config: Some(config),
                metadata: current.metadata,
            }));
    tokio::pin!(update);
    tokio::select! {
        result = &mut deletion => panic!("deletion passed the blocked VNI release: {result:?}"),
        result = &mut update => panic!("update did not wait for partition deletion: {result:?}"),
        _ = wait_for_blocked_query(&env.pool, deletion_pid, "FROM spx_partitions") => {}
    }
    blocker
        .commit()
        .await
        .expect("allow partition deletion to commit");
    let (deleted, updated) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(deletion, update)
    })
    .await
    .expect("deletion and update finish after releasing the VNI lock");
    deleted.expect("delete the unused partition before updating the instance");
    let error = updated.expect_err("attachment update rejects the committed tombstone");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(
        error
            .message()
            .contains(&format!("SPX partition {partition_id} is not found")),
        "{error}"
    );

    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await
        .expect("read instance after rejecting the attachment")
        .expect("rejected update preserves the instance");
    assert_eq!(
        serde_json::to_value(&persisted.config).expect("serialize persisted instance config"),
        serde_json::to_value(&before_instance.config).expect("serialize original instance config")
    );
    assert_eq!(persisted.config_version, before_instance.config_version);
    assert_eq!(
        persisted.spx_config_version,
        before_instance.spx_config_version
    );
    let after = deletion_records(&env, partition_id).await;
    assert!(after["partition"]["deleted"].is_string());
    assert_eq!(after["allocation"]["state"]["state"], "free");
    assert!(after["allocation"]["allocated"].is_null());
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
    // `find_by` now excludes soft-deleted rows, so read the tombstone directly to
    // confirm deletion left the row in place with its deletion timestamp set.
    let is_soft_deleted: bool =
        sqlx::query_scalar("SELECT deleted IS NOT NULL FROM spx_partitions WHERE id=$1")
            .bind(id)
            .fetch_one(&mut *txn)
            .await
            .expect("SPX partition must remain as a soft-deleted row");
    assert!(
        is_soft_deleted,
        "SPX partition must remain as a soft-deleted row"
    );
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
