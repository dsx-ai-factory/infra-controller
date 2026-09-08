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

use std::collections::HashSet;

use carbide_instrument::{DynamicLog, Event, LabelValue, LogAt};
use carbide_uuid::machine::{DpuMachineId, MachineId, MachineInterfaceId};
use mac_address::MacAddress;
use model::hardware_info::HardwareInfo;
use model::machine::Machine;
use model::machine_boot_interface::BootInterfaceSelectionSource;
use model::network_segment::NetworkSegmentType;
use model::pci::PciBdfOrderingKey;
use rpc::forge::BootInterfaceSelectionSource as RpcBootInterfaceSelectionSource;

/// Comparison recorded for each scout PCI candidate.
#[derive(Clone, Copy, Debug, Eq, LabelValue, PartialEq)]
enum ScoutPciComparison {
    /// The scout candidate matches the stored boot interface.
    MatchesStored,
    /// The scout candidate differs from the stored boot interface.
    DiffersFromStored,
    /// The report or stored selection is missing required information.
    MissingData,
    /// An identifier expected to be unique maps to multiple interfaces or BDF keys.
    IdentifierCollision,
}

/// The stored identity fields needed to match and describe one candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EligibleInterface {
    machine_interface_id: MachineInterfaceId,
    mac_address: MacAddress,
    dpu_machine_id: DpuMachineId,
}

/// One eligible interface paired with its parsed scout endpoint BDF.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Candidate {
    interface: EligibleInterface,
    ordering_key: PciBdfOrderingKey,
    pci_slot: String,
}

impl Candidate {
    /// Converts this candidate to the context attached to a comparison.
    fn subject(&self) -> ComparisonSubject {
        ComparisonSubject {
            machine_interface_id: self.interface.machine_interface_id,
            mac_address: self.interface.mac_address,
            pci_slot: Some(self.pci_slot.clone()),
        }
    }
}

/// Interface and slot details used to produce a comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ComparisonSubject {
    machine_interface_id: MachineInterfaceId,
    mac_address: MacAddress,
    pci_slot: Option<String>,
}

impl ComparisonSubject {
    /// Builds context for an interface before its PCI BDF is available.
    fn for_interface(interface: EligibleInterface) -> Self {
        Self {
            machine_interface_id: interface.machine_interface_id,
            mac_address: interface.mac_address,
            pci_slot: None,
        }
    }
}

/// A scout PCI comparison with the context needed to record it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::handlers) struct Comparison {
    comparison: ScoutPciComparison,
    subject: Option<ComparisonSubject>,
    desired_mac_address: Option<MacAddress>,
    selection_source: Option<BootInterfaceSelectionSource>,
    reason: &'static str,
}

impl Comparison {
    /// Captures a comparison and its stored machine context without emitting it.
    fn from_machine(
        machine: &Machine,
        comparison: ScoutPciComparison,
        subject: Option<ComparisonSubject>,
        reason: &'static str,
    ) -> Self {
        Self {
            comparison,
            subject,
            desired_mac_address: machine
                .config
                .desired_boot_interface
                .as_ref()
                .map(|target| target.value.mac_address()),
            selection_source: machine
                .config
                .boot_interface_selection
                .map(|selection| selection.source),
            reason,
        }
    }

    /// Returns the selected interface when the report identified one complete candidate.
    pub(in crate::handlers) fn candidate_interface_id(&self) -> Option<MachineInterfaceId> {
        let interface_id = self.subject.as_ref()?.machine_interface_id;
        matches!(
            self.comparison,
            ScoutPciComparison::MatchesStored | ScoutPciComparison::DiffersFromStored
        )
        .then_some(interface_id)
    }

    /// Emits the comparison metric and its structured log.
    pub(in crate::handlers) fn emit(self, machine_id: &MachineId) {
        let (interface_mac_address, pci_slot) = self.subject.map_or_else(
            || (None, None),
            |subject| (Some(subject.mac_address.to_string()), subject.pci_slot),
        );

        carbide_instrument::emit(ScoutPciCompared {
            result: self.comparison,
            machine_id: machine_id.to_string(),
            interface_mac_address,
            desired_mac_address: self
                .desired_mac_address
                .map(|mac_address| mac_address.to_string()),
            pci_slot,
            selection_source: self
                .selection_source
                .map(RpcBootInterfaceSelectionSource::from)
                .map(|selection_source| selection_source.as_str().to_owned()),
            reason: self.reason,
        });
    }
}

