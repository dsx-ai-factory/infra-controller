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

use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_test_harness::prelude::*;
use config_version::ConfigVersion;
use db::ObjectColumnFilter;
use mac_address::MacAddress;
use model::metadata::Metadata;
use model::power_shelf::PowerShelfControllerState;
use model::site_explorer::{
    Chassis, ComputerSystem, EndpointExplorationError, EndpointExplorationReport, EndpointType,
    ExploredEndpoint, PreingestionState,
};
use rpc::forge::DhcpDiscovery;

use crate::env::Env;

trait EnvExt {
    fn new_power_shelf(
        &self,
        bmc_mac_address: &str,
        ip: &str,
        serial_number: &str,
    ) -> FakePowerShelf;
}

impl EnvExt for Env {
    fn new_power_shelf(
        &self,
        bmc_mac_address: &str,
        ip: &str,
        serial_number: &str,
    ) -> FakePowerShelf {
        FakePowerShelf {
            bmc_mac_address: bmc_mac_address.parse().unwrap(),
            ip: ip.to_string(),
            serial_number: serial_number.to_string(),
            bmc_username: "admin",
            bmc_password: "password",
            relay_address: self.underlay_segment.relay_address.to_string(),
        }
    }
}

struct FakePowerShelf {
    pub bmc_mac_address: MacAddress,
    pub serial_number: String,
    pub bmc_username: &'static str,
    pub bmc_password: &'static str,
    pub relay_address: String,
    pub ip: String, // DHCP assigned IP (may be different from ip_address)
}

impl FakePowerShelf {
    /// Builds model input for `add_expected_power_shelf`; `bmc_ip_address` drives the same static
    /// BMC pre-allocation path as expected machines / switches.
    fn as_expected_power_shelf(&self) -> model::expected_power_shelf::ExpectedPowerShelf {
        model::expected_power_shelf::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: self.bmc_mac_address,
            bmc_username: self.bmc_username.to_string(),
            bmc_password: self.bmc_password.to_string(),
            serial_number: self.serial_number.clone(),
            bmc_ip_address: Some(self.ip.parse().unwrap()),
            metadata: Metadata {
                name: format!("Test Power Shelf {}", self.serial_number),
                description: format!("A test power shelf with serial {}", self.serial_number),
                labels: HashMap::new(),
            },
            rack_id: None,
            bmc_retain_credentials: None,
        }
    }
}

