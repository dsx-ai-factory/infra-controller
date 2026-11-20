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
use std::net::IpAddr;
use std::str::FromStr;

use carbide_api_core::test_support::default_config;
use carbide_test_harness::prelude::*;
use model::site_explorer::{
    BlueFieldOperatingMode, Chassis, ComputerSystem, ComputerSystemAttributes,
    EndpointExplorationReport, EndpointType, NetworkAdapter, PCIeDevice,
};

fn pcie(part: &str, fw: &str, serial: &str, id: &str) -> PCIeDevice {
    PCIeDevice {
        description: Some(format!("NVIDIA BlueField-3 {part}")),
        firmware_version: Some(fw.to_string()),
        gpu_vendor: None,
        id: Some(id.to_string()),
        manufacturer: Some("Nvidia".to_string()),
        name: Some("Network Device".to_string()),
        part_number: Some(part.to_string()),
        serial_number: Some(serial.to_string()),
        status: None,
    }
}

// Exercises the paginated explored-device read path: seed a host whose Redfish
// PCIe inventory shows a NIC-mode DPU plus that DPU's own BMC endpoint, then
// confirm the host-ids step lists the host and the by-ids step projects the
// device and joins it to its DPU BMC by serial.
#[sqlx_test]
async fn test_find_explored_mlx_devices(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = default_config::get();
    config.site_explorer.host_dpu_rules = serde_json::from_value(serde_json::json!([{
        "id": "site-oem-supernic",
        "sources": ["chassis", "chassis_network_adapter"],
        "part_numbers": [{"exact": "OEM-123"}],
        "product": "bluefield3_supernic"
    }]))?;
    let env = TestHarness::builder(pool)
        .with_api_builder_fn(move |builder| builder.with_runtime_config(config.into()))
        .build()
        .await;

    let host_ip = IpAddr::from_str("192.0.2.20")?;
    let dpu_ip = IpAddr::from_str("192.0.2.50")?;

    // The host BMC reports a NIC-mode DPU (900-9D3B4) running outdated NIC FW.
    let host_report = EndpointExplorationReport {
        endpoint_type: EndpointType::Bmc,
        systems: vec![ComputerSystem {
            pcie_devices: vec![pcie(
                "900-9D3B4-00EN-EA0",
                "32.38.1002",
                "MT2403X00984",
                "188-0",
            )],
            ..Default::default()
        }],
        ..Default::default()
    };
    // The DPU's own BMC endpoint, keyed by the matching serial, reporting NIC mode.
    let dpu_report = EndpointExplorationReport {
        endpoint_type: EndpointType::Bmc,
        systems: vec![ComputerSystem {
            id: "Bluefield".to_string(),
            serial_number: Some("MT2403X00984".to_string()),
            pcie_devices: vec![pcie("SN37C16116", "1.2", "self", "1")],
            attributes: ComputerSystemAttributes {
                nic_mode: Some(BlueFieldOperatingMode::Nic),
                ..Default::default()
            },
            ..Default::default()
        }],
        chassis: vec![Chassis {
            id: "Card1".to_string(),
            model: Some("NVIDIA BlueField 3".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };

    let mut txn = env.db_txn().await;
    db::explored_endpoints::insert(host_ip, &host_report, false, &mut txn).await?;
    db::explored_endpoints::insert(dpu_ip, &dpu_report, false, &mut txn).await?;
    // This host has no PCIe inventory at all. Its chassis identifies a site-specific SKU.
    let oem_host = EndpointExplorationReport {
        chassis: vec![Chassis {
            part_number: Some("OEM-123".into()),
            serial_number: Some("CHASSIS-SN".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    db::explored_endpoints::insert("192.0.2.21".parse()?, &oem_host, false, &mut txn).await?;
    // Only the adapter identifies the card. The PCIe row supplies firmware and slot.
    let adapter_host = EndpointExplorationReport {
        systems: vec![ComputerSystem {
            pcie_devices: vec![pcie("unrecognized", "1.2", " ADAPTER-SN ", "42")],
            ..Default::default()
        }],
        chassis: vec![Chassis {
            network_adapters: vec![NetworkAdapter {
                part_number: Some("OEM-123".into()),
                serial_number: Some("ADAPTER-SN".into()),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    db::explored_endpoints::insert("192.0.2.22".parse()?, &adapter_host, false, &mut txn).await?;
    txn.commit().await?;

    // List the host BMC IPs carrying BlueField devices -- the DPU endpoint is
    // excluded, since it reports no host-side inventory.
    let mut host_ids = env
        .api()
        .find_explored_mlx_device_host_ids(tonic::Request::new(
            ::rpc::site_explorer::ExploredMlxDeviceHostSearchFilter {},
        ))
        .await
        .map(|response| response.into_inner())
        .unwrap()
        .host_ids;
    host_ids.sort();
    assert_eq!(host_ids, ["192.0.2.20", "192.0.2.21", "192.0.2.22"]);

    // Fetch the devices for that page of hosts.
    let mut devices = env
        .api()
        .find_explored_mlx_devices_by_ids(tonic::Request::new(
            ::rpc::site_explorer::ExploredMlxDevicesByIdsRequest {
                host_ids: host_ids.clone(),
            },
        ))
        .await
        .map(|response| response.into_inner())
        .unwrap()
        .devices;

    devices.sort_by(|a, b| a.host_bmc_ip.cmp(&b.host_bmc_ip));
    assert_eq!(devices.len(), 3);
    let oem = &devices[1];
    assert_eq!(oem.host_bmc_ip, "192.0.2.21");
    assert_eq!(
        oem.device_kind,
        ::rpc::site_explorer::MlxDeviceKind::Bf3SuperNic as i32
    );
    assert_eq!(oem.part_number.as_deref(), Some("OEM-123"));
    assert_eq!(oem.serial_number.as_deref(), Some("CHASSIS-SN"));
    assert_eq!(oem.pcie_id, None);
    assert_eq!(oem.firmware_version, None);
    assert_eq!(oem.dpu_bmc_ip, None);
    let adapter = &devices[2];
    assert_eq!(adapter.host_bmc_ip, "192.0.2.22");
    assert_eq!(adapter.device_kind, oem.device_kind);
    assert_eq!(adapter.part_number.as_deref(), Some("OEM-123"));
    assert_eq!(adapter.serial_number.as_deref(), Some("ADAPTER-SN"));
    assert_eq!(adapter.pcie_id.as_deref(), Some("42"));
    assert_eq!(adapter.firmware_version.as_deref(), Some("1.2"));
    let device = &devices[0];
    assert_eq!(device.host_bmc_ip, "192.0.2.20");
    assert_eq!(device.firmware_version.as_deref(), Some("32.38.1002"));
    assert_eq!(
        device.device_kind,
        ::rpc::site_explorer::MlxDeviceKind::Bf3NicMode as i32
    );
    // Joined to its DPU endpoint by serial.
    assert_eq!(device.dpu_bmc_ip.as_deref(), Some("192.0.2.50"));
    assert_eq!(
        device.nic_mode,
        Some(::rpc::site_explorer::BlueFieldOperatingMode::Nic as i32)
    );

    // A direct lookup also excludes the DPU BMC, even though it reports its own PCIe card.
    let other_devices = env
        .api()
        .find_explored_mlx_devices_by_ids(tonic::Request::new(
            ::rpc::site_explorer::ExploredMlxDevicesByIdsRequest {
                host_ids: vec![dpu_ip.to_string(), "198.51.100.1".to_string()],
            },
        ))
        .await
        .map(|response| response.into_inner())
        .unwrap()
        .devices;
    assert!(other_devices.is_empty());

    Ok(())
}
