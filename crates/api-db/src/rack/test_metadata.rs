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

use carbide_uuid::rack::{RackId, RackProfileId};
use db::{DatabaseError, ObjectColumnFilter, rack as db_rack};
use model::metadata::Metadata;
use model::rack::RackConfig;

use crate as db;

#[crate::sqlx_test]
async fn test_rack_metadata_defaults(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let rack_id = RackId::new("test-rack-1".to_string());

    let rack = db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("NVL72")),
        &RackConfig::default(),
        None,
    )
    .await?;

    // Default metadata: name = rack_id, description empty, no labels
    assert_eq!(rack.metadata.name, rack_id.to_string());
    assert_eq!(rack.metadata.description, "");
    assert!(rack.metadata.labels.is_empty());
    assert_eq!(rack.version.version_nr(), 1);
    assert_eq!(rack.controller_state.version, rack.version);

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_rack_metadata_from_expected(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let rack_id = RackId::new("test-rack-2".to_string());

    let expected_metadata = Metadata {
        name: "My Rack".to_string(),
        description: "A test rack".to_string(),
        labels: [("env".to_string(), "staging".to_string())]
            .into_iter()
            .collect(),
    };

    let rack = db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("NVL72")),
        &RackConfig::default(),
        Some(&expected_metadata),
    )
    .await?;

    assert_eq!(rack.metadata.name, "My Rack");
    assert_eq!(rack.metadata.description, "A test rack");
    assert_eq!(
        rack.metadata.labels.get("env"),
        Some(&"staging".to_string())
    );

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_rack_metadata_update(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let rack_id = RackId::new("test-rack-3".to_string());

    let rack = db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("NVL72")),
        &RackConfig::default(),
        None,
    )
    .await?;
    let version1 = rack.version;

    let new_metadata = Metadata {
        name: "Updated Rack".to_string(),
        description: "Updated description".to_string(),
        labels: [("team".to_string(), "infra".to_string())]
            .into_iter()
            .collect(),
    };

    db_rack::update_metadata(&mut txn, &rack_id, version1, new_metadata.clone()).await?;

    let updated_rack = db_rack::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(db_rack::IdColumn, &rack_id),
    )
    .await
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(updated_rack.metadata.name, "Updated Rack");
    assert_eq!(updated_rack.metadata.description, "Updated description");
    assert_eq!(
        updated_rack.metadata.labels.get("team"),
        Some(&"infra".to_string())
    );
    assert_eq!(updated_rack.version.version_nr(), 2);

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_rack_metadata_version_conflict(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let rack_id = RackId::new("test-rack-4".to_string());

    let rack = db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("NVL72")),
        &RackConfig::default(),
        None,
    )
    .await?;
    let version1 = rack.version;

    let metadata = Metadata {
        name: "First Update".to_string(),
        ..Metadata::default()
    };
    db_rack::update_metadata(&mut txn, &rack_id, version1, metadata).await?;

    // Using stale version should fail with ConcurrentModificationError
    let metadata2 = Metadata {
        name: "Second Update".to_string(),
        ..Metadata::default()
    };
    let err = db_rack::update_metadata(&mut txn, &rack_id, version1, metadata2)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DatabaseError::ConcurrentModificationError(..)),
        "Expected ConcurrentModificationError, got: {:?}",
        err
    );

    txn.rollback().await?;
    Ok(())
}
