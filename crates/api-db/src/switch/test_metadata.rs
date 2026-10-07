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

use carbide_uuid::switch::SwitchId;
use db::{DatabaseError, switch as db_switch};
use model::metadata::Metadata;
use model::switch::{NewSwitch, SwitchConfig};

use crate as db;

#[crate::sqlx_test]
async fn test_switch_metadata_defaults(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let switch_id = SwitchId::from(uuid::Uuid::new_v4());

    let new_switch = NewSwitch {
        id: switch_id,
        config: SwitchConfig {
            name: "switch-serial-001".to_string(),
            enable_nmxc: false,
            fabric_manager_config: None,
        },
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
        slot_number: None,
        tray_index: None,
    };

    let switch = db_switch::create(&mut txn, &new_switch).await?;

    // Default metadata: name = switch ID, description empty, no labels
    assert_eq!(switch.metadata.name, switch_id.to_string());
    assert_eq!(switch.metadata.description, "");
    assert!(switch.metadata.labels.is_empty());
    assert_eq!(switch.version.version_nr(), 1);

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_switch_metadata_from_expected(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let switch_id = SwitchId::from(uuid::Uuid::new_v4());

    let expected_metadata = Metadata {
        name: "My Switch".to_string(),
        description: "Top-of-rack switch".to_string(),
        labels: [("role".to_string(), "tor".to_string())]
            .into_iter()
            .collect(),
    };

    let new_switch = NewSwitch {
        id: switch_id,
        config: SwitchConfig {
            name: "switch-serial-002".to_string(),
            enable_nmxc: false,
            fabric_manager_config: None,
        },
        bmc_mac_address: None,
        metadata: Some(expected_metadata),
        rack_id: None,
        slot_number: None,
        tray_index: None,
    };

    let switch = db_switch::create(&mut txn, &new_switch).await?;

    assert_eq!(switch.metadata.name, "My Switch");
    assert_eq!(switch.metadata.description, "Top-of-rack switch");
    assert_eq!(switch.metadata.labels.get("role"), Some(&"tor".to_string()));

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_switch_metadata_update(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let switch_id = SwitchId::from(uuid::Uuid::new_v4());

    let new_switch = NewSwitch {
        id: switch_id,
        config: SwitchConfig {
            name: "switch-serial-003".to_string(),
            enable_nmxc: false,
            fabric_manager_config: None,
        },
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
        slot_number: None,
        tray_index: None,
    };

    let switch = db_switch::create(&mut txn, &new_switch).await?;
    let version1 = switch.version;

    let new_metadata = Metadata {
        name: "Updated Switch".to_string(),
        description: "Updated description".to_string(),
        labels: [("team".to_string(), "network".to_string())]
            .into_iter()
            .collect(),
    };

    db_switch::update_metadata(&mut txn, &switch_id, version1, new_metadata.clone()).await?;

    let found = db_switch::find_by(
        &mut txn,
        db::ObjectColumnFilter::One(db_switch::IdColumn, &switch_id),
    )
    .await?;
    let updated_switch = &found[0];

    assert_eq!(updated_switch.metadata.name, "Updated Switch");
    assert_eq!(updated_switch.metadata.description, "Updated description");
    assert_eq!(
        updated_switch.metadata.labels.get("team"),
        Some(&"network".to_string())
    );
    assert_eq!(updated_switch.version.version_nr(), 2);

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_switch_metadata_version_conflict(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let switch_id = SwitchId::from(uuid::Uuid::new_v4());

    let new_switch = NewSwitch {
        id: switch_id,
        config: SwitchConfig {
            name: "switch-serial-004".to_string(),
            enable_nmxc: false,
            fabric_manager_config: None,
        },
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
        slot_number: None,
        tray_index: None,
    };

    let switch = db_switch::create(&mut txn, &new_switch).await?;
    let version1 = switch.version;

    let metadata = Metadata {
        name: "First Update".to_string(),
        ..Metadata::default()
    };
    db_switch::update_metadata(&mut txn, &switch_id, version1, metadata).await?;

    // Using stale version should fail
    let metadata2 = Metadata {
        name: "Second Update".to_string(),
        ..Metadata::default()
    };
    let err = db_switch::update_metadata(&mut txn, &switch_id, version1, metadata2)
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
