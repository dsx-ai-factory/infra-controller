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

use bmc_explorer::nv_generate_exploration_report;
use bmc_mock::test_support;
use nv_redfish::core::ODataId;
use serde_json::{Value, json};
use tokio::test;

use crate::common;

const PORT: &str = "/redfish/v1/Chassis/Self/NetworkAdapters/1/Ports/1";
const PORT_2: &str = "/redfish/v1/Chassis/Self/NetworkAdapters/1/Ports/2";

async fn explore_with_port(port: Value) -> Vec<model::site_explorer::Chassis> {
    let h = test_support::generic_ami_bmc_with_network_adapter_port(port).await;

    bmc_explorer::test_support::explore_network_adapter_ports(
        &h.service_root,
        &[ODataId::from("/redfish/v1/Chassis/Self".to_string())],
    )
    .await
    .unwrap()
}

#[test]
async fn adapter_ports_supply_missing_host_mac_addresses() {
    struct Case {
        name: &'static str,
        port: Value,
        expected: &'static str,
    }

    let cases = [
        Case {
            name: "Lenovo OEM fallback",
            port: json!({
                "@odata.id": PORT,
                "@odata.type": "#Port.v1_6_0.Port",
                "Id": "1",
                "Name": "Port 1",
                "LinkStatus": "LinkUp",
                "Oem": {
                    "Lenovo": { "PhysicalPortMacAddress": "946DAE53CB9B" }
                }
            }),
            expected: "94:6d:ae:53:cb:9b",
        },
        Case {
            name: "standard data takes precedence",
            port: json!({
                "@odata.id": PORT,
                "@odata.type": "#Port.v1_6_0.Port",
                "Id": "1",
                "Name": "Port 1",
                "Ethernet": {
                    "AssociatedMACAddresses": ["02:aa:bb:cc:dd:01"]
                },
                "Oem": {
                    "Lenovo": { "PhysicalPortMacAddress": { "incompatible": true } }
                }
            }),
            expected: "02:aa:bb:cc:dd:01",
        },
        Case {
            name: "Lenovo OEM fallback after malformed standard data",
            port: json!({
                "@odata.id": PORT,
                "@odata.type": "#Port.v1_6_0.Port",
                "Id": "1",
                "Name": "Port 1",
                "Ethernet": {
                    "AssociatedMACAddresses": ["not-a-mac"]
                },
                "Oem": {
                    "Lenovo": { "PhysicalPortMacAddress": "946DAE53CB9B" }
                }
            }),
            expected: "94:6d:ae:53:cb:9b",
        },
    ];

    for Case {
        name,
        port,
        expected,
    } in cases
    {
        let chassis = explore_with_port(port).await;
        let adapter = &chassis[0].network_adapters[0];
        assert_eq!(
            adapter.port_mac_addresses,
            vec![expected.parse().unwrap()],
            "unexpected adapter Port MAC for {name}",
        );
    }
}

#[test]
async fn adapter_ports_keep_valid_members_when_another_member_fails() {
    let h = test_support::generic_ami_bmc_with_network_adapter_ports(vec![
        json!({
            "@odata.id": PORT,
            "@odata.type": "#Port.v1_6_0.Port",
            "Id": "1",
            "Name": "Port 1",
            "Ethernet": {
                "AssociatedMACAddresses": ["02:aa:bb:cc:dd:01"]
            }
        }),
        json!({
            "@odata.id": PORT_2
        }),
    ])
    .await;

    let chassis = bmc_explorer::test_support::explore_network_adapter_ports(
        &h.service_root,
        &[ODataId::from("/redfish/v1/Chassis/Self".to_string())],
    )
    .await
    .unwrap();

    assert!(
        chassis[0].network_adapters[0]
            .port_mac_addresses
            .iter()
            .any(|mac| *mac == "02:aa:bb:cc:dd:01".parse().unwrap())
    );
}

#[test]
async fn adapter_ports_are_not_fetched_from_unlinked_chassis() {
    let h = test_support::generic_ami_bmc_with_network_adapter_port(json!({
        "@odata.id": PORT,
        "@odata.type": "#Port.v1_6_0.Port",
        "Id": "1",
        "Name": "Port 1",
        "Ethernet": {
            "AssociatedMACAddresses": ["02:aa:bb:cc:dd:01"]
        }
    }))
    .await;

    let chassis = bmc_explorer::test_support::explore_network_adapter_ports(
        &h.service_root,
        &[ODataId::from("/redfish/v1/Chassis/Other".to_string())],
    )
    .await
    .unwrap();

    assert!(chassis[0].network_adapters[0].port_mac_addresses.is_empty());
}

#[test]
async fn generic_ami_hosts_do_not_use_the_lenovo_port_fallback() {
    let h = test_support::generic_ami_bmc_with_network_adapter_port(json!({
        "@odata.id": PORT,
        "@odata.type": "#Port.v1_6_0.Port",
        "Id": "1",
        "Name": "Port 1",
        "Oem": {
            "Lenovo": { "PhysicalPortMacAddress": "946DAE53CB9B" }
        }
    }))
    .await;

    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();

    assert!(
        report.chassis[0].network_adapters[0]
            .port_mac_addresses
            .is_empty()
    );
    assert!(report.all_mac_addresses().is_empty());
}