#[sqlx_test]
async fn test_site_explorer_power_shelf_discovery(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let mut power_shelf = env.new_power_shelf("B8:3F:D2:90:97:B0", "", "PS123456789");

    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(
                power_shelf.bmc_mac_address.to_string(),
                power_shelf.relay_address.to_string(),
            )
            .tonic_request(),
        )
        .await?
        .into_inner();
    tracing::info!(
        mac_address = %power_shelf.bmc_mac_address,
        ip_address = %response.address,
        "DHCP assigned ip"
    );
    power_shelf.ip = response.address.clone();
    // Create expected power shelf entry in the database
    let mut txn = env.pool.begin().await?;
    let expected_power_shelf = power_shelf.as_expected_power_shelf();
    db::expected_power_shelf::create(&mut txn, expected_power_shelf.clone()).await?;
    txn.commit().await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        explorations_per_run: 1,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    // Mock power shelf exploration result
    explorer.insert_endpoint_result(
        power_shelf.ip.parse().unwrap(),
        Ok(EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            last_exploration_error: None,
            last_exploration_latency: None,
            vendor: Some(bmc_vendor::BMCVendor::Nvidia),
            machine_id: None,
            managers: Vec::new(),
            systems: vec![ComputerSystem {
                serial_number: Some("PS123456789".to_string()),
                ..Default::default()
            }],
            chassis: vec![Chassis {
                model: Some("PowerShelf-2000".to_string()),
                id: "powershelf".to_string(),
                manufacturer: Some("lite-on technology corp.".to_string()),
                part_number: Some("PS123456789".to_string()),
                serial_number: Some("PS123456789".to_string()),
                ..Default::default()
            }],
            service: Vec::new(),
            versions: HashMap::default(),
            model: Some("PowerShelf-2000".to_string()),
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
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
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

    // A declared power shelf takes manual ingestion, so it is created in the
    // same iteration it is explored -- no separate preingestion pass needed.
    assert_eq!(
        test_meter
            .formatted_metric("carbide_endpoint_explorations_count")
            .unwrap(),
        "1"
    );
    assert_eq!(
        test_meter
            .formatted_metric("carbide_site_explorer_created_power_shelves_count")
            .unwrap(),
        "1"
    );

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_power_shelf_discovery_with_static_ip(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    let power_shelf = env.new_power_shelf("B8:3F:D2:90:97:B0", "192.0.1.180", "PS123456789");

    tracing::info!(
        ip_address = %power_shelf.ip,
        mac_address = %power_shelf.bmc_mac_address,
        "Static ip assigned to power shelf mac",
    );
    // Create expected power shelf via the RPC handler, which
    // pre-allocates a machine interface with the static IP.
    env.api()
        .add_expected_power_shelf(tonic::Request::new(
            power_shelf.as_expected_power_shelf().into(),
        ))
        .await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        explorations_per_run: 1,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    // Mock power shelf exploration result
    explorer.insert_endpoint_result(
        power_shelf.ip.parse().unwrap(),
        Ok(EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            last_exploration_error: None,
            last_exploration_latency: None,
            vendor: Some(bmc_vendor::BMCVendor::Nvidia),
            machine_id: None,
            managers: Vec::new(),
            systems: vec![ComputerSystem {
                serial_number: Some("PS123456789".to_string()),
                ..Default::default()
            }],
            chassis: vec![Chassis {
                model: Some("PowerShelf-2000".to_string()),
                id: "powershelf".to_string(),
                manufacturer: Some("lite-on technology corp.".to_string()),
                part_number: Some("PS123456789".to_string()),
                serial_number: Some("PS123456789".to_string()),
                ..Default::default()
            }],
            service: Vec::new(),
            versions: HashMap::default(),
            model: Some("PowerShelf-2000".to_string()),
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
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
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

    // explorer.run_single_iteration().await.unwrap();
    // Check metrics
    assert_eq!(
        test_meter
            .formatted_metric("carbide_endpoint_explorations_count")
            .unwrap(),
        "1"
    );
    assert_eq!(
        test_meter
            .formatted_metric("carbide_site_explorer_created_power_shelves_count")
            .unwrap(),
        "1"
    );

    Ok(())
}

/// Power-shelf companion to `switch_skips_creation_when_bmc_mac_already_used`:
/// a second call to `create_power_shelf` for the same BMC MAC must not insert
/// a second row, even if the inputs we'd hash into a `PowerShelfId` differ.
#[sqlx_test]
async fn power_shelf_skips_creation_when_bmc_mac_already_used(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    let bmc_mac: MacAddress = "B8:3F:D2:90:97:E0".parse().unwrap();
    let bmc_ip: IpAddr = "192.168.1.200".parse().unwrap();

    // Seed an `expected_power_shelves` record so the foreign key on
    // power_shelves.bmc_mac_address (which references
    // expected_power_shelves.bmc_mac_address) is satisfied.
    let mut txn = env.pool.begin().await?;
    db::expected_power_shelf::create(
        &mut txn,
        model::expected_power_shelf::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac,
            bmc_username: "admin".to_string(),
            bmc_password: "password".to_string(),
            serial_number: "PS-EXISTING".to_string(),
            bmc_ip_address: Some(bmc_ip),
            metadata: Metadata {
                name: "PS-EXISTING-NAME".to_string(),
                description: String::new(),
                labels: HashMap::new(),
            },
            rack_id: None,
            bmc_retain_credentials: None,
        },
    )
    .await?;
    txn.commit().await?;

    let explorer = env.test_site_explorer(SiteExplorerConfig {
        create_power_shelves: Arc::new(true.into()),
        ..Default::default()
    });

    let explored_endpoint = ExploredEndpoint {
        address: bmc_ip,
        report: EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            vendor: Some(bmc_vendor::BMCVendor::Nvidia),
            chassis: vec![Chassis::default()],
            ..Default::default()
        },
        report_version: ConfigVersion::initial(),
        preingestion_state: PreingestionState::Complete,
        waiting_for_explorer_refresh: false,
        exploration_requested: false,
        last_redfish_bmc_reset: None,
        last_ipmitool_bmc_reset: None,
        last_redfish_reboot: None,
        last_redfish_powercycle: None,
        pause_remediation: false,
        boot_interface_mac: None,
        boot_interface_id: None,
        pause_ingestion_and_poweron: false,
    };

    let expected_first = model::expected_power_shelf::ExpectedPowerShelf {
        expected_power_shelf_id: None,
        bmc_mac_address: bmc_mac,
        bmc_username: "admin".to_string(),
        bmc_password: "password".to_string(),
        serial_number: "PS-EXISTING".to_string(),
        bmc_ip_address: Some(bmc_ip),
        metadata: Metadata {
            name: "PS-name-v1".to_string(),
            description: String::new(),
            labels: HashMap::new(),
        },
        rack_id: None,
        bmc_retain_credentials: None,
    };
    assert!(
        explorer
            .create_power_shelf(explored_endpoint.clone(), &expected_first, &env.pool)
            .await?,
        "first discovery must create a power_shelves row"
    );

    let mut txn = env.pool.begin().await?;
    let after_first = db::power_shelf::find_by(
        &mut txn,
        ObjectColumnFilter::<db::power_shelf::IdColumn>::All,
    )
    .await?;
    txn.commit().await?;
    assert_eq!(after_first.len(), 1);
    let original_id = after_first[0].id;

    // Second discovery for the same BMC MAC but a different name (which is
    // what currently feeds `PowerShelfId` generation). Without the BMC MAC
    // check, this would insert a second record.
    let expected_second = model::expected_power_shelf::ExpectedPowerShelf {
        metadata: Metadata {
            name: "PS-name-v2".to_string(),
            description: String::new(),
            labels: HashMap::new(),
        },
        ..expected_first
    };
    assert!(
        !explorer
            .create_power_shelf(explored_endpoint, &expected_second, &env.pool)
            .await?,
        "second discovery with same BMC MAC must not create a duplicate row"
    );

    let mut txn = env.pool.begin().await?;
    let after_second = db::power_shelf::find_by(
        &mut txn,
        ObjectColumnFilter::<db::power_shelf::IdColumn>::All,
    )
    .await?;
    txn.commit().await?;
    assert_eq!(after_second.len(), 1, "exactly one power_shelves row");
    assert_eq!(after_second[0].id, original_id, "original ID preserved");

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_power_shelf_with_expected_config(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    // Create a power shelf using the new FakePowerShelf struct
    let mut power_shelf = env.new_power_shelf("B8:3F:D2:90:97:B1", "192.168.1.100", "PS123456789");

    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(
                power_shelf.bmc_mac_address.to_string(),
                power_shelf.relay_address.to_string(),
            )
            .tonic_request(),
        )
        .await?
        .into_inner();
    tracing::info!(
        mac_address = %power_shelf.bmc_mac_address,
        ip_address = %response.address,
        "DHCP assigned ip"
    );
    power_shelf.ip = response.address.clone();
    // Create an expected power shelf entry
    let mut txn = env.pool.begin().await?;
    let expected_power_shelf = power_shelf.as_expected_power_shelf();

    db::expected_power_shelf::create(&mut txn, expected_power_shelf.clone()).await?;
    txn.commit().await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        explorations_per_run: 1,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    // Mock power shelf exploration result with matching serial
    explorer.insert_endpoint_result(
        power_shelf.ip.parse().unwrap(), // Use expected IP address, not DHCP-assigned IP
        Ok(EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            last_exploration_error: None,
            last_exploration_latency: None,
            vendor: Some(bmc_vendor::BMCVendor::Nvidia),
            machine_id: None,
            managers: Vec::new(),
            systems: vec![ComputerSystem {
                serial_number: Some("PS123456789".to_string()),
                ..Default::default()
            }],
            chassis: vec![Chassis {
                model: Some("PowerShelf-2000".to_string()),
                id: "powershelf".to_string(),
                manufacturer: Some("lite-on technology corp.".to_string()),
                part_number: Some("PS123456789".to_string()),
                serial_number: Some("PS123456789".to_string()),
                ..Default::default()
            }],
            service: Vec::new(),
            versions: HashMap::default(),
            model: Some("PowerShelf-2000".to_string()),
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

    explorer.run_single_iteration().await.unwrap();
    let mut txn = env.pool.begin().await?;
    db::explored_endpoints::set_preingestion_complete(power_shelf.ip.parse().unwrap(), &mut txn)
        .await?;
    txn.commit().await?;
    explorer.run_single_iteration().await.unwrap();

    // Verify power shelf was created with expected metadata
    let mut txn = env.pool.begin().await?;
    let power_shelves = db::power_shelf::find_by(
        &mut txn,
        ObjectColumnFilter::<db::power_shelf::IdColumn>::All,
    )
    .await?;
    txn.commit().await?;

    assert_eq!(power_shelves.len(), 1);
    let power_shelf_db = &power_shelves[0];
    assert_eq!(power_shelf_db.config.name, "Test Power Shelf PS123456789");

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_power_shelf_creation_limit(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    // Create multiple power shelf machines using FakePowerShelf
    let mut power_shelves = vec![
        env.new_power_shelf("B8:3F:D2:90:97:B2", "", "PS123456790"),
        env.new_power_shelf("B8:3F:D2:90:97:B3", "", "PS123456791"),
        env.new_power_shelf("B8:3F:D2:90:97:B4", "", "PS123456792"),
    ];
    for power_shelf in &mut power_shelves {
        let response = env
            .api()
            .discover_dhcp(
                DhcpDiscovery::builder(
                    power_shelf.bmc_mac_address.to_string(),
                    power_shelf.relay_address.to_string(),
                )
                .tonic_request(),
            )
            .await?
            .into_inner();
        tracing::info!(
            mac_address = %power_shelf.bmc_mac_address,
            ip_address = %response.address,
            "DHCP assigned ip"
        );
        power_shelf.ip = response.address.clone();
    }
    // Create expected power shelf entries in the database
    let mut txn = env.pool.begin().await?;
    for power_shelf in &power_shelves {
        let expected_power_shelf = power_shelf.as_expected_power_shelf();
        db::expected_power_shelf::create(&mut txn, expected_power_shelf.clone()).await?;
    }
    txn.commit().await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        explorations_per_run: 3,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 2, // Limit to 2 per run
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    // Mock exploration results for all power shelves
    for power_shelf in &power_shelves {
        explorer.insert_endpoint_result(
            power_shelf.ip.parse().unwrap(), // Use expected IP address, not DHCP-assigned IP
            Ok(EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                last_exploration_error: None,
                last_exploration_latency: None,
                vendor: Some(bmc_vendor::BMCVendor::Nvidia),
                machine_id: None,
                managers: Vec::new(),
                systems: vec![ComputerSystem {
                    serial_number: Some(power_shelf.serial_number.clone()),
                    ..Default::default()
                }],
                chassis: vec![Chassis {
                    model: Some("PowerShelf-2000".to_string()),
                    id: "powershelf".to_string(),
                    manufacturer: Some("lite-on technology corp.".to_string()),
                    part_number: Some("PS123456789".to_string()),
                    serial_number: Some(power_shelf.serial_number.clone()),
                    ..Default::default()
                }],
                service: Vec::new(),
                versions: HashMap::default(),
                model: Some("PowerShelf-2000".to_string()),
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
    }
    let test_meter = &env.test_harness.test_meter;

    // Declared power shelves take manual ingestion, so they are created in the
    // same iteration they are explored. With a per-run limit of 2, the first
    // iteration creates 2 of the 3...
    explorer.run_single_iteration().await.unwrap();
    assert_eq!(
        test_meter
            .formatted_metric("carbide_site_explorer_created_power_shelves_count")
            .unwrap(),
        "2"
    );

    // ...and the next iteration creates the remaining one.
    explorer.run_single_iteration().await.unwrap();
    assert_eq!(
        test_meter
            .formatted_metric("carbide_site_explorer_created_power_shelves_count")
            .unwrap(),
        "1"
    );

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_power_shelf_disabled(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    // Create a power shelf machine using FakePowerShelf
    let mut power_shelf = env.new_power_shelf("B8:3F:D2:90:97:B5", "", "PS123456793");
    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(
                power_shelf.bmc_mac_address.to_string(),
                power_shelf.relay_address.to_string(),
            )
            .tonic_request(),
        )
        .await?
        .into_inner();
    tracing::info!(
        mac_address = %power_shelf.bmc_mac_address,
        ip_address = %response.address,
        "DHCP assigned ip"
    );
    power_shelf.ip = response.address.clone();

    // Create expected power shelf entry in the database
    let mut txn = env.pool.begin().await?;
    let expected_power_shelf = power_shelf.as_expected_power_shelf();
    db::expected_power_shelf::create(&mut txn, expected_power_shelf.clone()).await?;
    txn.commit().await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        explorations_per_run: 1,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_power_shelves: Arc::new(false.into()), // Disabled
        power_shelves_created_per_run: 1,
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    // Mock power shelf exploration result
    explorer.insert_endpoint_result(
        power_shelf.ip.parse().unwrap(),
        Ok(EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            last_exploration_error: None,
            last_exploration_latency: None,
            vendor: Some(bmc_vendor::BMCVendor::Nvidia),
            machine_id: None,
            managers: Vec::new(),
            systems: vec![ComputerSystem {
                serial_number: Some("PS123456789".to_string()),
                ..Default::default()
            }],
            chassis: vec![Chassis {
                model: Some("PowerShelf-2000".to_string()),
                ..Default::default()
            }],
            service: Vec::new(),
            versions: HashMap::default(),
            model: Some("PowerShelf-2000".to_string()),
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

    // Check that no power shelves were created
    assert_eq!(
        test_meter
            .formatted_metric("carbide_site_explorer_created_power_shelves_count")
            .unwrap(),
        "0"
    );

    // Verify no power shelves exist in database
    let mut txn = env.pool.begin().await?;
    let power_shelves = db::power_shelf::find_by(
        &mut txn,
        ObjectColumnFilter::<db::power_shelf::IdColumn>::All,
    )
    .await?;
    txn.commit().await?;

    assert_eq!(power_shelves.len(), 0);

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_power_shelf_error_handling(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    // Create a power shelf machine using FakePowerShelf
    let mut power_shelf = env.new_power_shelf("B8:3F:D2:90:97:B6", "", "PS123456794");

    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(
                power_shelf.bmc_mac_address.to_string(),
                power_shelf.relay_address.to_string(),
            )
            .tonic_request(),
        )
        .await?
        .into_inner();
    tracing::info!(
        mac_address = %power_shelf.bmc_mac_address,
        ip_address = %response.address,
        "DHCP assigned ip"
    );
    power_shelf.ip = response.address.clone();
    // Create expected power shelf entry in the database
    let mut txn = env.pool.begin().await?;
    let expected_power_shelf = power_shelf.as_expected_power_shelf();
    db::expected_power_shelf::create(&mut txn, expected_power_shelf.clone()).await?;
    txn.commit().await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        explorations_per_run: 1,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    // Mock power shelf exploration error
    explorer.insert_endpoint_result(
        power_shelf.ip.parse().unwrap(),
        Err(EndpointExplorationError::Unauthorized {
            details: "Not authorized".to_string(),
            response_body: None,
            response_code: None,
        }),
    );
    let test_meter = &env.test_harness.test_meter;

    explorer.run_single_iteration().await.unwrap();

    let mut txn = env.pool.begin().await?;
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await?;
    assert_eq!(explored.len(), 1);

    // Verify error was recorded
    let report = &explored[0];
    assert_eq!(
        report.report.last_exploration_error,
        Some(EndpointExplorationError::Unauthorized {
            details: "Not authorized".to_string(),
            response_body: None,
            response_code: None,
        })
    );

    // Check metrics for error
    assert_eq!(
        test_meter
            .formatted_metric("carbide_endpoint_exploration_failures_count")
            .unwrap(),
        "{failure=\"unauthorized\"} 1"
    );

    Ok(())
}

/// Test power shelf state history error handling
#[sqlx_test]
async fn test_power_shelf_state_history_error_handling(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    // Create a power shelf using FakePowerShelf
    let mut power_shelf = env.new_power_shelf("B8:3F:D2:90:97:B0", "", "PS999999999");

    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(
                power_shelf.bmc_mac_address.to_string(),
                power_shelf.relay_address.to_string(),
            )
            .tonic_request(),
        )
        .await?
        .into_inner();
    tracing::info!(
        mac_address = %power_shelf.bmc_mac_address,
        ip_address = %response.address,
        "DHCP assigned ip"
    );
    power_shelf.ip = response.address.clone();
    // Create expected power shelf entry in the database
    let mut txn = env.pool.begin().await?;
    let expected_power_shelf = power_shelf.as_expected_power_shelf();
    db::expected_power_shelf::create(&mut txn, expected_power_shelf.clone()).await?;
    txn.commit().await?;

    // Create exploration report for power shelf
    let exploration_report = EndpointExplorationReport {
        endpoint_type: EndpointType::Bmc,
        last_exploration_error: None,
        last_exploration_latency: None,
        vendor: Some(bmc_vendor::BMCVendor::Nvidia),
        machine_id: None,
        managers: Vec::new(),
        systems: vec![ComputerSystem {
            serial_number: Some("PS999999999".to_string()),
            ..Default::default()
        }],
        chassis: vec![Chassis {
            model: Some("TestModel".to_string()),
            ..Default::default()
        }],
        service: Vec::new(),
        versions: HashMap::default(),
        model: Some("TestModel".to_string()),
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
    };

    let explored_endpoint = ExploredEndpoint {
        address: power_shelf.ip.parse().unwrap(),
        report: exploration_report.clone(),
        report_version: ConfigVersion::initial(),
        preingestion_state: PreingestionState::Complete,
        waiting_for_explorer_refresh: false,
        exploration_requested: false,
        last_redfish_bmc_reset: None,
        last_ipmitool_bmc_reset: None,
        last_redfish_reboot: None,
        last_redfish_powercycle: None,
        pause_remediation: false,
        boot_interface_mac: None,
        boot_interface_id: None,

        pause_ingestion_and_poweron: false,
    };

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        explorations_per_run: 2,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        ..Default::default()
    };

    let explorer = env.test_site_explorer(explorer_config);

    // Create the power shelf using site explorer
    assert!(
        explorer
            .create_power_shelf(explored_endpoint.clone(), &expected_power_shelf, &env.pool,)
            .await?
    );

    // Get the created power shelf
    let mut txn = env.pool.begin().await?;
    let power_shelves = db::power_shelf::find_by(
        &mut txn,
        ObjectColumnFilter::<db::power_shelf::IdColumn>::All,
    )
    .await?;
    txn.commit().await?;

    assert_eq!(power_shelves.len(), 1);
    let power_shelf = &power_shelves[0];
    let power_shelf_id = power_shelf.id;

    // Test state history with various state types
    let test_states = [
        PowerShelfControllerState::Initializing,
        PowerShelfControllerState::FetchingData,
        PowerShelfControllerState::Configuring,
        PowerShelfControllerState::Ready,
    ];

    let mut txn = env.pool.begin().await?;

    for state in test_states.iter() {
        let version = ConfigVersion::initial();

        let history_entry = db::state_history::persist(
            &mut txn,
            db::state_history::StateHistoryTableId::PowerShelf,
            &power_shelf_id,
            state,
            version,
        )
        .await?;

        assert_eq!(
            history_entry.state.replace(" ", ""),
            serde_json::to_string(&state)?
        );
        assert_eq!(history_entry.state_version, version);

        // Verify the entry can be retrieved
        let retrieved_history = db::state_history::for_object(
            &mut txn,
            db::state_history::StateHistoryTableId::PowerShelf,
            &power_shelf_id,
        )
        .await?;
        let found_entry = retrieved_history
            .iter()
            .find(|entry| entry.state_version == version);
        assert!(found_entry.is_some());
        assert_eq!(
            found_entry.unwrap().state.replace(" ", ""),
            serde_json::to_string(&state)?
        );
    }

    txn.commit().await?;

    // Test finding history for non-existent power shelf
    let mut txn = env.pool.begin().await?;
    let non_existent_id = carbide_uuid::power_shelf::PowerShelfId::new(
        carbide_uuid::power_shelf::PowerShelfIdSource::ProductBoardChassisSerial,
        [0; 32],
        carbide_uuid::power_shelf::PowerShelfType::Host,
    );
    let empty_history = db::state_history::for_object(
        &mut txn,
        db::state_history::StateHistoryTableId::PowerShelf,
        &non_existent_id,
    )
    .await?;
    txn.commit().await?;

    assert!(empty_history.is_empty());

    // Test finding history for empty list of power shelf IDs
    let mut txn = env.pool.begin().await?;
    let empty_history_map = db::state_history::find_by_object_ids(
        &mut txn,
        db::state_history::StateHistoryTableId::PowerShelf,
        &[] as &[carbide_uuid::power_shelf::PowerShelfId],
    )
    .await?;
    txn.commit().await?;

    assert!(empty_history_map.is_empty());

    Ok(())
}
