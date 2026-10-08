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

use carbide_uuid::rack::{RackGroupId, RackId, RackProfileId};
use model::expected_rack::ExpectedRack;
use model::metadata::Metadata;
use sqlx::Connection;

use super::*;

#[crate::sqlx_test]
async fn expected_rack_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_expected_rack_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API's prepared statements while another connection applies DDL.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE expected_racks ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_expected_rack_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_expected_rack_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let expected = ExpectedRack {
        rack_id: RackId::new("projection-rack"),
        rack_profile_id: RackProfileId::new("NVL72"),
        rack_group_id: Some(RackGroupId::new("projection-group")),
        metadata: Metadata {
            name: "expected rack".to_string(),
            description: "populated projection fixture".to_string(),
            labels: [("location".to_string(), "row-1".to_string())]
                .into_iter()
                .collect(),
        },
    };
    let created = create(&mut txn, &expected).await?;
    let found = find_by_rack_id(&mut txn, &expected.rack_id)
        .await?
        .expect("the expected rack exists");
    let all = find_all(&mut txn).await?;
    assert_eq!(all.len(), 1);
    for actual in [&created, &found, &all[0]] {
        assert_eq!(actual.rack_id, expected.rack_id);
        assert_eq!(actual.rack_profile_id, expected.rack_profile_id);
        assert_eq!(actual.rack_group_id, expected.rack_group_id);
        assert_eq!(actual.metadata, expected.metadata);
    }

    txn.rollback().await?;
    Ok(())
}

fn new_rack_id() -> RackId {
    RackId::new(uuid::Uuid::new_v4().to_string())
}

async fn seed_expected_racks(txn: &mut sqlx::PgConnection) -> Vec<RackId> {
    let ids: Vec<RackId> = (0..3).map(|_| new_rack_id()).collect();

    create(
        txn,
        &ExpectedRack {
            rack_group_id: None,
            rack_id: ids[0].clone(),
            rack_profile_id: RackProfileId::new("NVL72"),

            metadata: Metadata {
                name: "rack-1".to_string(),
                description: "Test rack 1".to_string(),
                labels: Default::default(),
            },
        },
    )
    .await
    .unwrap();

    create(
        txn,
        &ExpectedRack {
            rack_group_id: None,
            rack_id: ids[1].clone(),
            rack_profile_id: RackProfileId::new("NVL72"),

            metadata: Metadata {
                name: "rack-2".to_string(),
                description: "Test rack 2".to_string(),
                labels: Default::default(),
            },
        },
    )
    .await
    .unwrap();

    create(
        txn,
        &ExpectedRack {
            rack_group_id: None,
            rack_id: ids[2].clone(),
            rack_profile_id: RackProfileId::new("NVL36"),

            metadata: Metadata {
                name: "rack-3".to_string(),
                description: "Test rack 3".to_string(),
                labels: [("env".to_string(), "test".to_string())]
                    .into_iter()
                    .collect(),
            },
        },
    )
    .await
    .unwrap();

    ids
}

#[crate::sqlx_test]
async fn test_db_find_all(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let _ids = seed_expected_racks(&mut txn).await;

    let all = find_all(&mut txn).await?;
    assert_eq!(all.len(), 3);
    Ok(())
}

#[crate::sqlx_test]
async fn test_db_find_nonexistent(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let rack_id = new_rack_id();
    let result = find_by_rack_id(&mut txn, &rack_id).await?;
    assert!(result.is_none());
    Ok(())
}

#[crate::sqlx_test]
async fn test_db_duplicate_create(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let ids = seed_expected_racks(&mut txn).await;

    let result = create(
        &mut txn,
        &ExpectedRack {
            rack_group_id: None,
            rack_id: ids[0].clone(),
            rack_profile_id: RackProfileId::new("NVL72"),

            metadata: Metadata::default(),
        },
    )
    .await;

    assert!(
        result.is_err(),
        "Creating a duplicate expected rack should fail"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_db_update(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let ids = seed_expected_racks(&mut txn).await;

    let expected_rack = find_by_rack_id(&mut txn, &ids[0])
        .await?
        .expect("Expected rack not found");

    assert_eq!(expected_rack.rack_profile_id.as_str(), "NVL72");

    let updated = ExpectedRack {
        rack_group_id: None,
        rack_id: ids[0].clone(),
        rack_profile_id: RackProfileId::new("NVL36"),
        metadata: Metadata {
            name: "updated-rack".to_string(),
            description: "Updated description".to_string(),
            labels: Default::default(),
        },
    };

    update(&mut txn, &updated).await?;

    txn.commit().await?;

    let mut txn = pool.begin().await?;
    let found = find_by_rack_id(&mut txn, &ids[0]).await?.unwrap();
    assert_eq!(found.rack_profile_id.as_str(), "NVL72");
    assert_eq!(found.metadata.name, "updated-rack");

    Ok(())
}

#[crate::sqlx_test]
async fn test_db_delete(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let ids = seed_expected_racks(&mut txn).await;

    delete(&mut txn, &ids[0]).await?;
    txn.commit().await?;

    let mut txn = pool.begin().await?;
    let result = find_by_rack_id(&mut txn, &ids[0]).await?;
    assert!(result.is_none());

    Ok(())
}

#[crate::sqlx_test]
async fn test_db_delete_nonexistent(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let rack_id = new_rack_id();

    let result = delete(&mut txn, &rack_id).await;
    assert!(result.is_err(), "Deleting nonexistent rack should fail");

    Ok(())
}

#[crate::sqlx_test]
async fn test_db_clear(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let _ids = seed_expected_racks(&mut txn).await;

    clear(&mut txn).await?;
    txn.commit().await?;

    let mut txn = pool.begin().await?;
    let all = find_all(&mut txn).await?;
    assert_eq!(all.len(), 0);

    Ok(())
}