/// Metric and structured log for one recorded scout PCI comparison.
#[derive(Event)]
#[event(
    event_name = "scout_pci_evaluated",
    metric_name = "carbide_scout_pci_evaluations_total",
    component = "nico-api",
    log = dynamic,
    metric = counter,
    message = "Recorded scout PCI evaluation of the stored boot interface selection",
    describe = "Number of comparisons between scout PCI address ordering and stored boot interface selections, by result."
)]
struct ScoutPciCompared {
    #[label]
    result: ScoutPciComparison,
    #[context]
    machine_id: String,
    #[context]
    interface_mac_address: Option<String>,
    #[context]
    desired_mac_address: Option<String>,
    #[context]
    pci_slot: Option<String>,
    #[context]
    selection_source: Option<String>,
    #[context]
    reason: &'static str,
}

impl DynamicLog for ScoutPciCompared {
    /// Uses warning for comparisons that need attention and debug for matches.
    fn log_at(&self) -> LogAt {
        match self.result {
            ScoutPciComparison::MatchesStored => LogAt::Level(tracing::Level::DEBUG),
            ScoutPciComparison::DiffersFromStored
            | ScoutPciComparison::MissingData
            | ScoutPciComparison::IdentifierCollision => LogAt::Level(tracing::Level::WARN),
        }
    }
}

/// Compares endpoint PCI BDFs reported by scout with the stored boot interface.
pub(in crate::handlers) fn compare(
    hardware_info: &HardwareInfo,
    machine: &Machine,
) -> Option<Comparison> {
    let eligible_interfaces = eligible_interfaces(machine);
    if eligible_interfaces.len() < 2 {
        return None;
    }

    let desired_mac_address = machine
        .config
        .desired_boot_interface
        .as_ref()
        .map(|target| target.value.mac_address());
    if desired_mac_address.is_some_and(|desired_mac_address| {
        !eligible_interfaces
            .iter()
            .any(|interface| interface.mac_address == desired_mac_address)
            && machine.status.interfaces.iter().any(|interface| {
                interface.machine_id == Some(machine.id)
                    && interface.mac_address == desired_mac_address
            })
    }) {
        return None;
    }

    if let Some(interface) = first_duplicate_mac(&eligible_interfaces) {
        return Some(Comparison::from_machine(
            machine,
            ScoutPciComparison::IdentifierCollision,
            Some(ComparisonSubject::for_interface(interface)),
            "eligible interface MAC is not unique",
        ));
    }
    if let Some(interface) = first_duplicate_dpu(&eligible_interfaces) {
        return Some(Comparison::from_machine(
            machine,
            ScoutPciComparison::IdentifierCollision,
            Some(ComparisonSubject::for_interface(interface)),
            "eligible interface DPU ID is not unique",
        ));
    }

    let candidates = match candidates_from_report(hardware_info, machine, &eligible_interfaces) {
        Ok(candidates) => candidates,
        Err(comparison) => return Some(comparison),
    };
    let candidate = candidates
        .into_iter()
        .min_by(|left, right| left.ordering_key.cmp(&right.ordering_key))?;

    let Some(desired_mac_address) = desired_mac_address else {
        return Some(Comparison::from_machine(
            machine,
            ScoutPciComparison::MissingData,
            Some(candidate.subject()),
            "stored boot interface is missing",
        ));
    };

    if !eligible_interfaces
        .iter()
        .any(|interface| interface.mac_address == desired_mac_address)
    {
        return Some(Comparison::from_machine(
            machine,
            ScoutPciComparison::MissingData,
            Some(candidate.subject()),
            "stored boot interface is not present on this host",
        ));
    }

    Some(compare_candidate(machine, candidate, desired_mac_address))
}

/// Collects this host's Admin interfaces that are attached to DPU machines.
fn eligible_interfaces(machine: &Machine) -> Vec<EligibleInterface> {
    let mut interfaces = machine
        .status
        .interfaces
        .iter()
        .filter_map(|interface| {
            let dpu_machine_id = interface.attached_dpu_machine_id?;
            (interface.machine_id == Some(machine.id)
                && interface.network_segment_type == Some(NetworkSegmentType::Admin))
            .then_some(EligibleInterface {
                machine_interface_id: interface.id,
                mac_address: interface.mac_address,
                dpu_machine_id,
            })
        })
        .collect::<Vec<_>>();
    interfaces.sort_by_key(|interface| interface.machine_interface_id);
    interfaces
}

