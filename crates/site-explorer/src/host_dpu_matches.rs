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

use model::expected_machine::{ExpectedMachine, HostDpuPolicy};
use model::site_explorer::{BlueFieldOperatingMode, ExploredEndpoint};

use crate::endpoint_exploration_report_ext::{EndpointExplorationReportExt, HostDpuCandidate};
use crate::errors::SiteExplorerError;

/// DPU inventory counts and host/BMC matches collected before mode configuration.
pub(super) struct HostDpuMatches<'a> {
    pub(super) inventory: DpuInventoryState,
    pub(super) inventory_matches: Vec<MatchedDpu<'a>>,
    pub(super) fallback_matches: Vec<MatchedDpu<'a>>,
    /// Number of serials when fallback is selected, including undiscovered DPUs.
    pub(super) expected_fallback_count: usize,
}

impl<'a> HostDpuMatches<'a> {
    /// Matches host serials to discovered DPU BMCs and resolves their target modes.
    pub(super) fn new(
        ep: &ExploredEndpoint,
        dpu_sn_to_endpoint: &'a HashMap<String, ExploredEndpoint>,
        expected_machine: Option<&'a ExpectedMachine>,
        host_dpu_policy: HostDpuPolicy,
    ) -> Self {
        let match_candidate = |candidate: HostDpuCandidate<'_>, source| {
            let endpoint = candidate
                .serial_number()
                .and_then(|serial| dpu_sn_to_endpoint.get(serial))?;
            Some(MatchedDpu {
                endpoint,
                target_operating_mode: host_dpu_policy.target_operating_mode(candidate.part_number),
                source,
            })
        };

        let pcie_matches = ep
            .report
            .pcie_devices_dpu_candidates()
            .into_iter()
            .filter_map(|candidate| match_candidate(candidate, DpuMatchSource::Pcie))
            .collect::<Vec<_>>();

        // A DPU can show up as a chassis instead of a PCIe device on some
        // BMCs; fall back only if PCIe matched no discovered DPU BMC,
        // independently of the matched DPUs' modes or configuration results.
        let (inventory_matches, reported_total) = if pcie_matches.is_empty() {
            let inventory_matches = ep
                .report
                .chassis_devices_dpu_candidates()
                .into_iter()
                .filter_map(|candidate| match_candidate(candidate, DpuMatchSource::Chassis))
                .collect();
            let reported_total = ep.report.pcie_and_chassis_devices_dpu_count(|serial| {
                dpu_sn_to_endpoint.contains_key(serial)
            });
            (inventory_matches, reported_total)
        } else {
            let reported_total = ep
                .report
                .pcie_devices_dpu_count(|serial| dpu_sn_to_endpoint.contains_key(serial));
            (pcie_matches, reported_total)
        };

        let dpu_inventory = DpuInventoryState {
            reported_total,
            has_unmatched_reported_dpu: reported_total > inventory_matches.len(),
        };
        let needs_expected_serial_fallback =
            inventory_matches.is_empty() || dpu_inventory.has_unmatched_reported_dpu;
        // Expected serials resolve incomplete matching. Once every
        // reported DPU is matched, mode changes and configuration errors
        // must wait for the next iteration.
        let mut fallback_matches = Vec::new();
        let expected_fallback_count =
            if needs_expected_serial_fallback && let Some(expected_machine) = expected_machine {
                for dpu_sn in &expected_machine.data.fallback_dpu_serial_numbers {
                    if let Some(dpu_ep) = dpu_sn_to_endpoint.get(dpu_sn.as_str()) {
                        // Fallback matching has only the expected serial and the
                        // discovered DPU BMC report; use the DPU's own part number
                        // to resolve the target mode.
                        fallback_matches.push(MatchedDpu {
                            endpoint: dpu_ep,
                            target_operating_mode: host_dpu_policy
                                .target_operating_mode(dpu_ep.report.dpu_part_number()),
                            source: DpuMatchSource::ExpectedSerial(dpu_sn),
                        });
                    }
                }
                expected_machine.data.fallback_dpu_serial_numbers.len()
            } else {
                0
            };

