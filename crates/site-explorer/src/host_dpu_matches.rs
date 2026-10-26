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

use model::expected_machine::{ExpectedMachine, HostDpuPolicy};
use model::site_explorer::{BlueFieldOperatingMode, ExploredEndpoint, is_bluefield_part_number};

use crate::errors::SiteExplorerError;
use crate::{duplicate_bluefield_serial, host_dpu_inventory_records};

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
        // Record the host's DPU devices against the discovered DPU BMCs.
        // A DPU can appear as a PCIe device under a system or as a chassis
        // network adapter (vendor-dependent), so we scan the PCIe inventory first
        // and fall back to chassis adapters if no DPU BMCs matched.
        // Keep inventory counts separate from configuration results so mode
        // checks cannot affect fallback selection.
        let mut dpu_inventory = DpuInventoryState::default();
        let mut seen_bluefield_serials = HashSet::new();
        let mut inventory_matches = Vec::new();
        for system in ep.report.systems.iter() {
            for pcie_device in system.pcie_devices.iter() {
                if let Some(serial_number) = duplicate_bluefield_serial(
                    pcie_device.part_number.as_deref(),
                    pcie_device.serial_number.as_deref(),
                    &mut seen_bluefield_serials,
                ) {
                    tracing::warn!(
                        host_bmc_ip_address = %ep.address,
                        %serial_number,
                        pcie_device_id = ?pcie_device.id,
                        "duplicate BlueField serial in host PCIe inventory; skipping duplicate record",
                    );
                    continue;
                }
                if let Some(matched_dpu) = dpu_inventory.record_reported_device(
                    pcie_device.part_number.as_deref(),
                    pcie_device.serial_number.as_deref(),
                    dpu_sn_to_endpoint,
                    DpuMatchSource::Pcie,
                    host_dpu_policy,
                ) {
                    inventory_matches.push(matched_dpu);
                }
            }
        }

        // A DPU can show up as a chassis instead of a PCIe device on some
        // BMCs; fall back only if PCIe matched no discovered DPU BMC,
        // independently of the matched DPUs' modes or configuration results.
        if dpu_inventory.matched_total == 0 {
            for chassis in ep.report.chassis.iter() {
                for record in host_dpu_inventory_records(chassis) {
                    if let Some(serial_number) = duplicate_bluefield_serial(
                        record.part_number,
                        record.serial_number,
                        &mut seen_bluefield_serials,
                    ) {
                        tracing::warn!(
                            host_bmc_ip_address = %ep.address,
                            %serial_number,
                            chassis_id = %chassis.id,
                            "duplicate BlueField serial in host chassis inventory; skipping duplicate record",
                        );
                        continue;
                    }
                    if let Some(matched_dpu) = dpu_inventory.record_reported_device(
                        record.part_number,
                        record.serial_number,
                        dpu_sn_to_endpoint,
                        DpuMatchSource::Chassis,
                        host_dpu_policy,
                    ) {
                        inventory_matches.push(matched_dpu);
                    }
                }
            }
        }

        let needs_expected_serial_fallback = dpu_inventory.needs_expected_serial_fallback();
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

/// Host inventory counts and serial matches, independent of DPU configuration.
#[derive(Debug, Default)]
pub(super) struct DpuInventoryState {
    /// Host inventory DPUs identified by part number or a discovered BMC serial.
    pub(super) reported_total: usize,
    /// Host inventory devices matched to discovered DPU BMCs before mode checks.
    pub(super) matched_total: usize,
    /// At least one reported BlueField has no discovered DPU BMC match.
    has_unmatched_reported_dpu: bool,
}

impl DpuInventoryState {
    /// Counts reported DPUs and resolves BMC matches with their target modes.
    fn record_reported_device<'a>(
        &mut self,
        part_number: Option<&str>,
        serial_number: Option<&str>,
        dpu_sn_to_endpoint: &'a HashMap<String, ExploredEndpoint>,
        source: DpuMatchSource<'a>,
        host_dpu_policy: HostDpuPolicy,
    ) -> Option<MatchedDpu<'a>> {
        // Count every DPU the host reports, independent of whether we've
        // discovered its BMC yet.
        let is_reported_dpu = part_number
            .map(str::trim)
            .is_some_and(is_bluefield_part_number);
        if is_reported_dpu {
            self.reported_total += 1;
        }

        // Only a device whose serial matches a *discovered* DPU BMC is ours to
        // classify; anything else is some other device, or a DPU whose BMC
        // we haven't explored yet.
        let Some(endpoint) = serial_number
            .map(str::trim)
            .and_then(|sn| dpu_sn_to_endpoint.get(sn))
        else {
            self.has_unmatched_reported_dpu |= is_reported_dpu;
            return None;
        };
        self.matched_total += 1;
        // A discovered DPU serial also identifies hardware without a BlueField part number.
        if !is_reported_dpu {
            self.reported_total += 1;
        }
        Some(MatchedDpu {
            endpoint,
            target_operating_mode: host_dpu_policy.target_operating_mode(part_number),
            source,
        })
    }

    pub(super) fn needs_expected_serial_fallback(&self) -> bool {
        self.matched_total == 0 || self.has_unmatched_reported_dpu
    }
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
    use model::site_explorer::EndpointExplorationReport;
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
            HostDpuMatches::new(
                &host,
                &endpoints,
                Some(&expected_machine),
                HostDpuPolicy::Manage,
            )
        };

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
    fn expected_serial_fallback_depends_on_matching() {
        let endpoints = HashMap::from([(
            "known-dpu".to_string(),
            bf2_dpu(Some(BlueFieldOperatingMode::Dpu)),
        )]);
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
                let mut inventory = DpuInventoryState::default();
                for (part_number, serial_number) in devices {
                    inventory.record_reported_device(
                        part_number,
                        serial_number,
                        &endpoints,
                        DpuMatchSource::Pcie,
                        HostDpuPolicy::Manage,
                    );
                }
                (
                    inventory.reported_total,
                    inventory.needs_expected_serial_fallback(),
                )
            },
        );
    }
}
