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

use model::expected_machine::ExpectedMachine;
use model::site_explorer::{ExploredEndpoint, is_bluefield_part_number};

use crate::{DpuMatchSource, MatchedDpu, duplicate_bluefield_serial, host_dpu_inventory_records};

/// DPU inventory counts and host/BMC matches collected before mode configuration.
pub(super) struct HostDpuMatches<'a> {
    pub(super) inventory: DpuInventoryState,
    pub(super) inventory_matches: Vec<MatchedDpu<'a>>,
    pub(super) fallback_matches: Vec<MatchedDpu<'a>>,
}

impl<'a> HostDpuMatches<'a> {
    /// Matches PCIe, chassis, and expected serials to discovered DPU BMCs.
    pub(super) fn new(
        ep: &'a ExploredEndpoint,
        dpu_sn_to_endpoint: &'a HashMap<String, ExploredEndpoint>,
        expected_machine: Option<&'a ExpectedMachine>,
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
        if needs_expected_serial_fallback && let Some(expected_machine) = expected_machine {
            for dpu_sn in &expected_machine.data.fallback_dpu_serial_numbers {
                if let Some(dpu_ep) = dpu_sn_to_endpoint.get(dpu_sn.as_str()) {
                    // Fallback matching has only the expected serial and the
                    // discovered DPU BMC report; pass the DPU's own part number
                    // for the legacy BF3 heuristic.
                    fallback_matches.push(MatchedDpu {
                        endpoint: dpu_ep,
                        part_number: dpu_ep.report.dpu_part_number(),
                        source: DpuMatchSource::ExpectedSerial(dpu_sn),
                    });
                }
            }
        }

        Self {
            inventory: dpu_inventory,
            inventory_matches,
            fallback_matches,
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
    /// Counts reported DPUs and tracks BMC matches independently of mode checks.
    fn record_reported_device<'a>(
        &mut self,
        part_number: Option<&'a str>,
        serial_number: Option<&str>,
        dpu_sn_to_endpoint: &'a HashMap<String, ExploredEndpoint>,
        source: DpuMatchSource<'a>,
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
            part_number,
            source,
        })
    }

    pub(super) fn needs_expected_serial_fallback(&self) -> bool {
        self.matched_total == 0 || self.has_unmatched_reported_dpu
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use model::site_explorer::BlueFieldOperatingMode;

    use super::*;
    use crate::test_support::bf2_dpu;

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
