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

use carbide_uuid::power_shelf::{HardwareHash, PowerShelfId, PowerShelfIdSource, PowerShelfType};
use model::metadata::Metadata;
use model::power_shelf::{NewPowerShelf, PowerShelf, PowerShelfConfig};
use sqlx::PgConnection;

use crate::{DatabaseError, power_shelf as db_power_shelf};

/// Returns the deterministic power shelf ID derived from `seed`.
pub(crate) fn seeded_id(seed: u8) -> PowerShelfId {
    let hash: HardwareHash = [seed; 32];
    PowerShelfId::new(
        PowerShelfIdSource::ProductBoardChassisSerial,
        hash,
        PowerShelfType::Rack,
    )
}

/// Creates a power shelf with a deterministic ID derived from `seed`.
///
/// Reusing the same seed in the same database or transaction will collide with
/// the existing power shelf ID.
pub async fn create_seeded(
    txn: &mut PgConnection,
    seed: u8,
    name: &str,
) -> Result<PowerShelf, DatabaseError> {
    let new_power_shelf = NewPowerShelf {
        id: seeded_id(seed),
        config: PowerShelfConfig {
            name: name.to_string(),
            capacity: Some(5000),
            voltage: Some(240),
        },
        bmc_mac_address: None,
        metadata: Some(Metadata {
            name: name.to_string(),
            description: String::new(),
            labels: Default::default(),
        }),
        rack_id: None,
    };

    db_power_shelf::create(txn, &new_power_shelf).await
}

/// Creates a power shelf with deterministic ID and caller-provided config.
///
/// Reusing the same seed in the same database or transaction will collide with
/// the existing power shelf ID.
pub async fn create_seeded_with_config(
    txn: &mut PgConnection,
    seed: u8,
    name: &str,
    capacity: Option<u32>,
    voltage: Option<u32>,
) -> Result<PowerShelf, DatabaseError> {
    let new_power_shelf = NewPowerShelf {
        id: seeded_id(seed),
        config: PowerShelfConfig {
            name: name.to_string(),
            capacity,
            voltage,
        },
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
    };

    db_power_shelf::create(txn, &new_power_shelf).await
}

pub async fn create_random_with_config(
    txn: &mut PgConnection,
    name: &str,
    capacity: Option<u32>,
    voltage: Option<u32>,
) -> Result<PowerShelf, DatabaseError> {
    let new_power_shelf = NewPowerShelf {
        id: PowerShelfId::from(uuid::Uuid::new_v4()),
        config: PowerShelfConfig {
            name: name.to_string(),
            capacity,
            voltage,
        },
        bmc_mac_address: None,
        metadata: None,
        rack_id: None,
    };

    db_power_shelf::create(txn, &new_power_shelf).await
}