        Self {
            inventory: dpu_inventory,
            inventory_matches,
            fallback_matches,
            expected_fallback_count,
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
    /// A PCIe device in the host report.
    Pcie,
    /// A chassis or network adapter in the host report.
    Chassis,
    /// An ExpectedMachine fallback serial, retained for failure diagnostics.
    ExpectedSerial(&'a str),
}

impl MatchedDpu<'_> {
    pub(super) fn warn_mode_check_failed(&self, err: &SiteExplorerError) {
        let dpu_ep = self.endpoint;
        match self.source {
            DpuMatchSource::Pcie | DpuMatchSource::Chassis => {
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
                    "failed to check fallback-matched DPU mode; skipping this device this pass",
                );
            }
        }
    }
}

/// Host inventory counts and matching completeness, independent of DPU configuration.
#[derive(Debug)]
pub(super) struct DpuInventoryState {
    /// Host inventory DPUs identified by part number or a discovered BMC serial.
    pub(super) reported_total: usize,
    /// At least one reported BlueField has no discovered DPU BMC match.
    pub(super) has_unmatched_reported_dpu: bool,
}

#[cfg(test)]
pub(super) fn matched_pcie_dpu(endpoint: &ExploredEndpoint) -> MatchedDpu<'_> {
    MatchedDpu {
        endpoint,
        target_operating_mode: None,
        source: DpuMatchSource::Pcie,
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
        let host_report: EndpointExplorationReport = ManagedHostConfig::default()
            .with_dpus(vec![dpu.clone(), DpuConfig::default()])
            .into();
        let endpoints = HashMap::from([(serial.clone(), explored_endpoint(dpu.into()))]);
        let expected_machine = ExpectedMachine {
            id: None,
            bmc_mac_address: "00:11:22:33:44:55".parse().unwrap(),
            data: ExpectedMachineData {
                fallback_dpu_serial_numbers: vec![serial],
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

        assert_eq!(matches.inventory_matches.len(), 1);
        assert_eq!(
            matches.inventory_matches[0].target_operating_mode,
            Some(BlueFieldOperatingMode::Nic),
        );
        assert_eq!(
            matches.fallback_matches[0].target_operating_mode,
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

        assert_eq!(matches.inventory.reported_total, 2);
        assert!(matches.inventory.has_unmatched_reported_dpu);
        assert_eq!(matches.inventory_matches.len(), 1);
        assert_eq!(
            matches.inventory_matches[0].target_operating_mode,
            Some(BlueFieldOperatingMode::Nic),
        );
        assert!(matches!(
            matches.inventory_matches[0].source,
            DpuMatchSource::Chassis,
        ));
    }

    #[test]
    fn expected_serial_fallback_depends_on_matching() {
        let endpoints = HashMap::from([(
            "known-dpu".to_string(),
            bf2_dpu(Some(BlueFieldOperatingMode::Dpu)),
        )]);
        let expected_machine = ExpectedMachine {
            id: None,
            bmc_mac_address: "00:11:22:33:44:55".parse().unwrap(),
            data: ExpectedMachineData {
                fallback_dpu_serial_numbers: vec!["known-dpu".to_string()],
                ..Default::default()
            },
        };
        const BLUEFIELD: &str = "900-9D3B6-00SV-AA0";
        check_values(
            [
                Check {
                    scenario: "no DPU inventory",
                    input: vec![],
                    expect: (0, true),
                },
                Check {
                    scenario: "reported BlueField matched by serial",
                    input: vec![(Some(BLUEFIELD), Some("known-dpu"))],
                    expect: (1, false),
                },
                Check {
                    scenario: "serial match without a part number",
                    input: vec![(None, Some(" known-dpu "))],
                    expect: (1, false),
                },
                Check {
                    scenario: "a duplicate without a part number is matched once",
                    input: vec![
                        (Some(BLUEFIELD), Some("known-dpu")),
                        (None, Some(" known-dpu ")),
                    ],
                    expect: (1, false),
                },
                Check {
                    scenario: "unmatched device without a BlueField part number",
                    input: vec![(None, Some("other-device"))],
                    expect: (0, true),
                },
                Check {
                    scenario: "reported BlueField has no serial",
                    input: vec![(Some(BLUEFIELD), None)],
                    expect: (1, true),
                },
                Check {
                    scenario: "another serial match cannot hide an undiscovered BlueField",
                    input: vec![
                        (None, Some("known-dpu")),
                        (Some(BLUEFIELD), Some("unknown-dpu")),
                    ],
                    expect: (2, true),
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
                (
                    matches.inventory.reported_total,
                    !matches.fallback_matches.is_empty(),
                )
            },
        );
    }
}
