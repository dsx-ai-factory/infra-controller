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

use carbide_hw_identification::host_dpu::{
    DpuProduct, HostDpuRule, HostDpuRules, InventorySource, ReportedPartNumber,
};
use itertools::Itertools;
use model::expected_machine::{ExpectedMachine, HostDpuPolicy};
use model::site_explorer::{BlueFieldOperatingMode, ExploredEndpoint};

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
    /// Use the DPU BMC part number when the selected inventory record lacks one,
    /// or only an unrecognized chassis or the expected machine supplies the serial.
    /// Expected serials are trimmed and deduplicated; blank entries are ignored.
    /// They can replace unmatched inventory serials without adding another slot.
    pub(super) fn new(
        ep: &ExploredEndpoint,
        dpu_sn_to_endpoint: &HashMap<&str, &'a ExploredEndpoint>,
        expected_machine: Option<&'a ExpectedMachine>,
        host_dpu_policy: HostDpuPolicy,
        rules: &'a HostDpuRules,
    ) -> Self {
        let inventory = rules.serials_with_part_numbers(&ep.report);
        let configured_serials = expected_machine
            .into_iter()
            .flat_map(|machine| &machine.data.fallback_dpu_serial_numbers)
            .filter_map(|serial| ConfiguredDpuSerial::parse(serial))
            .unique()
            .collect::<Vec<_>>();

        // Count each host-reported DPU serial once, including cards still awaiting BMC discovery.
        let reported_total = inventory
            .iter()
            .filter(|(serial, part_number)| {
                part_number.is_some_and(|part| part.rule.is_some())
                    || dpu_sn_to_endpoint.contains_key(serial)
                    || configured_serials.contains(&ConfiguredDpuSerial(serial))
            })
            .count();

        let inventory_matches = inventory.iter().filter_map(|&(serial, part_number)| {
            let endpoint = *dpu_sn_to_endpoint.get(serial)?;
            Some(MatchedDpu {
                endpoint,
                target_operating_mode: target_mode(host_dpu_policy, part_number, endpoint, rules),
                source: DpuMatchSource::Inventory,
                rule: part_number.and_then(|part| part.rule),
            })
        });
        let fallback_matches = configured_serials
            .iter()
            .copied()
            .filter(|serial| !inventory.iter().any(|&(reported, _)| reported == serial.0))
            .filter_map(|serial| {
                MatchedDpu::from_configured(serial, dpu_sn_to_endpoint, host_dpu_policy, rules)
            });
        let matched_dpus = inventory_matches
            .chain(fallback_matches)
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

/// An inventory part number eligible for mode selection takes precedence even when unknown.
fn target_mode(
    policy: HostDpuPolicy,
    host_part_number: Option<ReportedPartNumber<'_, '_>>,
    endpoint: &ExploredEndpoint,
    rules: &HostDpuRules,
) -> Option<BlueFieldOperatingMode> {
    let rule = match host_part_number {
        Some(part) => part.rule,
        None => endpoint
            .report
            .dpu_part_number()
            .and_then(|part| rules.matching_rule(InventorySource::DpuProductChassis, part)),
    };
    policy.resolve_operating_mode(match rule.map(|rule| rule.product) {
        Some(DpuProduct::BlueField3Dpu) => Some(BlueFieldOperatingMode::Dpu),
        Some(DpuProduct::BlueField3SuperNic | DpuProduct::BlueField3SuperNic9D3B4) => {
            Some(BlueFieldOperatingMode::Nic)
        }
        _ => None,
    })
}

/// A trimmed, nonempty DPU serial declared in ExpectedMachine configuration.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct ConfiguredDpuSerial<'a>(&'a str);

impl<'a> ConfiguredDpuSerial<'a> {
    fn parse(serial: &'a str) -> Option<Self> {
        let serial = serial.trim();
        (!serial.is_empty()).then_some(Self(serial))
    }

    fn find_explored_dpu<'e>(
        self,
        endpoints: &HashMap<&str, &'e ExploredEndpoint>,
    ) -> Option<&'e ExploredEndpoint> {
        endpoints.get(self.0).copied()
    }
}

