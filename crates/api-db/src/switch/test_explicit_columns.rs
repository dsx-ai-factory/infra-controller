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

use carbide_uuid::machine::MachineInterfaceId;
use carbide_uuid::network::NetworkSegmentId;
use model::allocation_type::AllocationType;
use model::bmc_info::BmcInfo;
use model::rack::{MaintenanceActivity, RackFirmwareUpgradeState};
use model::switch::{SwitchConfig, SwitchNvosUpdateState, SwitchNvosUpdateStatus, SwitchStatus};
use sqlx::Connection;

use super::*;

#[crate::sqlx_test]
async fn switch_reader_survives_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_switch_reader(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API's prepared statements while another connection commits DDL.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE switches ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_switch_reader(&mut api_connection).await?;
    Ok(())
}

async fn exercise_switch_reader(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let bmc_mac: MacAddress = "02:00:00:00:68:01".parse()?;
    sqlx::query(
        "INSERT INTO expected_switches (serial_number, bmc_mac_address, bmc_username, bmc_password)
         VALUES ('projection-switch', $1, 'admin', 'test-password')",
    )
    .bind(bmc_mac)
    .execute(txn.as_mut())
    .await?;
    let mut expected = create(
        &mut txn,
        &NewSwitch {
            id: "sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60".parse()?,
            config: SwitchConfig {
                name: "projection switch config".to_string(),
                enable_nmxc: true,
                fabric_manager_config: None,
            },
            bmc_mac_address: Some(bmc_mac),
            metadata: Some(Metadata {
                name: "projection switch".to_string(),
                description: "populated switch projection".to_string(),
                labels: [("location".to_string(), "row-1".to_string())].into(),
            }),
            rack_id: Some(RackId::new("projection-rack")),
            slot_number: Some(3),
            tray_index: Some(5),
        },
    )
    .await?;

    let timestamp = "2026-09-01T12:00:00Z".parse()?;
    expected.status = Some(SwitchStatus {
        switch_name: "projection switch".to_string(),
        power_state: "on".to_string(),
        health_status: "ok".to_string(),
    });
    expected.controller_state.value = SwitchControllerState::Ready;
    expected.controller_state.version = expected.controller_state.version.increment();
    expected.controller_state_outcome = Some(PersistentStateHandlerOutcome::Wait {
        reason: "waiting for firmware".to_string(),
        source_ref: None,
    });
    expected.switch_maintenance_requested = Some(SwitchMaintenanceRequest {
        requested_at: timestamp,
        initiator: "projection-test".to_string(),
        operation: SwitchMaintenanceOperation::Reset,
    });
    expected.switch_reprovisioning_requested = Some(SwitchReprovisionRequest {
        requested_at: timestamp,
        initiator: "projection-test".to_string(),
        activities: vec![MaintenanceActivity::ConfigureNmxCluster],
    });
    expected.firmware_upgrade_status = Some(RackFirmwareUpgradeStatus {
        task_id: "firmware-job".to_string(),
        status: RackFirmwareUpgradeState::InProgress,
        started_at: Some(timestamp),
        ended_at: None,
    });
    expected.nvos_update_status = Some(SwitchNvosUpdateStatus {
        task_id: "nvos-job".to_string(),
        firmware_id: "nvos-image".to_string(),
        image_filename: "nvos.img".to_string(),
        status: SwitchNvosUpdateState::Started,
        started_at: Some(timestamp),
        ended_at: None,
    });
    expected.fabric_manager_status = Some(FabricManagerStatus {
        fabric_manager_state: FabricManagerState::Ok,
        addition_info: Some(CONTROL_PLANE_STATE_CONFIGURED.to_string()),
        reason: Some("configured".to_string()),
        error_message: None,
    });
    expected.nvlink_domain_uuid = Some("11111111-1111-1111-1111-111111111111".parse()?);
    expected.health_reports.replace = Some(HealthReport::empty("projection-health".to_string()));
    expected.bmc_credential_rotation_requested = true;
    expected.decommission_requested = true;
    expected.is_primary = true;
    expected.deleted = Some(timestamp);
    expected.version = expected.version.increment();

    sqlx::query(
        "UPDATE switches SET status = $1, controller_state = $2,
         controller_state_version = $3, controller_state_outcome = $4,
         switch_maintenance_requested = $5, switch_reprovisioning_requested = $6,
         firmware_upgrade_status = $7, nvos_update_status = $8,
         fabric_manager_status = $9, nvlink_domain_uuid = $10, health_reports = $11,
         bmc_credential_rotation_requested = $12, decommission_requested = $13,
         is_primary = $14, deleted = $15, version = $16 WHERE id = $17",
    )
    .bind(sqlx::types::Json(&expected.status))
    .bind(sqlx::types::Json(&expected.controller_state.value))
    .bind(expected.controller_state.version)
    .bind(sqlx::types::Json(&expected.controller_state_outcome))
    .bind(sqlx::types::Json(&expected.switch_maintenance_requested))
    .bind(sqlx::types::Json(&expected.switch_reprovisioning_requested))
    .bind(sqlx::types::Json(&expected.firmware_upgrade_status))
    .bind(sqlx::types::Json(&expected.nvos_update_status))
    .bind(sqlx::types::Json(&expected.fabric_manager_status))
    .bind(expected.nvlink_domain_uuid)
    .bind(sqlx::types::Json(&expected.health_reports))
    .bind(expected.bmc_credential_rotation_requested)
    .bind(expected.decommission_requested)
    .bind(expected.is_primary)
    .bind(expected.deleted)
    .bind(expected.version)
    .bind(expected.id)
    .execute(txn.as_mut())
    .await?;

    let segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version, network_segment_type)
         VALUES ('projection-switch-bmc', 'V1-T0', 'tenant') RETURNING id",
    )
    .fetch_one(txn.as_mut())
    .await?;
    let interface_id: MachineInterfaceId = sqlx::query_scalar(
        "INSERT INTO machine_interfaces
         (switch_id, association_type, segment_id, mac_address,
          primary_interface, hostname, interface_type)
         VALUES ($1, 'Switch', $2, $3, false, 'bmc', 'Bmc') RETURNING id",
    )
    .bind(expected.id)
    .bind(segment_id)
    .bind(bmc_mac)
    .fetch_one(txn.as_mut())
    .await?;
    let bmc_ip = "192.0.2.68".parse()?;
    crate::machine_interface_address::insert(
        txn.as_mut(),
        interface_id,
        bmc_ip,
        AllocationType::Dhcp,
    )
    .await?;
    expected.bmc_info = Some(BmcInfo {
        machine_interface_id: Some(interface_id),
        ip: Some(bmc_ip),
        mac: Some(bmc_mac),
        ..Default::default()
    });

    let found = find_by(&mut txn, ObjectColumnFilter::One(IdColumn, &expected.id)).await?;
    assert_eq!(found.len(), 1);
    let actual = &found[0];
    assert_eq!(actual.id, expected.id);
    assert_eq!(actual.config, expected.config);
    assert_eq!(actual.status, expected.status);
    assert_eq!(actual.deleted, expected.deleted);
    assert_eq!(actual.bmc_mac_address, expected.bmc_mac_address);
    assert_eq!(actual.bmc_info, expected.bmc_info);
    assert!(actual.bmc_credential_rotation_requested);
    assert!(actual.decommission_requested);
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
    assert_eq!(
        actual.switch_maintenance_requested,
        expected.switch_maintenance_requested
    );
    assert_eq!(
        actual.switch_reprovisioning_requested,
        expected.switch_reprovisioning_requested
    );
    assert_eq!(
        actual.firmware_upgrade_status,
        expected.firmware_upgrade_status
    );
    assert_eq!(actual.nvos_update_status, expected.nvos_update_status);
    assert_eq!(actual.fabric_manager_status, expected.fabric_manager_status);
    assert_eq!(actual.nvlink_domain_uuid, expected.nvlink_domain_uuid);
    assert_eq!(actual.rack_id, expected.rack_id);
    assert_eq!(actual.metadata, expected.metadata);
    assert_eq!(actual.version, expected.version);
    assert!(actual.is_primary);
    assert_eq!(actual.slot_number, expected.slot_number);
    assert_eq!(actual.tray_index, expected.tray_index);
    assert_eq!(actual.health_reports, expected.health_reports);
    txn.rollback().await?;
    Ok(())
}
