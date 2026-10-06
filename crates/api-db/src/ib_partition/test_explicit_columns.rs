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

use config_version::Versioned;
use model::ib::{IBMtu, IBRateLimit, IBServiceLevel};
use model::ib_partition::IBPartitionConfig;
use sqlx::Connection;

use super::*;

#[crate::sqlx_test]
async fn ib_partition_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_ib_partition_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API's prepared statements while another connection commits DDL.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE ib_partitions ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_ib_partition_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_ib_partition_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let id = IBPartitionId::new();
    let pkey = 42.try_into()?;
    let config = IBPartitionConfig {
        name: "projection-partition".to_string(),
        pkey: Some(pkey),
        tenant_organization_id: "projection-tenant".parse()?,
        mtu: Some(IBMtu(2)),
        rate_limit: Some(IBRateLimit(100)),
        service_level: Some(IBServiceLevel(3)),
    };
    let metadata = Metadata {
        name: config.name.clone(),
        description: "partition description".to_string(),
        labels: [("owner".to_string(), "tenant".to_string())].into(),
    };
    let status = IBPartitionStatus {
        partition: Some("fabric-partition".to_string()),
        mtu: Some(IBMtu(4)),
        rate_limit: Some(IBRateLimit(200)),
        service_level: Some(IBServiceLevel(5)),
        pkey: Some(pkey),
    };
    let created = create(
        NewIBPartition {
            id,
            config: config.clone(),
            metadata: metadata.clone(),
        },
        txn.as_mut(),
        10,
        status.clone(),
    )
    .await?;
    assert_eq!(created.version.version_nr(), 1);
    let mut expected = IBPartition {
        id,
        version: created.version,
        config,
        status: Some(status),
        deleted: None,
        controller_state: Versioned::new(IBPartitionControllerState::Provisioning, created.version),
        controller_state_outcome: None,
        metadata,
    };
    assert_partition(&created, &expected);

    let controller_version = expected.controller_state.version.increment();
    assert_eq!(
        try_update_controller_state(
            txn.as_mut(),
            id,
            expected.controller_state.version,
            controller_version,
            &IBPartitionControllerState::Ready,
        )
        .await?,
        ConditionalWrite::Applied(()),
    );
    expected.controller_state =
        Versioned::new(IBPartitionControllerState::Ready, controller_version);
    let outcome = PersistentStateHandlerOutcome::Wait {
        reason: "waiting for fabric observation".to_string(),
        source_ref: None,
    };
    update_controller_state_outcome(txn.as_mut(), id, outcome.clone()).await?;
    expected.controller_state_outcome = Some(outcome);

    for found in [
        for_tenant(txn.as_mut(), "projection-tenant".to_string()).await?,
        find_by(txn.as_mut(), ObjectColumnFilter::One(IdColumn, &id)).await?,
    ] {
        assert_eq!(found.len(), 1);
        assert_partition(&found[0], &expected);
    }
    let metadata = Metadata {
        name: "renamed-partition".to_string(),
        description: "updated description".to_string(),
        labels: [("owner".to_string(), "updated-tenant".to_string())].into(),
    };
    let ConditionalWrite::Applied(updated) =
        update_metadata(id, expected.version, &metadata, txn.as_mut()).await?
    else {
        panic!("the current partition version must accept the metadata update");
    };
    assert_eq!(
        updated.version.version_nr(),
        expected.version.version_nr() + 1
    );
    expected.version = updated.version;
    expected.config.name = metadata.name.clone();
    expected.metadata = metadata;
    assert_partition(&updated, &expected);

    expected.status = Some(IBPartitionStatus {
        partition: Some("observed-partition".to_string()),
        mtu: None,
        rate_limit: None,
        service_level: None,
        pkey: Some(pkey),
    });
    let updated = update_status(id, &expected.status, txn.as_mut()).await?;
    assert_partition(&updated, &expected);
    let found = find_by(txn.as_mut(), ObjectColumnFilter::One(IdColumn, &id)).await?;
    assert_eq!(found.len(), 1);
    assert_partition(&found[0], &expected);

    // Older rows can have no legacy PKey, status, or controller result.
    sqlx::query(
        "UPDATE ib_partitions SET pkey = NULL, status = NULL, controller_state_outcome = NULL
         WHERE id = $1",
    )
    .bind(id)
    .execute(txn.as_mut())
    .await?;
    expected.config.pkey = None;
    expected.status = None;
    expected.controller_state_outcome = None;
    let deleted = mark_as_deleted(&expected, txn.as_mut()).await?;
    expected.deleted = Some(
        sqlx::query_scalar("SELECT NOW()")
            .fetch_one(txn.as_mut())
            .await?,
    );
    assert_partition(&deleted, &expected);
    let found = for_tenant(txn.as_mut(), "projection-tenant".to_string()).await?;
    assert_eq!(found.len(), 1);
    assert_partition(&found[0], &expected);

    txn.rollback().await?;
    Ok(())
}

fn assert_partition(actual: &IBPartition, expected: &IBPartition) {
    assert_eq!(actual.id, expected.id);
    assert_eq!(actual.version, expected.version);
    assert_eq!(actual.config, expected.config);
    assert_eq!(actual.status, expected.status);
    assert_eq!(actual.deleted, expected.deleted);
    assert_eq!(
        actual.controller_state.value,
        expected.controller_state.value
    );
    assert_eq!(
        actual.controller_state.version,
        expected.controller_state.version
    );
    assert_eq!(
        actual.controller_state_outcome,
        expected.controller_state_outcome
    );
    assert_eq!(actual.metadata, expected.metadata);
}