/// A host/DPU association established by serial, independent of mode readiness.
pub(super) struct MatchedDpu<'a> {
    /// Already discovered DPU BMC endpoint.
    pub(super) endpoint: &'a ExploredEndpoint,
    /// Target resolved from the host policy and this match's source part number.
    pub(super) target_operating_mode: Option<BlueFieldOperatingMode>,
    /// Host inventory rule that identified the card, when pairing had part-number evidence.
    pub(super) rule: Option<&'a HostDpuRule>,
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
    fn from_configured(
        serial: ConfiguredDpuSerial<'a>,
        endpoints: &HashMap<&str, &'a ExploredEndpoint>,
        policy: HostDpuPolicy,
        rules: &HostDpuRules,
    ) -> Option<Self> {
        let endpoint = serial.find_explored_dpu(endpoints)?;

        Some(Self {
            endpoint,
            target_operating_mode: target_mode(policy, None, endpoint, rules),
            source: DpuMatchSource::ExpectedSerial(serial),
            rule: None,
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
                    host_dpu_rule_id = self.rule.map(|rule| rule.id.as_ref()),
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
        rule: None,
    }
}

#[cfg(test)]
mod tests {
    use carbide_hw_identification::hw::nvidia_bluefield_3::{
        DPU_9D3B6_00CV_AA0, SUPERNIC_9D3B4_00EN_EA0,
    };
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
        let mut serial_only_endpoint = explored_endpoint(serial_only_dpu.into());
        serial_only_endpoint
            .report
            .chassis
            .iter_mut()
            .find(|c| c.id == "Card1")
            .unwrap()
            .part_number = Some("OEM-NIC".into());
        let rules: HostDpuRules = serde_json::from_value(serde_json::json!([{
            "id": "site-peer-nic",
            "sources": ["dpu_product_chassis"],
            "part_numbers": [{"exact": "OEM-NIC"}],
            "product": "bluefield3_supernic"
        }]))
        .unwrap();
        let endpoint_rows = [
            (serial.clone(), explored_endpoint(dpu.into())),
            (serial_only, serial_only_endpoint),
            (
                expected_serial.clone(),
                explored_endpoint(expected_dpu.into()),
            ),
        ];
        let endpoints = endpoint_rows
            .iter()
            .map(|(serial, endpoint)| (serial.as_str(), endpoint))
            .collect();
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
                Some(SUPERNIC_9D3B4_00EN_EA0.to_owned());
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
                &rules,
            )
        };

        assert_eq!(matches.expected_total, 3);
        assert_eq!(matches.matched_dpus.len(), 3);
        assert_eq!(
            matches.matched_dpus[0].rule.map(|rule| rule.id.as_ref()),
            Some("nvidia-bluefield3-supernic-9d3b4")
        );
        assert_eq!(matches.matched_dpus[1].rule, None);
        assert_eq!(matches.matched_dpus[2].rule, None);
        assert_eq!(
            matches.matched_dpus[0].target_operating_mode,
            Some(BlueFieldOperatingMode::Nic),
        );
        assert_eq!(
            matches.matched_dpus[1].target_operating_mode,
            Some(BlueFieldOperatingMode::Nic),
        );
        assert_eq!(
            matches.matched_dpus[2].target_operating_mode,
            Some(BlueFieldOperatingMode::Dpu),
        );
    }

    #[test]
    fn first_unrecognized_record_controls_bmc_mode_fallback() {
        let dpu = DpuConfig {
            nic_mode: Some(BlueFieldOperatingMode::Nic),
            ..DpuConfig::default()
        };
        let endpoint = explored_endpoint(dpu.clone().into());
        let endpoints = HashMap::from([(dpu.serial.as_str(), &endpoint)]);
        let rules = HostDpuRules::default();

        check_values(
            [
                Check {
                    scenario: "missing then unknown uses the BF3 DPU BMC part number",
                    input: [None, Some("UNKNOWN-OEM")],
                    expect: Some(BlueFieldOperatingMode::Dpu),
                },
                Check {
                    scenario: "unknown then missing preserves NIC mode",
                    input: [Some("UNKNOWN-OEM"), None],
                    expect: None,
                },
            ],
            |parts| {
                let report = ManagedHostConfig::default()
                    .with_dpus(vec![dpu.clone(), dpu.clone()])
                    .into();
                let mut host = explored_endpoint(report);
                host.report.chassis.clear();
                for (device, part) in host.report.systems[0].pcie_devices.iter_mut().zip(parts) {
                    device.part_number = part.map(str::to_owned);
                }
                let matches =
                    HostDpuMatches::new(&host, &endpoints, None, HostDpuPolicy::Manage, &rules);
                assert_eq!(matches.expected_total, 1);
                let [card] = matches.matched_dpus.as_slice() else {
                    panic!("expected one matched DPU");
                };
                card.target_operating_mode
            },
        );
    }

    #[test]
    fn unknown_chassis_keeps_configured_bmc_mode_fallback() {
        let dpu = DpuConfig {
            nic_mode: Some(BlueFieldOperatingMode::Nic),
            ..DpuConfig::default()
        };
        let endpoint = explored_endpoint(dpu.clone().into());
        let endpoints = HashMap::from([(dpu.serial.as_str(), &endpoint)]);
        let expected = ExpectedMachine {
            id: None,
            bmc_mac_address: "00:11:22:33:44:55".parse().unwrap(),
            data: ExpectedMachineData {
                fallback_dpu_serial_numbers: vec![dpu.serial.clone()],
                ..Default::default()
            },
        };
        let rules = HostDpuRules::default();
        check_values(
            [
                Check {
                    scenario: "unknown chassis leaves configured serial mode selection to the BMC",
                    input: vec![],
                    expect: Some(BlueFieldOperatingMode::Dpu),
                },
                Check {
                    scenario: "unknown adapter part number still blocks BMC mode fallback",
                    input: vec![Chassis {
                        network_adapters: vec![NetworkAdapter {
                            part_number: Some("UNKNOWN-ADAPTER".into()),
                            serial_number: Some(dpu.serial.clone()),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    expect: None,
                },
            ],
            |additional_chassis| {
                let mut host = explored_endpoint(EndpointExplorationReport::default());
                host.report.chassis = vec![Chassis {
                    part_number: Some("UNKNOWN-CHASSIS".into()),
                    serial_number: Some(dpu.serial.clone()),
                    ..Default::default()
                }];
                host.report.chassis.extend(additional_chassis);
                let matches = HostDpuMatches::new(
                    &host,
                    &endpoints,
                    Some(&expected),
                    HostDpuPolicy::Manage,
                    &rules,
                );
                assert_eq!(matches.expected_total, 1);
                assert_eq!(matches.matched_dpus.len(), 1);
                matches.matched_dpus[0].target_operating_mode
            },
        );
    }

    #[test]
    fn site_rule_pairs_oem_chassis_and_sets_product_mode() {
        use figment::providers::{Format, Toml};
        let config: crate::config::SiteExplorerConfig = figment::Figment::from(Toml::string(
            r#"
            [[site_explorer.host_dpu_rules]]
            id = "site-oem-supernic"
            sources = ["chassis"]
            part_numbers = [{exact = "OEM-A"}, {prefix = "OEM-B-"}]
            product = "bluefield3_supernic"
        "#,
        ))
        .extract_inner("site_explorer")
        .unwrap();
        let endpoint = bf2_dpu(Some(BlueFieldOperatingMode::Dpu));
        let endpoints = HashMap::from([("card-1", &endpoint)]);
        let mut host = explored_endpoint(EndpointExplorationReport::default());
        host.report.chassis = vec![
            Chassis {
                part_number: Some(" oem-b-123 ".into()),
                serial_number: Some(" card-1 ".into()),
                ..Default::default()
            },
            Chassis {
                part_number: Some("SN37C16116".into()),
                serial_number: Some("pending".into()),
                ..Default::default()
            },
        ];
        let matches = HostDpuMatches::new(
            &host,
            &endpoints,
            None,
            HostDpuPolicy::Manage,
            &config.host_dpu_rules,
        );
        assert_eq!(matches.expected_total, 2);
        assert_eq!(matches.matched_dpus.len(), 1);
        assert_eq!(
            matches.matched_dpus[0].rule.map(|rule| rule.id.as_ref()),
            Some("site-oem-supernic")
        );
        assert_eq!(
            matches.matched_dpus[0].target_operating_mode,
            Some(BlueFieldOperatingMode::Nic)
        );
    }

    #[test]
    fn chassis_matches_keep_part_numbers_and_unmatched_pcie_inventory() {
        let rules = HostDpuRules::default();
        let endpoint_rows = [(
            "known-dpu".to_string(),
            bf2_dpu(Some(BlueFieldOperatingMode::Dpu)),
        )];
        let endpoints = endpoint_rows
            .iter()
            .map(|(serial, endpoint)| (serial.as_str(), endpoint))
            .collect();
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
                            part_number: Some(DPU_9D3B6_00CV_AA0.to_owned()),
                            serial_number: Some("unknown-dpu".to_string()),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                Chassis {
                    part_number: Some(SUPERNIC_9D3B4_00EN_EA0.to_owned()),
                    serial_number: Some("known-dpu".to_string()),
                    ..Default::default()
                },
            ];
            HostDpuMatches::new(&host, &endpoints, None, HostDpuPolicy::Manage, &rules)
        };

        assert_eq!(matches.expected_total, 2);
        assert_eq!(matches.matched_dpus.len(), 1);
        assert_eq!(
            matches.matched_dpus[0].rule.map(|rule| rule.id.as_ref()),
            Some("nvidia-bluefield3-supernic-9d3b4")
        );
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
    fn unknown_chassis_serials_pair_with_explored_dpus() {
        let rules = HostDpuRules::default();
        let endpoint = bf2_dpu(Some(BlueFieldOperatingMode::Dpu));
        let endpoints = HashMap::from([("chassis-dpu", &endpoint)]);
        check_values(
            [
                Check {
                    scenario: "unknown chassis without adapters",
                    input: vec![],
                    expect: (1, 1, 0, vec![(false, None)]),
                },
                Check {
                    scenario: "unknown chassis with an unmatched adapter",
                    input: vec![NetworkAdapter {
                        part_number: Some("UNKNOWN-ADAPTER".to_string()),
                        serial_number: Some("other-device".to_string()),
                        ..Default::default()
                    }],
                    expect: (1, 1, 0, vec![(false, None)]),
                },
                Check {
                    scenario: "unknown chassis and adapter report the same serial",
                    input: vec![NetworkAdapter {
                        serial_number: Some("chassis-dpu".to_string()),
                        ..Default::default()
                    }],
                    expect: (1, 1, 0, vec![(false, None)]),
                },
            ],
            |network_adapters| {
                let mut host = explored_endpoint(EndpointExplorationReport::default());
                host.report.chassis = vec![Chassis {
                    part_number: Some("UNKNOWN-CHASSIS".to_string()),
                    serial_number: Some(" chassis-dpu ".to_string()),
                    network_adapters,
                    ..Default::default()
                }];
                let matches =
                    HostDpuMatches::new(&host, &endpoints, None, HostDpuPolicy::Manage, &rules);
                (
                    matches.expected_total,
                    matches.matched_dpus.len(),
                    rules.identify_dpus(&host.report).len(),
                    matches
                        .matched_dpus
                        .iter()
                        .map(|dpu| (dpu.rule.is_some(), dpu.target_operating_mode))
                        .collect::<Vec<_>>(),
                )
            },
        );
    }

    #[test]
    fn combines_all_matching_sources() {
        let rules = HostDpuRules::default();
        const BLUEFIELD: &str = "900-9D3B6-00SV-AA0";
        let endpoint_rows = [
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
        ];
        let endpoints = endpoint_rows
            .iter()
            .map(|(serial, endpoint)| (serial.as_str(), endpoint))
            .collect();
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
                    &rules,
                );
                (matches.expected_total, matches.matched_dpus.len())
            },
        );
    }

    #[test]
    fn expected_serials_are_merged_and_normalized() {
        let rules = HostDpuRules::default();
        let endpoint_rows = [(
            "known-dpu".to_string(),
            bf2_dpu(Some(BlueFieldOperatingMode::Dpu)),
        )];
        let endpoints = endpoint_rows
            .iter()
            .map(|(serial, endpoint)| (serial.as_str(), endpoint))
            .collect();
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
                    &rules,
                );
                (matches.expected_total, matches.matched_dpus.len())
            },
        );
    }

    #[test]
    fn fallback_serials_fill_unmatched_reported_slots() {
        let rules = HostDpuRules::default();
        let endpoint_rows = [
            (
                "inventory-dpu".to_string(),
                bf2_dpu(Some(BlueFieldOperatingMode::Dpu)),
            ),
            (
                "fallback-dpu".to_string(),
                bf2_dpu(Some(BlueFieldOperatingMode::Dpu)),
            ),
        ];
        let endpoints = endpoint_rows
            .iter()
            .map(|(serial, endpoint)| (serial.as_str(), endpoint))
            .collect();
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
                    &rules,
                );
                (matches.expected_total, matches.matched_dpus.len())
            },
        );
    }
}
