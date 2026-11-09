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

use std::collections::{HashMap, HashSet};

use itertools::Itertools;
use model::expected_machine::{ExpectedMachine, HostDpuPolicy};
use model::site_explorer::{BlueFieldOperatingMode, ExploredEndpoint};

use crate::endpoint_exploration_report_ext::{DeviceIdentifiers, EndpointExplorationReportExt};
use crate::errors::SiteExplorerError;

/// Expected DPU count and unique host/BMC matches collected before configuration.
pub(super) struct HostDpuMatches<'a> {
    /// DPU count from usable inventory serials or matched/configured serials,
    /// whichever is larger.
    /// Configured serials may fill unmatched inventory slots.
    pub(super) expected_total: usize,
    pub(super) matched_dpus: Vec<MatchedDpu<'a>>,
}

impl<'a> HostDpuMatches<'a> {
    /// Matches inventory devices and configured DPU serials against explored BMCs.
    ///
    /// Host inventory part numbers take precedence; missing part numbers use
    /// the DPU BMC report for both inventory and expected-serial matches.
    /// Expected serials are trimmed and deduplicated; blank entries are ignored.
    /// They can replace unmatched inventory serials without adding another slot.
    pub(super) fn new(
        ep: &ExploredEndpoint,
        dpu_sn_to_endpoint: &'a HashMap<String, ExploredEndpoint>,
        expected_machine: Option<&'a ExpectedMachine>,
        host_dpu_policy: HostDpuPolicy,
    ) -> Self {
        let inventory_devices = ep.report.devices_with_unique_serials();

        // All unique devices serial numbers reported by host.
        let inventory_serials = inventory_devices
            .iter()
            .filter_map(InventorySerial::from_device)
            .collect::<HashSet<_>>();

        // All unique DPU serial numbers from expected host
        // configuration.
        let configured_serials = expected_machine
            .into_iter()
            .flat_map(|machine| &machine.data.fallback_dpu_serial_numbers)
            .filter_map(|serial| ConfiguredDpuSerial::parse(serial))
            .unique()
            .collect::<Vec<_>>();

        // Count reported devices identified as DPUs by part number or serial.
        let reported_total = inventory_devices
            .iter()
            .filter(|device| {
                device.is_bluefield()
                    || InventorySerial::from_device(device).is_some_and(|serial| {
                        serial.find_explored_dpu(dpu_sn_to_endpoint).is_some()
                            || configured_serials
                                .iter()
                                .any(|configured| configured.matches_inventory(serial))
                    })
            })
            .count();

        // Resolve inventory devices using their host-reported part numbers.
        let inventory_matches = inventory_devices.into_iter().filter_map(|device| {
            MatchedDpu::from_inventory(device, dpu_sn_to_endpoint, host_dpu_policy)
        });

        // Configured serials already reported by the host use the inventory path.
        // Resolve only the remaining configured serials through the fallback path.
        let unreported_configured_matches = configured_serials
            .iter()
            .copied()
            .filter(|serial| !serial.is_reported_in(&inventory_serials))
            .filter_map(|serial| {
                MatchedDpu::from_configured(serial, dpu_sn_to_endpoint, host_dpu_policy)
            });

        // Both paths now contain MatchedDpu values; their serial sources stay distinct.
        let matched_dpus = inventory_matches
            .chain(unreported_configured_matches)
            .collect::<Vec<_>>();

        // Every configured serial must be explored, including ones also in inventory.
        let unexplored_configured_total = configured_serials
            .iter()
            .filter(|serial| serial.find_explored_dpu(dpu_sn_to_endpoint).is_none())
            .count();

        // Fallback can fill unmatched inventory slots. Also require every match and
        // every configured DPU still awaiting exploration; mode readiness is checked later.
        let expected_total = reported_total.max(matched_dpus.len() + unexplored_configured_total);

        Self {
            expected_total,
            matched_dpus,
        }
    }
}

/// A trimmed, nonempty serial read from a host inventory device.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct InventorySerial<'a>(&'a str);

impl<'a> InventorySerial<'a> {
    fn from_device(device: &DeviceIdentifiers<'a>) -> Option<Self> {
        device.serial_number().map(Self)
    }

    fn find_explored_dpu(
        self,
        endpoints: &HashMap<String, ExploredEndpoint>,
    ) -> Option<&ExploredEndpoint> {
        endpoints.get(self.0)
    }
}