/// Returns the second interface carrying a repeated eligible MAC address.
fn first_duplicate_mac(interfaces: &[EligibleInterface]) -> Option<EligibleInterface> {
    let mut seen = HashSet::with_capacity(interfaces.len());
    interfaces
        .iter()
        .copied()
        .find(|interface| !seen.insert(interface.mac_address))
}

/// Returns the second interface carrying a repeated eligible DPU ID.
fn first_duplicate_dpu(interfaces: &[EligibleInterface]) -> Option<EligibleInterface> {
    let mut seen = HashSet::with_capacity(interfaces.len());
    interfaces
        .iter()
        .copied()
        .find(|interface| !seen.insert(interface.dpu_machine_id))
}

/// Matches every eligible MAC to one report row and one complete endpoint BDF.
fn candidates_from_report(
    hardware_info: &HardwareInfo,
    machine: &Machine,
    eligible_interfaces: &[EligibleInterface],
) -> Result<Vec<Candidate>, Comparison> {
    let mut candidates = Vec::with_capacity(eligible_interfaces.len());

    for interface in eligible_interfaces {
        let mut matches = hardware_info
            .network_interfaces
            .iter()
            .filter(|reported| reported.mac_address == interface.mac_address);
        let Some(reported) = matches.next() else {
            return Err(Comparison::from_machine(
                machine,
                ScoutPciComparison::MissingData,
                Some(ComparisonSubject::for_interface(*interface)),
                "eligible interface has no scout report row",
            ));
        };
        if matches.next().is_some() {
            return Err(Comparison::from_machine(
                machine,
                ScoutPciComparison::IdentifierCollision,
                Some(ComparisonSubject::for_interface(*interface)),
                "eligible interface has multiple scout report rows",
            ));
        }

        let Some(pci_slot) = reported
            .pci_properties
            .as_ref()
            .and_then(|properties| properties.slot.as_deref())
            .map(str::trim)
            .filter(|slot| !slot.is_empty())
        else {
            return Err(Comparison::from_machine(
                machine,
                ScoutPciComparison::MissingData,
                Some(ComparisonSubject::for_interface(*interface)),
                "eligible interface report has no PCI BDF",
            ));
        };
        let subject = ComparisonSubject {
            machine_interface_id: interface.machine_interface_id,
            mac_address: interface.mac_address,
            pci_slot: Some(pci_slot.to_string()),
        };
        let ordering_key = match PciBdfOrderingKey::from_bdf(pci_slot) {
            Ok(ordering_key) => ordering_key,
            Err(_) => {
                return Err(Comparison::from_machine(
                    machine,
                    ScoutPciComparison::MissingData,
                    Some(subject),
                    "eligible interface report has malformed PCI BDF",
                ));
            }
        };

        candidates.push(Candidate {
            interface: *interface,
            ordering_key,
            pci_slot: pci_slot.to_string(),
        });
    }

    let mut ordering_keys = HashSet::with_capacity(candidates.len());
    if let Some(candidate) = candidates
        .iter()
        .find(|candidate| !ordering_keys.insert(&candidate.ordering_key))
    {
        return Err(Comparison::from_machine(
            machine,
            ScoutPciComparison::IdentifierCollision,
            Some(candidate.subject()),
            "eligible interfaces share a parsed PCI BDF key",
        ));
    }

    Ok(candidates)
}

