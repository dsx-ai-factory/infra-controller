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

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use carbide_site_explorer::SwitchCreator;
use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_test_harness::prelude::*;
use db::{self, explored_endpoints as db_explored_endpoints};
use mac_address::MacAddress;
use model::metadata::Metadata;
use model::site_explorer::{
    Chassis, ComputerSystem, EndpointExplorationReport, EndpointType, ExploredManagedSwitch,
};
use model::switch::SwitchSearchFilter;
use rpc::forge::DhcpDiscovery;

use crate::env::Env;

fn expected_switch_fixture(
    bmc_mac: MacAddress,
    nvos_mac: MacAddress,
    serial: &str,
) -> model::expected_switch::ExpectedSwitch {
    model::expected_switch::ExpectedSwitch {
        expected_switch_id: None,
        bmc_mac_address: bmc_mac,
        nvos_mac_addresses: vec![nvos_mac],
        serial_number: serial.to_string(),
        bmc_username: "ADMIN".to_string(),
        bmc_password: "Pwd2023".to_string(),
        nvos_username: None,
        nvos_password: None,
        bmc_ip_address: None,
        nvos_ip_address: None,
        metadata: Metadata {
            name: format!("Test Switch {serial}"),
            description: String::new(),
            labels: HashMap::new(),
        },
        rack_id: None,
        bmc_retain_credentials: None,
    }
}

fn explored_managed_switch_fixture(
    bmc_ip: IpAddr,
    nvos_mac: MacAddress,
    chassis_serial: Option<&str>,
) -> ExploredManagedSwitch {
    let chassis = Chassis {
        id: "mgx_nvswitch_0".to_string(),
        manufacturer: Some("NVIDIA".to_string()),
        model: Some("Switch".to_string()),
        serial_number: chassis_serial.map(String::from),
        part_number: chassis_serial.map(String::from),
        ..Default::default()
    };
    ExploredManagedSwitch {
        bmc_ip,
        nv_os_mac_addresses: vec![nvos_mac],
        report: EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            vendor: Some(bmc_vendor::BMCVendor::Nvidia),
            chassis: vec![chassis],
            model: Some("Switch".to_string()),
            ..Default::default()
        },
    }
}

#[sqlx_test]
async fn test_site_explorer_switch_discovery(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    let bmc_mac: MacAddress = "B8:3F:D2:90:97:C0".parse().unwrap();
    let serial_number = "SW-SN-001".to_string();
    let bmc_username = "ADMIN".to_string();
    let bmc_password = "Pwd2023".to_string();

    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(
                bmc_mac.to_string(),
                env.underlay_segment.relay_address.to_string(),
            )
            .tonic_request(),
        )
        .await?
        .into_inner();
    tracing::info!(
        mac_address = %bmc_mac,
        ip_address = %response.address,
        "DHCP assigned ip"
    );
    let switch_ip = response.address.clone();

    let mut txn = env.pool.begin().await?;
    let expected_switch = model::expected_switch::ExpectedSwitch {
        expected_switch_id: None,
        bmc_mac_address: bmc_mac,
        nvos_mac_addresses: vec![bmc_mac],
        serial_number: serial_number.clone(),
        bmc_username: bmc_username.clone(),
        bmc_password: bmc_password.clone(),
        nvos_username: None,
        nvos_password: None,
        bmc_ip_address: None,
        nvos_ip_address: None,
        metadata: Metadata {
            name: format!("Test Switch {}", serial_number),
            description: format!("A test switch with serial {}", serial_number),
            labels: HashMap::new(),
        },
        rack_id: None,
        bmc_retain_credentials: None,
    };
    db::expected_switch::create(&mut txn, expected_switch).await?;
    txn.commit().await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        explorations_per_run: 1,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_switches: Arc::new(true.into()),
        switches_created_per_run: 1,
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoint_result(
        switch_ip.parse().unwrap(),
        Ok(EndpointExplorationReport {
            hardware_class: None,
            endpoint_type: EndpointType::Bmc,
            last_exploration_error: None,
            last_exploration_latency: None,
            vendor: Some(bmc_vendor::BMCVendor::Nvidia),
            machine_id: None,
            managers: Vec::new(),
            systems: vec![ComputerSystem {
                serial_number: Some(serial_number.clone()),
                ..Default::default()
            }],
            chassis: vec![Chassis {
                id: "mgx_nvswitch_0".to_string(),
                model: Some("Switch".to_string()),
                manufacturer: Some("NVIDIA".to_string()),
                serial_number: Some(serial_number.clone()),
                part_number: Some(serial_number.clone()),
                ..Default::default()
            }],
            service: Vec::new(),
            versions: HashMap::default(),
            model: Some("Switch".to_string()),
            machine_setup_status: None,
            secure_boot_status: None,
            lockdown_status: None,
            power_shelf_id: None,
            switch_id: None,
            compute_tray_index: None,
            physical_slot_number: None,
            revision_id: None,
            topology_id: None,
            remediation_error: None,
        }),
    );
    let test_meter = &env.test_harness.test_meter;

    explorer.run_single_iteration().await.unwrap();

    let mut txn = env.pool.begin().await?;
    let explored = db_explored_endpoints::find_all(txn.as_mut()).await.unwrap();
    txn.commit().await?;
    assert_eq!(explored.len(), 1);

    for report in &explored {
        assert_eq!(report.report_version.version_nr(), 1);
        let guard = explorer.endpoint_explorer().reports.lock().unwrap();
        let res = guard.get(&report.address).unwrap();
        assert!(res.is_ok());
        assert_eq!(
            res.clone().unwrap().endpoint_type,
            report.report.endpoint_type
        );
        assert_eq!(res.clone().unwrap().vendor, report.report.vendor);
        assert_eq!(res.clone().unwrap().systems, report.report.systems);
    }

    let mut txn = env.pool.begin().await?;
    db_explored_endpoints::set_preingestion_complete(switch_ip.parse().unwrap(), &mut txn).await?;
    txn.commit().await?;

    explorer.run_single_iteration().await.unwrap();

    assert_eq!(
        test_meter
            .formatted_metric("carbide_endpoint_explorations_count")
            .unwrap(),
        "1"
    );

    let mut txn = env.pool.begin().await?;
    let switches = db::switch::find_ids(txn.as_mut(), SwitchSearchFilter::default()).await?;
    println!("switches: {:?}", switches);
    txn.commit().await?;
    assert_eq!(switches.len(), 1, "Expected one switch to be created");

    Ok(())
}