/// A trimmed, nonempty DPU serial declared in ExpectedMachine configuration.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct ConfiguredDpuSerial<'a>(&'a str);

impl<'a> ConfiguredDpuSerial<'a> {
    fn parse(serial: &'a str) -> Option<Self> {
        let serial = serial.trim();
        (!serial.is_empty()).then_some(Self(serial))
    }

    fn matches_inventory(&self, inventory: InventorySerial<'_>) -> bool {
        self.0 == inventory.0
    }

    fn is_reported_in(&self, inventory: &HashSet<InventorySerial<'_>>) -> bool {
        inventory.contains(&InventorySerial(self.0))
    }

    fn find_explored_dpu(
        self,
        endpoints: &HashMap<String, ExploredEndpoint>,
    ) -> Option<&ExploredEndpoint> {
        endpoints.get(self.0)
    }
}

/// A host/DPU association established by serial, independent of mode readiness.
pub(super) struct MatchedDpu<'a> {
    /// Already discovered DPU BMC endpoint.
    pub(super) endpoint: &'a ExploredEndpoint,
    /// Target resolved from the host policy and this match's source part number.
    pub(super) target_operating_mode: Option<BlueFieldOperatingMode>,
    /// Source of the association and its failure diagnostic context.
    source: DpuMatchSource<'a>,
}

/// Where a host/DPU serial association was found.
#[derive(Clone, Copy)]
enum DpuMatchSource<'a> {
    /// A PCIe device, chassis, or network adapter in the host report.
    Inventory,
    /// A serial supplied only by ExpectedMachine, retained for failure diagnostics.
    ExpectedSerial(ConfiguredDpuSerial<'a>),
}

impl<'a> MatchedDpu<'a> {
    fn from_inventory(
        device: DeviceIdentifiers<'_>,
        endpoints: &'a HashMap<String, ExploredEndpoint>,
        policy: HostDpuPolicy,
    ) -> Option<Self> {
        let serial = InventorySerial::from_device(&device)?;
        let endpoint = serial.find_explored_dpu(endpoints)?;
        let part_number = device
            .part_number
            .or_else(|| endpoint.report.dpu_part_number());

        Some(Self {
            endpoint,
            target_operating_mode: policy.target_operating_mode(part_number),
            source: DpuMatchSource::Inventory,
        })
    }

    fn from_configured(
        serial: ConfiguredDpuSerial<'a>,
        endpoints: &'a HashMap<String, ExploredEndpoint>,
        policy: HostDpuPolicy,
    ) -> Option<Self> {
        let endpoint = serial.find_explored_dpu(endpoints)?;

        Some(Self {
            endpoint,
            target_operating_mode: policy.target_operating_mode(endpoint.report.dpu_part_number()),
            source: DpuMatchSource::ExpectedSerial(serial),
        })
    }

    pub(super) fn warn_mode_check_failed(&self, err: &SiteExplorerError) {
        let dpu_ep = self.endpoint;
        match self.source {
            DpuMatchSource::Inventory => {
                tracing::warn!(
                    target: "carbide_site_explorer",
                    bmc_ip_address = %dpu_ep.address,
                    error = %err,
                    "failed to check DPU mode; skipping this device",
                );
            }
            DpuMatchSource::ExpectedSerial(serial_number) => {
                tracing::warn!(
                    target: "carbide_site_explorer",
                    bmc_ip_address = %dpu_ep.address,
                    dpu_serial_number = %serial_number.0,
                    error = %err,
                    "failed to check expected-serial DPU mode; skipping this device this pass",
                );
            }
        }
    }
}

