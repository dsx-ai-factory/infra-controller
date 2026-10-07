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

use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use db::{DatabaseError, power_shelf as db_power_shelf};
use model::metadata::Metadata;
use model::power_shelf::{NewPowerShelf, PowerShelfConfig};

use crate as db;

#[crate::sqlx_test]
async fn test_power_shelf_metadata_defaults(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let ps_id = PowerShelfId::from(uuid::Uuid::new_v4());

    let new_ps = NewPowerShelf {
        id: ps_id,
        bmc_mac_address: None,
        rack_id: Some(RackId::new("test-rack-1".to_string())),
        config: PowerShelfConfig {
            name: "shelf-serial-001".to_string(),
            capacity: Some(100),
            voltage: Some(240),
        },
        metadata: None,
    };

    let ps = db_power_shelf::create(&mut txn, &new_ps).await?;

    // Default metadata: name = power shelf ID, description empty, no labels
    assert_eq!(ps.metadata.name, ps_id.to_string());
    assert_eq!(ps.metadata.description, "");
    assert!(ps.metadata.labels.is_empty());
    assert_eq!(ps.version.version_nr(), 1);

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_power_shelf_metadata_from_expected(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let ps_id = PowerShelfId::from(uuid::Uuid::new_v4());

    let expected_metadata = Metadata {
        name: "My Power Shelf".to_string(),
        description: "48V power shelf".to_string(),
        labels: [("rack".to_string(), "rack-01".to_string())]
            .into_iter()
            .collect(),
    };

    let new_ps = NewPowerShelf {
        id: ps_id,
        bmc_mac_address: None,
        rack_id: Some(RackId::new("test-rack-1".to_string())),
        config: PowerShelfConfig {
            name: "shelf-serial-002".to_string(),
            capacity: Some(100),
            voltage: Some(240),
        },
        metadata: Some(expected_metadata),
    };

    let ps = db_power_shelf::create(&mut txn, &new_ps).await?;

    assert_eq!(ps.metadata.name, "My Power Shelf");
    assert_eq!(ps.metadata.description, "48V power shelf");
    assert_eq!(ps.metadata.labels.get("rack"), Some(&"rack-01".to_string()));

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_power_shelf_metadata_update(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let ps_id = PowerShelfId::from(uuid::Uuid::new_v4());

    let new_ps = NewPowerShelf {
        id: ps_id,
        bmc_mac_address: None,
        rack_id: Some(RackId::new("test-rack-1".to_string())),
        config: PowerShelfConfig {
            name: "shelf-serial-003".to_string(),
            capacity: Some(100),
            voltage: Some(240),
        },
        metadata: None,
    };

    let ps = db_power_shelf::create(&mut txn, &new_ps).await?;
    let version1 = ps.version;

    let new_metadata = Metadata {
        name: "Updated Shelf".to_string(),
        description: "Updated description".to_string(),
        labels: [("team".to_string(), "power".to_string())]
            .into_iter()
            .collect(),
    };

    db_power_shelf::update_metadata(&mut txn, &ps_id, version1, new_metadata.clone()).await?;

    let found = db_power_shelf::find_by(
        &mut txn,
        db::ObjectColumnFilter::One(db_power_shelf::IdColumn, &ps_id),
    )
    .await?;
    let updated_ps = &found[0];

    assert_eq!(updated_ps.metadata.name, "Updated Shelf");
    assert_eq!(updated_ps.metadata.description, "Updated description");
    assert_eq!(
        updated_ps.metadata.labels.get("team"),
        Some(&"power".to_string())
    );
    assert_eq!(updated_ps.version.version_nr(), 2);

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_power_shelf_metadata_version_conflict(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let ps_id = PowerShelfId::from(uuid::Uuid::new_v4());

    let new_ps = NewPowerShelf {
        id: ps_id,
        bmc_mac_address: None,
        rack_id: Some(RackId::new("test-rack-1".to_string())),
        config: PowerShelfConfig {
            name: "shelf-serial-004".to_string(),
            capacity: Some(100),
            voltage: Some(240),
        },
        metadata: None,
    };

    let ps = db_power_shelf::create(&mut txn, &new_ps).await?;
    let version1 = ps.version;

    let metadata = Metadata {
        name: "First Update".to_string(),
        ..Metadata::default()
    };
    db_power_shelf::update_metadata(&mut txn, &ps_id, version1, metadata).await?;

    // Using stale version should fail
    let metadata2 = Metadata {
        name: "Second Update".to_string(),
        ..Metadata::default()
    };
    let err = db_power_shelf::update_metadata(&mut txn, &ps_id, version1, metadata2)
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