/// Compares the scout candidate with the stored desired boot interface MAC.
fn compare_candidate(
    machine: &Machine,
    candidate: Candidate,
    desired_mac_address: MacAddress,
) -> Comparison {
    if candidate.interface.mac_address == desired_mac_address {
        Comparison::from_machine(
            machine,
            ScoutPciComparison::MatchesStored,
            Some(candidate.subject()),
            "candidate matches the stored boot interface",
        )
    } else {
        Comparison::from_machine(
            machine,
            ScoutPciComparison::DiffersFromStored,
            Some(candidate.subject()),
            "candidate differs from the stored boot interface",
        )
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values, value_scenarios};
    use config_version::Versioned;
    use model::hardware_info::{NetworkInterface, PciDeviceProperties};
    use model::machine::MachineInterfaceSnapshot;
    use model::machine_boot_interface::MachineBootInterfaceTarget;
    use model::test_support::machine_snapshot::{
        config_version, dpu_machine_id, host_machine, host_machine_id,
    };

    use super::*;

    /// One variation from the complete, matching comparison fixture.
    #[derive(Clone, Copy)]
    enum ComparisonCase {
        NotEnoughInterfaces,
        BdfOrdering,
        ReversedRows,
        DiffersFromStored,
        DomainOrdering,
        NormalizedOrdering,
        MalformedSlot,
        MissingRow,
        MissingPci,
        MissingSlot,
        BlankSlot,
        DuplicateRow,
        DuplicateKey,
        DuplicateMac,
        DuplicateDpu,
        MissingDesired,
        UnknownDesired,
        IntegratedInterface,
        IntegratedDesired,
        IntegratedDesiredWithMissingSlot,
        ExtraReportRow,
    }

    /// Returns a deterministic MAC address for test interface identities.
    fn mac(index: u8) -> MacAddress {
        MacAddress::new([0x02, 0, 0, 0, 0, index])
    }

    /// Builds one Admin interface owned by the fixture host and attached to a DPU.
    fn eligible_interface(
        mac_address: MacAddress,
        dpu_machine_id: DpuMachineId,
    ) -> MachineInterfaceSnapshot {
        let mut interface = MachineInterfaceSnapshot::mock_with_mac(mac_address);
        interface.id =
            MachineInterfaceId::from(uuid::Uuid::from_u128(u128::from(mac_address.bytes()[5])));
        interface.machine_id = Some(host_machine_id());
        interface.attached_dpu_machine_id = Some(dpu_machine_id);
        interface.network_segment_type = Some(NetworkSegmentType::Admin);
        interface
    }

    /// Builds an Admin interface owned by the host and not attached to a DPU.
    fn integrated_interface() -> MachineInterfaceSnapshot {
        let mut interface = MachineInterfaceSnapshot::mock_with_mac(mac(9));
        interface.id = MachineInterfaceId::from(uuid::Uuid::from_u128(9));
        interface.machine_id = Some(host_machine_id());
        interface.network_segment_type = Some(NetworkSegmentType::Admin);
        interface.attached_dpu_machine_id = None;
        interface
    }

    /// Builds one scout network row with the supplied endpoint PCI BDF.
    fn reported_interface(mac_address: MacAddress, slot: Option<&str>) -> NetworkInterface {
        NetworkInterface {
            mac_address,
            pci_properties: Some(PciDeviceProperties {
                vendor: String::new(),
                device: String::new(),
                // DEVPATH is deliberately not required for endpoint-BDF ordering.
                path: String::new(),
                numa_node: 0,
                description: None,
                slot: slot.map(str::to_string),
            }),
        }
    }

    /// Builds a machine and returns its bounded scout PCI comparison.
    fn run_comparison(case: ComparisonCase) -> Option<ScoutPciComparison> {
        let mut machine = host_machine();
        machine.status.interfaces = vec![
            eligible_interface(mac(1), dpu_machine_id(0)),
            eligible_interface(mac(2), dpu_machine_id(1)),
        ];
        let mut reports = vec![
            reported_interface(mac(1), Some("0:2:0.0")),
            reported_interface(mac(2), Some("0:10:0.0")),
        ];
        let mut desired_mac_address = Some(mac(1));

        match case {
            ComparisonCase::NotEnoughInterfaces => machine.status.interfaces.truncate(1),
            ComparisonCase::BdfOrdering => {}
            ComparisonCase::ReversedRows => {
                machine.status.interfaces.reverse();
                reports.reverse();
            }
            ComparisonCase::DiffersFromStored => {
                reports = vec![
                    reported_interface(mac(1), Some("0:10:0.0")),
                    reported_interface(mac(2), Some("0:2:0.0")),
                ];
            }
            ComparisonCase::DomainOrdering => {
                reports = vec![
                    reported_interface(mac(1), Some("1:0:0.0")),
                    reported_interface(mac(2), Some("0:ff:0.0")),
                ];
                desired_mac_address = Some(mac(2));
            }
            ComparisonCase::NormalizedOrdering => {
                reports = vec![
                    reported_interface(mac(1), Some(" 0000:0A:00.0 ")),
                    reported_interface(mac(2), Some("0:b:0.0")),
                ];
            }
            ComparisonCase::MalformedSlot => {
                reports[0].pci_properties.as_mut().unwrap().slot = Some("Riser_Slot1".to_string());
            }
            ComparisonCase::MissingRow => {
                reports.remove(0);
            }
            ComparisonCase::MissingPci => reports[0].pci_properties = None,
            ComparisonCase::MissingSlot => {
                reports[0].pci_properties.as_mut().unwrap().slot = None;
            }
            ComparisonCase::BlankSlot => {
                reports[0].pci_properties.as_mut().unwrap().slot = Some(" \t".to_string());
            }
            ComparisonCase::DuplicateRow => reports.insert(0, reports[0].clone()),
            ComparisonCase::DuplicateKey => {
                reports[1].pci_properties.as_mut().unwrap().slot =
                    Some("0000:0002:00.00".to_string());
            }
            ComparisonCase::DuplicateMac => {
                machine.status.interfaces[1] = eligible_interface(mac(1), dpu_machine_id(1));
            }
            ComparisonCase::DuplicateDpu => {
                machine.status.interfaces[1] = eligible_interface(mac(2), dpu_machine_id(0));
            }
            ComparisonCase::MissingDesired => desired_mac_address = None,
            ComparisonCase::UnknownDesired => desired_mac_address = Some(mac(9)),
            ComparisonCase::IntegratedInterface => {
                machine.status.interfaces.push(integrated_interface());
                reports.push(reported_interface(mac(9), Some("0000:01:00.0")));
            }
            ComparisonCase::IntegratedDesired => {
                machine.status.interfaces.push(integrated_interface());
                reports.push(reported_interface(mac(9), Some("0000:01:00.0")));
                desired_mac_address = Some(mac(9));
            }
            ComparisonCase::IntegratedDesiredWithMissingSlot => {
                machine.status.interfaces.push(integrated_interface());
                reports[0].pci_properties.as_mut().unwrap().slot = None;
                desired_mac_address = Some(mac(9));
            }
            ComparisonCase::ExtraReportRow => {
                reports.push(reported_interface(mac(9), None));
            }
        }

        machine.config.desired_boot_interface = desired_mac_address.map(|address| {
            Versioned::new(
                MachineBootInterfaceTarget::MacOnly(address),
                config_version(20),
            )
        });

        let hardware_info = HardwareInfo {
            network_interfaces: reports,
            ..HardwareInfo::default()
        };
        compare(&hardware_info, &machine).map(|comparison| comparison.comparison)
    }

    /// Builds one compact comparison table row.
    macro_rules! comparison_case {
        ($scenario:literal, $input:ident, $expect:ident) => {
            Check {
                scenario: $scenario,
                input: ComparisonCase::$input,
                expect: Some(ScoutPciComparison::$expect),
            }
        };
        ($scenario:literal, $input:ident) => {
            Check {
                scenario: $scenario,
                input: ComparisonCase::$input,
                expect: None,
            }
        };
    }

    /// One table covers ordering, missing data, identifier collisions, and stored matching.
    #[test]
    fn comparison_requires_complete_unique_scout_bdf_mappings() {
        check_values(
            [
                comparison_case!(
                    "fewer than two eligible interfaces are ignored",
                    NotEnoughInterfaces
                ),
                comparison_case!(
                    "variable-width numeric BDF 2 sorts before 10 without DEVPATH",
                    BdfOrdering,
                    MatchesStored
                ),
                comparison_case!(
                    "stored and report row order do not affect selection",
                    ReversedRows,
                    MatchesStored
                ),
                comparison_case!(
                    "lower second BDF differs from the stored first interface",
                    DiffersFromStored,
                    DiffersFromStored
                ),
                comparison_case!("lower domain sorts first", DomainOrdering, MatchesStored),
                comparison_case!(
                    "BDF comparison ignores case, padding, and surrounding whitespace",
                    NormalizedOrdering,
                    MatchesStored
                ),
                comparison_case!(
                    "arbitrary PCI slot values are not valid BDFs",
                    MalformedSlot,
                    MissingData
                ),
                comparison_case!("report row is missing", MissingRow, MissingData),
                comparison_case!("PCI properties are missing", MissingPci, MissingData),
                comparison_case!("PCI BDF is missing", MissingSlot, MissingData),
                comparison_case!("blank PCI BDF is missing data", BlankSlot, MissingData),
                comparison_case!(
                    "duplicate report row causes an identifier collision",
                    DuplicateRow,
                    IdentifierCollision
                ),
                comparison_case!(
                    "numeric-equivalent BDFs cause an identifier collision",
                    DuplicateKey,
                    IdentifierCollision
                ),
                comparison_case!(
                    "duplicate eligible MAC causes an identifier collision",
                    DuplicateMac,
                    IdentifierCollision
                ),
                comparison_case!(
                    "duplicate eligible DPU identity causes an identifier collision",
                    DuplicateDpu,
                    IdentifierCollision
                ),
                comparison_case!("stored target is missing", MissingDesired, MissingData),
                comparison_case!(
                    "unknown stored target is missing data",
                    UnknownDesired,
                    MissingData
                ),
                comparison_case!(
                    "integrated interface is excluded from DPU ordering",
                    IntegratedInterface,
                    MatchesStored
                ),
                comparison_case!(
                    "integrated boot interface needs no DPU comparison",
                    IntegratedDesired
                ),
                comparison_case!(
                    "integrated boot interface is ignored before scout PCI BDFs are checked",
                    IntegratedDesiredWithMissingSlot
                ),
                comparison_case!(
                    "unrelated scout row is ignored",
                    ExtraReportRow,
                    MatchesStored
                ),
            ],
            run_comparison,
        );
    }

    /// Only comparisons with a complete candidate expose an interface for automatic selection.
    #[test]
    fn candidate_interface_requires_complete_comparison() {
        let interface_id = MachineInterfaceId::from(uuid::Uuid::from_u128(1));
        value_scenarios!(run = |comparison: ScoutPciComparison| {
            Comparison {
                comparison,
                subject: Some(ComparisonSubject {
                    machine_interface_id: interface_id,
                    mac_address: mac(1),
                    pci_slot: Some("0000:02:00.0".to_string()),
                }),
                desired_mac_address: Some(mac(1)),
                selection_source: Some(BootInterfaceSelectionSource::RedfishChassisId),
                reason: "test reason",
            }
            .candidate_interface_id()
        };
            "complete" {
                ScoutPciComparison::DiffersFromStored => Some(interface_id),
            }
            "not selectable" {
                ScoutPciComparison::MissingData => None,
                ScoutPciComparison::IdentifierCollision => None,
            }
        );
    }

    /// One table covers every comparison's metric label and log level.
    #[test]
    fn event_emits_every_comparison_with_candidate_context() {
        /// Expected observable values for one bounded comparison.
        struct EventCase {
            comparison: ScoutPciComparison,
            label: &'static str,
            level: tracing::Level,
        }

        let cases = [
            EventCase {
                comparison: ScoutPciComparison::MatchesStored,
                label: "matches_stored",
                level: tracing::Level::DEBUG,
            },
            EventCase {
                comparison: ScoutPciComparison::DiffersFromStored,
                label: "differs_from_stored",
                level: tracing::Level::WARN,
            },
            EventCase {
                comparison: ScoutPciComparison::MissingData,
                label: "missing_data",
                level: tracing::Level::WARN,
            },
            EventCase {
                comparison: ScoutPciComparison::IdentifierCollision,
                label: "identifier_collision",
                level: tracing::Level::WARN,
            },
        ];
        let metrics = MetricsCapture::start();
        let machine_id = host_machine_id();
        let logs = capture_logs(|| {
            for case in &cases {
                Comparison {
                    comparison: case.comparison,
                    subject: Some(ComparisonSubject {
                        machine_interface_id: MachineInterfaceId::from(uuid::Uuid::from_u128(1)),
                        mac_address: mac(1),
                        pci_slot: Some("0000:02:00.0".to_string()),
                    }),
                    desired_mac_address: Some(mac(1)),
                    selection_source: Some(BootInterfaceSelectionSource::RedfishChassisId),
                    reason: "test reason",
                }
                .emit(&machine_id);
            }
        });

        assert_eq!(logs.len(), cases.len());
        for (index, case) in cases.iter().enumerate() {
            assert!(
                metrics.counter_delta(
                    "carbide_scout_pci_evaluations_total",
                    &[("result", case.label)],
                ) >= 1.0,
                "missing comparison series {}",
                case.label,
            );
            assert_eq!(
                logs[index].level, case.level,
                "wrong log level for {}",
                case.label,
            );
            assert_eq!(logs[index].field("result"), Some(case.label));
        }

        let machine_id = machine_id.to_string();
        assert_eq!(logs[0].field("machine_id"), Some(machine_id.as_str()));
        assert_eq!(
            logs[0].field("interface_mac_address"),
            Some("02:00:00:00:00:01")
        );
        assert_eq!(logs[0].field("pci_slot"), Some("0000:02:00.0"));
    }
}
