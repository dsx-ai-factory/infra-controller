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

use std::str::FromStr;

use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use mac_address::MacAddress;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::network_segment::NetworkSegmentControllerState;

use crate as db;
use crate::DatabaseError;
use crate::test_support::network_segment::admin_segment;

fn test_machine_id() -> MachineId {
    MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x42; 32],
        MachineType::Dpu,
    )
}

#[crate::sqlx_test]
async fn prevent_duplicate_mac_addresses(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let network_segment = db::network_segment::persist(
        admin_segment("ADMIN_TEST", "192.0.2.0/24", "192.0.2.1", 3),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let mac_address = MacAddress::from_str("52:54:00:12:34:56").unwrap();

    let new_interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &mac_address,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    let machine_id = test_machine_id();
    db::machine::get_or_create(&mut txn, None, &machine_id, &new_interface).await?;

    let duplicate_interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &mac_address,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await;

    txn.commit().await?;

    assert!(matches!(
        duplicate_interface,
        Err(DatabaseError::NetworkSegmentDuplicateMacAddress(_))
    ));

    Ok(())
}
