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

use carbide_uuid::machine::{MachineIdSource, MachineType};
use model::dns::NewDomain;
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use model::predicted_machine_interface::NewPredictedMachineInterface;
use sqlx::Connection;

use super::*;
use crate::{dhcp_entry, machine_boot_override, predicted_machine_interface};

#[crate::sqlx_test]
async fn interface_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep prepared statements on the API connection while a migration commits.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE machine_interfaces ADD COLUMN test_added_column text;
         ALTER TABLE predicted_machine_interfaces ADD COLUMN test_added_column text;
         ALTER TABLE machine_boot_override ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_queries(connection: &mut PgConnection) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let host_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x77; 32],
        MachineType::Host,
    );
    let dpu_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x78; 32],
        MachineType::Dpu,
    );
    for machine_id in [host_id, dpu_id] {
        crate::machine::create(
            txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
    }
    let domain =
        crate::dns::domain::persist(NewDomain::new("projection.example"), txn.as_mut()).await?;
    let segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version, network_segment_type)
         VALUES ('projection-segment', 'V1-T0', $1) RETURNING id",
    )
    .bind(NetworkSegmentType::HostInband)
    .fetch_one(txn.as_mut())
    .await?;
    let mac_address = "02:00:00:00:00:78".parse()?;
    let interface_id = insert_machine_interface(
        txn.as_mut(),
        &segment_id,
        &mac_address,
        "projection-host".to_string(),
        Some(domain.id),
        true,
        InterfaceType::Data,
    )
    .await?;
    associate_interface_with_machine(
        &interface_id,
        MachineInterfaceAssociation::Machine(host_id),
        txn.as_mut(),
    )
    .await?;
    associate_interface_with_dpu_machine(&interface_id, &dpu_id, txn.as_mut()).await?;
    set_boot_interface_id(mac_address, "NIC.Slot.7-1", txn.as_mut()).await?;
    let last_dhcp = "2026-09-01T12:00:00Z".parse()?;
    update_last_dhcp(txn.as_mut(), interface_id, Some(last_dhcp)).await?;
    let address = "192.0.2.78".parse()?;
    crate::machine_interface_address::insert(
        txn.as_mut(),
        interface_id,
        address,
        AllocationType::Dhcp,
    )
    .await?;
    dhcp_entry::persist(
        dhcp_entry::DhcpEntry {
            machine_interface_id: interface_id,
            vendor_string: "PXEClient:Arch:00007".to_string(),
        },
        txn.as_mut(),
    )
    .await?;

    let expected = find_one(txn.as_mut(), interface_id).await?;
    assert_eq!(expected.id, interface_id);
    assert_eq!(expected.hostname, "projection-host");
    assert_eq!(expected.interface_type, InterfaceType::Data);
    assert!(expected.primary_interface);
    assert_eq!(expected.mac_address, mac_address);
    assert_eq!(expected.machine_id, Some(host_id));
    assert_eq!(expected.attached_dpu_machine_id, Some(dpu_id.try_into()?));
    assert_eq!(expected.domain_id, Some(domain.id));
    assert_eq!(expected.segment_id, segment_id);
    assert_eq!(expected.boot_interface_id.as_deref(), Some("NIC.Slot.7-1"));
    assert_eq!(expected.last_dhcp, Some(last_dhcp));
    assert_eq!(expected.addresses, vec![address]);
    assert_eq!(expected.vendors, vec!["PXEClient:Arch:00007"]);
    assert_eq!(
        expected.network_segment_type,
        Some(NetworkSegmentType::HostInband)
    );
    assert_eq!(
        expected.association_type,
        Some(InterfaceAssociationType::Machine)
    );
    assert_eq!(expected.power_shelf_id, None);
    assert_eq!(expected.switch_id, None);
    let found = find_by_machine_and_segment(txn.as_mut(), &host_id, segment_id).await?;
    assert_eq!(found.len(), 1);
    assert_eq!(
        serde_json::to_value(&found[0])?,
        serde_json::to_value(&expected)?
    );
    let found = find_by_ip(txn.as_mut(), address)
        .await?
        .expect("the populated interface is found");
    assert_eq!(
        serde_json::to_value(found)?,
        serde_json::to_value(&expected)?
    );
    // PostgreSQL validates the prepared result columns even when no interface matches.
    assert!(
        find_for_update_if_matches_instance_ip(
            txn.as_mut(),
            interface_id,
            "198.51.100.78".parse()?,
        )
        .await?
        .is_none()
    );

    let predicted = predicted_machine_interface::create(
        NewPredictedMachineInterface {
            machine_id: &host_id,
            mac_address,
            expected_network_segment_type: NetworkSegmentType::HostInband,
            boot_interface_id: Some("NIC.Slot.7-1".to_string()),
            primary_interface: true,
        },
        txn.as_mut(),
    )
    .await?;
    assert_eq!(predicted.machine_id, host_id);
    assert_eq!(predicted.mac_address, mac_address);
    assert_eq!(
        predicted.expected_network_segment_type,
        NetworkSegmentType::HostInband
    );
    assert_eq!(predicted.boot_interface_id.as_deref(), Some("NIC.Slot.7-1"));
    assert!(predicted.primary_interface);
    let found = predicted_machine_interface::find_by_machine_id(txn.as_mut(), &host_id).await?;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, predicted.id);
    assert_eq!(found[0].machine_id, predicted.machine_id);
    assert_eq!(found[0].mac_address, predicted.mac_address);
    assert_eq!(
        found[0].expected_network_segment_type,
        predicted.expected_network_segment_type
    );
    assert_eq!(found[0].boot_interface_id, predicted.boot_interface_id);
    assert_eq!(found[0].primary_interface, predicted.primary_interface);

    let boot_override = machine_boot_override::create(
        txn.as_mut(),
        interface_id,
        Some("#!ipxe\nboot".to_string()),
        Some("user-data".to_string()),
    )
    .await?
    .expect("the boot override was inserted");
    assert_eq!(boot_override.machine_interface_id, interface_id);
    assert_eq!(boot_override.custom_pxe.as_deref(), Some("#!ipxe\nboot"));
    assert_eq!(boot_override.custom_user_data.as_deref(), Some("user-data"));
    let found = machine_boot_override::find_optional(txn.as_mut(), interface_id)
        .await?
        .expect("the boot override is persisted");
    assert_eq!(
        found.machine_interface_id,
        boot_override.machine_interface_id
    );
    assert_eq!(found.custom_pxe, boot_override.custom_pxe);
    assert_eq!(found.custom_user_data, boot_override.custom_user_data);

    txn.rollback().await?;
    Ok(())
}
