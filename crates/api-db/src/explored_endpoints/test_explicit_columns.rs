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

use model::site_explorer::{ComputerSystem, EndpointExplorationError, EthernetInterface};
use sqlx::Connection;

use super::*;

#[crate::sqlx_test]
async fn endpoint_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_endpoint_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep prepared statements on the API connection while a migration commits.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE explored_endpoints ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_endpoint_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_endpoint_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let address = "192.0.2.75".parse()?;
    let mac_address = "02:00:00:00:00:75".parse()?;
    let report = EndpointExplorationReport {
        model: Some("projection-platform".to_string()),
        systems: vec![ComputerSystem {
            id: "Bluefield".to_string(),
            serial_number: Some("projection-dpu".to_string()),
            ethernet_interfaces: vec![EthernetInterface {
                mac_address: Some(mac_address),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    insert(address, &report, true, txn.as_mut()).await?;
    set_pause_remediation(address, true, txn.as_mut()).await?;
    request_exploration_for_addresses(&[address], txn.as_mut()).await?;
    let boot_interface = MachineBootInterface {
        mac_address,
        interface_id: "NIC.Slot.7-1".to_string(),
    };
    set_boot_interface(address, &boot_interface, txn.as_mut()).await?;
    set_last_redfish_bmc_reset(address, txn.as_mut()).await?;
    set_last_ipmitool_bmc_reset(address, txn.as_mut()).await?;
    set_last_redfish_reboot(address, txn.as_mut()).await?;
    set_last_redfish_powercycle(address, txn.as_mut()).await?;

    let mut rows = find_all_by_ip(address, txn.as_mut()).await?;
    assert_eq!(rows.len(), 1);
    let mut expected = rows.remove(0);
    assert_eq!(expected.address, address);
    assert_eq!(expected.report, report);
    assert_eq!(expected.report_version.version_nr(), 1);
    assert_eq!(expected.preingestion_state, PreingestionState::Initial);
    assert!(!expected.waiting_for_explorer_refresh);
    assert!(expected.exploration_requested);
    assert!(expected.pause_ingestion_and_poweron);
    assert!(expected.pause_remediation);
    assert_eq!(expected.boot_interface_mac, Some(mac_address));
    assert_eq!(
        expected.boot_interface_id.as_deref(),
        Some(boot_interface.interface_id.as_str())
    );
    let reset_at = expected
        .last_redfish_bmc_reset
        .expect("the reset timestamp was written");
    assert_eq!(expected.last_ipmitool_bmc_reset, Some(reset_at));
    assert_eq!(expected.last_redfish_reboot, Some(reset_at));
    assert_eq!(expected.last_redfish_powercycle, Some(reset_at));

    for found in [
        find_by_ips(txn.as_mut(), vec![address]).await?,
        find_by_dpu_serial_numbers(txn.as_mut(), vec!["projection-dpu".to_string()]).await?,
        find_all(txn.as_mut()).await?,
        find_preingest_not_waiting_not_error(txn.as_mut()).await?,
        find_by_mac_address(txn.as_mut(), mac_address).await?,
    ] {
        assert_eq!(found, vec![expected.clone()]);
    }

    expected.preingestion_state = PreingestionState::UpgradeFirmwareWait {
        task_id: "projection-task".to_string(),
        final_version: "1.2.3".to_string(),
        upgrade_type: FirmwareComponentType::default(),
        power_drains_needed: Some(2),
        firmware_number: Some(3),
    };
    set_preingestion(address, expected.preingestion_state.clone(), txn.as_mut()).await?;
    assert_eq!(
        find_preingest_installing(txn.as_mut()).await?,
        vec![expected.clone()]
    );
    set_preingestion_complete(address, txn.as_mut()).await?;
    expected.preingestion_state = PreingestionState::Complete;
    assert_eq!(
        find_all_preingestion_complete(txn.as_mut()).await?,
        vec![expected.clone()]
    );

    let mut failed_report = report.clone();
    failed_report.last_exploration_error = Some(EndpointExplorationError::ConnectionTimeout {
        details: "projection timeout".to_string(),
    });
    assert_eq!(
        try_update(
            address,
            expected.report_version,
            &failed_report,
            false,
            txn.as_mut()
        )
        .await?,
        ConditionalWrite::Applied(()),
    );
    clear_last_known_error(address, txn.as_mut()).await?;
    let cleared = find_all_by_ip(address, txn.as_mut()).await?;
    assert_eq!(cleared.len(), 1);
    assert_eq!(cleared[0].report, report);
    assert_eq!(cleared[0].report_version.version_nr(), 3);
    assert!(cleared[0].waiting_for_explorer_refresh);
    assert!(!cleared[0].exploration_requested);
    assert_eq!(cleared[0].boot_interface_id, expected.boot_interface_id);
    assert_eq!(cleared[0].last_redfish_bmc_reset, Some(reset_at));

    // Reset the fixture without clearing the connection's prepared statements.
    txn.rollback().await?;
    Ok(())
}