/// When a switch is rediscovered with a chassis serial that hashes to a new
/// `SwitchId`, the BMC MAC check must keep us from inserting a second record.
#[sqlx_test]
async fn switch_skips_creation_when_bmc_mac_already_used(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let bmc_mac: MacAddress = "B8:3F:D2:90:97:D0".parse().unwrap();
    let nvos_mac: MacAddress = "B8:3F:D2:90:97:D1".parse().unwrap();

    let expected_switch = expected_switch_fixture(bmc_mac, nvos_mac, "SW-DRIFT");
    let mut txn = env.pool.begin().await?;
    db::expected_switch::create(&mut txn, expected_switch.clone()).await?;
    txn.commit().await?;

    let switch_creator = SwitchCreator::new(env.pool.clone(), SiteExplorerConfig::default());

    // First discovery, we get a real serial, which succeeds,
    // and inserts a switches row.
    assert!(
        switch_creator
            .create_managed_switch(
                &explored_managed_switch_fixture(
                    "10.0.0.1".parse().unwrap(),
                    nvos_mac,
                    Some("SW-DRIFT-v1"),
                ),
                &expected_switch,
                &env.pool,
            )
            .await?,
        "first discovery must create a switch row"
    );

    let mut txn = env.pool.begin().await?;
    let ids_after_first = db::switch::find_ids(txn.as_mut(), SwitchSearchFilter::default()).await?;
    txn.commit().await?;
    assert_eq!(ids_after_first.len(), 1);
    let original_id = ids_after_first[0];

    // Second discovery, we hit the same BMC MAC, but get a different chassis serial.
    // Without the BMC MAC check, this would give us a different SwitchId and insert
    // a second record.
    assert!(
        !switch_creator
            .create_managed_switch(
                &explored_managed_switch_fixture(
                    "10.0.0.1".parse().unwrap(),
                    nvos_mac,
                    Some("SW-DRIFT-v2"),
                ),
                &expected_switch,
                &env.pool,
            )
            .await?,
        "second discovery with drifted fingerprint must not create a duplicate row"
    );

    let mut txn = env.pool.begin().await?;
    let ids_after_second =
        db::switch::find_ids(txn.as_mut(), SwitchSearchFilter::default()).await?;
    txn.commit().await?;
    assert_eq!(
        ids_after_second,
        vec![original_id],
        "exactly one switch row, original ID preserved"
    );

    Ok(())
}

/// A switch BMC reporting `"NA"` for its chassis serial is treated as a
/// missing serial: `generate_switch_id` should error with
/// `MissingHardwareInfo::Serial` rather than give us a junk `SwitchId`, and
/// no record gets created. The next exploration cycle picks the switch up
/// once a real serial is reported.
#[sqlx_test]
async fn switch_treats_na_chassis_serial_as_missing(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let bmc_mac: MacAddress = "B8:3F:D2:90:97:D2".parse().unwrap();
    let nvos_mac: MacAddress = "B8:3F:D2:90:97:D3".parse().unwrap();

    let expected_switch = expected_switch_fixture(bmc_mac, nvos_mac, "SW-NA");
    let mut txn = env.pool.begin().await?;
    db::expected_switch::create(&mut txn, expected_switch.clone()).await?;
    txn.commit().await?;

    let switch_creator = SwitchCreator::new(env.pool.clone(), SiteExplorerConfig::default());

    let result = switch_creator
        .create_managed_switch(
            &explored_managed_switch_fixture("10.0.0.2".parse().unwrap(), nvos_mac, Some("NA")),
            &expected_switch,
            &env.pool,
        )
        .await;
    assert!(
        result.is_err(),
        "placeholder NA chassis serial must surface as an error, got: {result:?}"
    );

    let mut txn = env.pool.begin().await?;
    let ids = db::switch::find_ids(txn.as_mut(), SwitchSearchFilter::default()).await?;
    txn.commit().await?;
    assert!(
        ids.is_empty(),
        "no switch row must be inserted when chassis serial is NA"
    );

    Ok(())
}