#[cfg(test)]
pub(super) fn matched_inventory_dpu(endpoint: &ExploredEndpoint) -> MatchedDpu<'_> {
    MatchedDpu {
        endpoint,
        target_operating_mode: None,
        source: DpuMatchSource::Inventory,
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_harness::test_support::fixture_config::{
        FixtureDefault as _, ManagedHostConfigExt as _,
    };
    use carbide_test_support::{Check, check_values};
    use model::expected_machine::ExpectedMachineData;
    use model::site_explorer::{Chassis, EndpointExplorationReport, NetworkAdapter};
    use model::test_support::{DpuConfig, ManagedHostConfig};

    use super::*;
    use crate::test_support::{bf2_dpu, explored_endpoint};

    #[test]
    fn matched_targets_preserve_part_number_source_after_host_report_is_dropped() {
        let dpu = DpuConfig::default();
        let serial = dpu.serial.clone();
        let serial_only_dpu = DpuConfig::default();
        let serial_only = serial_only_dpu.serial.clone();
        let expected_dpu = DpuConfig::default();
        let expected_serial = expected_dpu.serial.clone();
        let host_report: EndpointExplorationReport = ManagedHostConfig::default()
            .with_dpus(vec![dpu.clone(), serial_only_dpu.clone()])
            .into();
        let endpoints = HashMap::from([
            (serial.clone(), explored_endpoint(dpu.into())),
            (serial_only, explored_endpoint(serial_only_dpu.into())),
            (
                expected_serial.clone(),
                explored_endpoint(expected_dpu.into()),
            ),
        ]);
        let expected_machine = ExpectedMachine {
            id: None,
            bmc_mac_address: "00:11:22:33:44:55".parse().unwrap(),
            data: ExpectedMachineData {
                fallback_dpu_serial_numbers: vec![serial, expected_serial],
                ..Default::default()
            },
        };
        let matches = {
            let mut host = explored_endpoint(host_report);
            host.report.systems[0].pcie_devices[0].part_number =
                Some("900-9D3B4-00EN-EA0".to_string());
            host.report.systems[0].pcie_devices[1].part_number = None;
            host.report.chassis.clear();
            let mut duplicate = host.report.systems[0].pcie_devices[0].clone();
            duplicate.part_number = None;
            host.report.systems[0].pcie_devices.insert(0, duplicate);
            HostDpuMatches::new(
                &host,
                &endpoints,
                Some(&expected_machine),
                HostDpuPolicy::Manage,
            )
        };

        assert_eq!(matches.expected_total, 3);
        assert_eq!(matches.matched_dpus.len(), 3);
        assert_eq!(
            matches.matched_dpus[0].target_operating_mode,
            Some(BlueFieldOperatingMode::Nic),
        );
        assert_eq!(
            matches.matched_dpus[1].target_operating_mode,
            Some(BlueFieldOperatingMode::Dpu),
        );
        assert_eq!(
            matches.matched_dpus[2].target_operating_mode,
            Some(BlueFieldOperatingMode::Dpu),
        );
    }

    #[test]
    fn chassis_matches_keep_part_numbers_and_unmatched_pcie_inventory() {
        let endpoints = HashMap::from([(
            "known-dpu".to_string(),
            bf2_dpu(Some(BlueFieldOperatingMode::Dpu)),
        )]);
        let matches = {
            let host_report: EndpointExplorationReport = ManagedHostConfig::default()
                .with_dpus(vec![DpuConfig {
                    serial: "unknown-dpu".to_string(),
                    ..DpuConfig::default()
                }])
                .into();
            let mut host = explored_endpoint(host_report);
            host.report.chassis = vec![
                Chassis {
                    network_adapters: vec![
                        NetworkAdapter {
                            serial_number: Some(" known-dpu ".to_string()),
                            ..Default::default()
                        },
                        NetworkAdapter {
                            serial_number: Some("known-dpu".to_string()),
                            ..Default::default()
                        },
                        NetworkAdapter {
                            part_number: Some("900-9D3B6-00CV-AA0".to_string()),
                            serial_number: Some("unknown-dpu".to_string()),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                Chassis {
                    part_number: Some("900-9D3B4-00EN-EA0".to_string()),
                    serial_number: Some("known-dpu".to_string()),
                    ..Default::default()
                },
            ];
            HostDpuMatches::new(&host, &endpoints, None, HostDpuPolicy::Manage)
        };

        assert_eq!(matches.expected_total, 2);
        assert_eq!(matches.matched_dpus.len(), 1);
        assert_eq!(
            matches.matched_dpus[0].target_operating_mode,
            Some(BlueFieldOperatingMode::Nic),
        );
        assert!(matches!(
            matches.matched_dpus[0].source,
            DpuMatchSource::Inventory,
        ));
    }

    #[test]
    fn combines_all_matching_sources() {
        const BLUEFIELD: &str = "900-9D3B6-00SV-AA0";
        let endpoints = HashMap::from([
            (
                "pcie-dpu".to_string(),
                bf2_dpu(Some(BlueFieldOperatingMode::Dpu)),
            ),
            (
                "chassis-dpu".to_string(),
                bf2_dpu(Some(BlueFieldOperatingMode::Dpu)),
            ),
            (
                "expected-only-dpu".to_string(),
                bf2_dpu(Some(BlueFieldOperatingMode::Dpu)),
            ),
        ]);
        let expected_machine = ExpectedMachine {
            id: None,
            bmc_mac_address: "00:11:22:33:44:55".parse().unwrap(),
            data: ExpectedMachineData {
                fallback_dpu_serial_numbers: vec![
                    "pcie-dpu".to_string(),
                    "expected-only-dpu".to_string(),
                ],
                ..Default::default()
            },
        };
        check_values(
            [
                Check {
                    scenario: "PCIe, chassis, and an expected-only DPU are matched together",
                    input: "chassis-dpu",
                    expect: (3, 3),
                },
                Check {
                    scenario: "an expected-only DPU fills an unmatched reported slot",
                    input: "undiscovered-dpu",
                    expect: (2, 2),
                },
                Check {
                    scenario: "copies of a PCIe serial in chassis and expected serials count once",
                    input: " pcie-dpu ",
                    expect: (2, 2),
                },
            ],
            |chassis_serial| {
                let host_report: EndpointExplorationReport = ManagedHostConfig::default()
                    .with_dpus(vec![DpuConfig {
                        serial: "pcie-dpu".to_string(),
                        ..DpuConfig::default()
                    }])
                    .into();
                let mut host = explored_endpoint(host_report);
                host.report.chassis = vec![Chassis {
                    part_number: Some(BLUEFIELD.to_string()),
                    serial_number: Some(chassis_serial.to_string()),
                    ..Default::default()
                }];
                let matches = HostDpuMatches::new(
                    &host,
                    &endpoints,
                    Some(&expected_machine),
                    HostDpuPolicy::Manage,
                );
                (matches.expected_total, matches.matched_dpus.len())
            },
        );
    }

    #[test]
    fn expected_serials_are_merged_and_normalized() {
        let endpoints = HashMap::from([(
            "known-dpu".to_string(),
            bf2_dpu(Some(BlueFieldOperatingMode::Dpu)),
        )]);
        let expected_machine = ExpectedMachine {
            id: None,
            bmc_mac_address: "00:11:22:33:44:55".parse().unwrap(),
            data: ExpectedMachineData {
                fallback_dpu_serial_numbers: vec![
                    " known-dpu ".to_string(),
                    "known-dpu".to_string(),
                    "  ".to_string(),
                    "undiscovered-expected-dpu".to_string(),
                ],
                ..Default::default()
            },
        };
        const BLUEFIELD: &str = "900-9D3B6-00SV-AA0";
        check_values(
            [
                Check {
                    scenario: "no DPU inventory",
                    input: vec![],
                    expect: (2, 1),
                },
                Check {
                    scenario: "reported BlueField matched by serial",
                    input: vec![(Some(BLUEFIELD), Some("known-dpu"))],
                    expect: (2, 1),
                },
                Check {
                    scenario: "serial match without a part number",
                    input: vec![(None, Some(" known-dpu "))],
                    expect: (2, 1),
                },
                Check {
                    scenario: "a duplicate without a part number is matched once",
                    input: vec![
                        (Some(BLUEFIELD), Some("known-dpu")),
                        (None, Some(" known-dpu ")),
                    ],
                    expect: (2, 1),
                },
                Check {
                    scenario: "unmatched device without a BlueField part number",
                    input: vec![(None, Some("other-device"))],
                    expect: (2, 1),
                },
                Check {
                    scenario: "expected serial identifies an undiscovered device without a part number",
                    input: vec![(None, Some("undiscovered-expected-dpu"))],
                    expect: (2, 1),
                },
                Check {
                    scenario: "undiscovered BlueField listed in expected serials counts once",
                    input: vec![(Some(BLUEFIELD), Some("undiscovered-expected-dpu"))],
                    expect: (2, 1),
                },
                Check {
                    scenario: "reported BlueField has no serial",
                    input: vec![(Some(BLUEFIELD), None)],
                    expect: (2, 1),
                },
                Check {
                    scenario: "an undiscovered configured serial can fill an unmatched reported slot",
                    input: vec![
                        (None, Some("known-dpu")),
                        (Some(BLUEFIELD), Some("unknown-dpu")),
                    ],
                    expect: (2, 1),
                },
            ],
            |devices| {
                let mut host = explored_endpoint(ManagedHostConfig::zero_dpu().into());
                host.report.chassis.clear();
                host.report.systems[0].pcie_devices = devices
                    .into_iter()
                    .map(
                        |(part_number, serial_number)| model::site_explorer::PCIeDevice {
                            part_number: part_number.map(str::to_string),
                            serial_number: serial_number.map(str::to_string),
                            description: None,
                            firmware_version: None,
                            gpu_vendor: None,
                            id: None,
                            manufacturer: None,
                            name: None,
                            status: None,
                        },
                    )
                    .collect();
                let matches = HostDpuMatches::new(
                    &host,
                    &endpoints,
                    Some(&expected_machine),
                    HostDpuPolicy::Manage,
                );
                (matches.expected_total, matches.matched_dpus.len())
            },
        );
    }

    #[test]
    fn fallback_serials_fill_unmatched_reported_slots() {
        let endpoints = HashMap::from([
            (
                "inventory-dpu".to_string(),
                bf2_dpu(Some(BlueFieldOperatingMode::Dpu)),
            ),
            (
                "fallback-dpu".to_string(),
                bf2_dpu(Some(BlueFieldOperatingMode::Dpu)),
            ),
        ]);
        check_values(
            [
                Check {
                    scenario: "fallback replaces a serial that differs from the DPU BMC serial",
                    input: (vec![Some("wrong-serial")], vec!["fallback-dpu"]),
                    expect: (1, 1),
                },
                Check {
                    scenario: "fallback replaces a missing reported serial",
                    input: (vec![None], vec!["fallback-dpu"]),
                    expect: (1, 1),
                },
                Check {
                    scenario: "fallback replaces a blank reported serial",
                    input: (vec![Some("  ")], vec!["fallback-dpu"]),
                    expect: (1, 1),
                },
                Check {
                    scenario: "serial-less copies do not add DPU slots alongside fallback",
                    input: (vec![None, None, Some("  ")], vec!["fallback-dpu"]),
                    expect: (1, 1),
                },
                Check {
                    scenario: "serial-less records do not block a matched inventory DPU",
                    input: (vec![Some("inventory-dpu"), None, None], vec![]),
                    expect: (1, 1),
                },
                Check {
                    scenario: "one fallback cannot fill two unmatched reported slots",
                    input: (
                        vec![Some("wrong-serial"), Some("another-wrong-serial")],
                        vec!["fallback-dpu"],
                    ),
                    expect: (2, 1),
                },
                Check {
                    scenario: "fallback matching an inventory DPU cannot fill another slot",
                    input: (
                        vec![Some("inventory-dpu"), Some("wrong-serial")],
                        vec!["inventory-dpu"],
                    ),
                    expect: (2, 1),
                },
            ],
            |(reported_serials, fallback_serials)| {
                let mut host = explored_endpoint(ManagedHostConfig::zero_dpu().into());
                host.report.chassis.clear();
                host.report.systems[0].pcie_devices = reported_serials
                    .into_iter()
                    .map(|serial_number| model::site_explorer::PCIeDevice {
                        part_number: Some("900-9D3B6-00SV-AA0".to_string()),
                        serial_number: serial_number.map(str::to_string),
                        description: None,
                        firmware_version: None,
                        gpu_vendor: None,
                        id: None,
                        manufacturer: None,
                        name: None,
                        status: None,
                    })
                    .collect();
                let expected_machine = ExpectedMachine {
                    id: None,
                    bmc_mac_address: "00:11:22:33:44:55".parse().unwrap(),
                    data: ExpectedMachineData {
                        fallback_dpu_serial_numbers: fallback_serials
                            .into_iter()
                            .map(str::to_string)
                            .collect(),
                        ..Default::default()
                    },
                };
                let matches = HostDpuMatches::new(
                    &host,
                    &endpoints,
                    Some(&expected_machine),
                    HostDpuPolicy::Manage,
                );
                (matches.expected_total, matches.matched_dpus.len())
            },
        );
    }
}
