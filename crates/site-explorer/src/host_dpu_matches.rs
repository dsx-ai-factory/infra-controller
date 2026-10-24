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

use crate::endpoint_exploration_report_ext::{EndpointExplorationReportExt, HostDpuCandidate};
use crate::errors::SiteExplorerError;

/// Expected DPU count and unique host/BMC matches collected before configuration.
pub(super) struct HostDpuMatches<'a> {
    /// Unique reported or configured DPUs, including those not yet discovered.
    /// BlueField records without usable serials count separately.
    pub(super) expected_total: usize,
    pub(super) matched_dpus: Vec<MatchedDpu<'a>>,
}

impl<'a> HostDpuMatches<'a> {
    /// Combines PCIe, chassis, and expected serials before resolving target modes.
    ///
    /// Host inventory determines the target mode for overlapping serials. Only
    /// expected serials absent from inventory use the DPU BMC's part number.
    /// Expected serials are trimmed and deduplicated; blank entries are ignored.
    pub(super) fn new(
        ep: &ExploredEndpoint,
        dpu_sn_to_endpoint: &'a HashMap<String, ExploredEndpoint>,
        expected_machine: Option<&'a ExpectedMachine>,
        host_dpu_policy: HostDpuPolicy,
    ) -> Self {
        let inventory_candidates = ep.report.dpu_candidates();
        let inventory_serials = inventory_candidates
            .iter()
            .filter_map(|candidate| candidate.serial_number())
            .collect::<HashSet<_>>();
        let expected_serials = expected_machine
            .into_iter()
            .flat_map(|machine| &machine.data.fallback_dpu_serial_numbers)
            .map(|serial| serial.trim())
            .filter(|serial| !serial.is_empty())
            .unique()
            .collect::<Vec<_>>();
        let additional_serials = expected_serials
            .iter()
            .copied()
            .filter(|serial| !inventory_serials.contains(serial))
            .collect::<Vec<_>>();
        let reported_total = ep.report.dpu_count(|serial| {
            dpu_sn_to_endpoint.contains_key(serial) || expected_serials.contains(&serial)
        });
        let expected_total = reported_total + additional_serials.len();

        let matched_dpus = inventory_candidates
            .into_iter()
            .map(|candidate| (candidate, DpuMatchSource::Inventory))
            .chain(additional_serials.into_iter().map(|serial| {
                let candidate = HostDpuCandidate {
                    serial_number: Some(serial),
                    part_number: dpu_sn_to_endpoint
                        .get(serial)
                        .and_then(|endpoint| endpoint.report.dpu_part_number()),
                };
                (candidate, DpuMatchSource::ExpectedSerial(serial))
            }))
            .filter_map(|(candidate, source)| {
                let endpoint = candidate
                    .serial_number()
                    .and_then(|serial| dpu_sn_to_endpoint.get(serial))?;
                Some(MatchedDpu {
                    endpoint,
                    target_operating_mode: host_dpu_policy
                        .target_operating_mode(candidate.part_number),
                    source,
                })
            })
            .collect();

        Self {
            expected_total,
            matched_dpus,
        }
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
    ExpectedSerial(&'a str),
}

impl MatchedDpu<'_> {
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
                    dpu_serial_number = %serial_number,
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
        let expected_dpu = DpuConfig::default();
        let expected_serial = expected_dpu.serial.clone();
        let host_report: EndpointExplorationReport = ManagedHostConfig::default()
            .with_dpus(vec![dpu.clone(), DpuConfig::default()])
            .into();
        let endpoints = HashMap::from([
            (serial.clone(), explored_endpoint(dpu.into())),
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
        assert_eq!(matches.matched_dpus.len(), 2);
        assert_eq!(
            matches.matched_dpus[0].target_operating_mode,
            Some(BlueFieldOperatingMode::Nic),
        );
        assert_eq!(
            matches.matched_dpus[1].target_operating_mode,
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
                    scenario: "an undiscovered reported DPU is still expected alongside configured serials",
                    input: "undiscovered-dpu",
                    expect: (3, 2),
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
                    expect: (3, 1),
                },
                Check {
                    scenario: "another serial match cannot hide an undiscovered BlueField",
                    input: vec![
                        (None, Some("known-dpu")),
                        (Some(BLUEFIELD), Some("unknown-dpu")),
                    ],
                    expect: (3, 1),
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
}
