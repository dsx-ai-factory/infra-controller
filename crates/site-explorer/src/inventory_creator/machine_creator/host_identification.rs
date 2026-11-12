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

use itertools::Itertools;
use mac_address::MacAddress;
use model::expected_machine::HostDpuPolicy;
use model::machine_boot_interface::BootInterfaceSelectionSource;
use model::site_explorer::dpu_ordering::{
    ReportSelectionOrdering, apply_report_selection, assign_host_chassis_ids,
    sort_dpus_by_redfish_chassis_id, sort_dpus_by_redfish_serial,
};
use model::site_explorer::{
    ExploredDpu, ExploredEndpoint, ExploredManagedHost, HostPrimaryInterfaceSelection,
};

use super::MachineCreator;
use crate::IdentifiedManagedHost;
use crate::boot_interfaces::HostBootInterfaces;
use crate::metrics::{
    BootInterfaceSelected, DpuMigrationSignal, PairingBlockerReason, SiteExplorationMetrics,
};

/// A host whose DPU checks allow identification, including hosts with Ignore policy.
pub(crate) struct ConfiguredIngestionCandidate {
    pub(crate) endpoint: ExploredEndpoint,
    pub(crate) dpus: Vec<ExploredDpu>,
    pub(crate) host_dpu_policy: HostDpuPolicy,
    pub(crate) declared_primary: Option<MacAddress>,
}

impl MachineCreator {
    /// Builds managed hosts and boot-interface updates from configured candidates.
    ///
    /// Identification runs independently of whether machine creation is enabled.
    /// Unmatched observations are returned for independent reconciliation.
    pub(crate) fn identify_managed_hosts(
        candidates: Vec<ConfiguredIngestionCandidate>,
        metrics: &mut SiteExplorationMetrics,
    ) -> (
        Vec<IdentifiedManagedHost>,
        HostBootInterfaces,
        Vec<ExploredEndpoint>,
    ) {
        let mut managed_hosts = Vec::new();
        let mut boot_interfaces = HostBootInterfaces::new();
        let mut unmatched_reports = Vec::new();
        for candidate in candidates {
            let ConfiguredIngestionCandidate {
                endpoint: ep,
                dpus,
                host_dpu_policy,
                declared_primary,
            } = candidate;
            let managed_host = match host_dpu_policy {
                HostDpuPolicy::Ignore => {
                    // Keep the DPU slice empty so ignored DPU PFs cannot become
                    // fallback primaries, while still refreshing a declared host NIC.
                    let primary_interface_selection = ep
                        .report
                        .select_host_primary_interface(&[], declared_primary);
                    if let Some(selection) = primary_interface_selection {
                        boot_interfaces.add(&ep, selection.mac_address);
                    }

                    Some((
                        ExploredManagedHost {
                            host_bmc_ip: ep.address,
                            dpus: Vec::new(),
                        },
                        primary_interface_selection,
                    ))
                }
                HostDpuPolicy::Manage | HostDpuPolicy::Nic => Self::build_managed_host(
                    &ep,
                    dpus,
                    host_dpu_policy,
                    declared_primary,
                    &mut boot_interfaces,
                    metrics,
                ),
            };

            if let Some((explored_host, primary_interface_selection)) = managed_host {
                managed_hosts.push(IdentifiedManagedHost {
                    explored_host,
                    report: ep.report,
                    primary_interface_selection,
                });
                metrics.exploration_identified_managed_hosts += 1;
            } else {
                unmatched_reports.push(ep);
            }
        }

        (managed_hosts, boot_interfaces, unmatched_reports)
    }

    /// Resolves host/DPU grouping and primary-interface selection, borrowing the report.
    ///
    /// Returns `None` when an automatic boot MAC does not match the discovered DPUs.
    /// A report-resolved declared NIC may boot independently when the host has a BF4 DPU.
    /// Report-selected boot interfaces are recorded even when that mismatch blocks
    /// identification. The caller handles hosts with the `Ignore` policy separately.
    fn build_managed_host(
        ep: &ExploredEndpoint,
        mut dpus_explored_for_host: Vec<ExploredDpu>,
        host_dpu_policy: HostDpuPolicy,
        declared_primary: Option<MacAddress>,
        boot_interfaces: &mut HostBootInterfaces,
        metrics: &mut SiteExplorationMetrics,
    ) -> Option<(ExploredManagedHost, Option<HostPrimaryInterfaceSelection>)> {
        // The host report owns the physical chassis identity, while each
        // `ExploredDpu` owns the DPU endpoint report. Join those views by
        // the same serial number used for DPU pairing before we choose a
        // fallback order.
        assign_host_chassis_ids(&ep.report, &mut dpus_explored_for_host);

        // ExpectedMachine and complete UEFI PCI evidence resolve the order
        // before either provisional Redfish fallback is considered.
        let mut dpu_order_resolved = false;
        let mut primary_interface_selection = ep
            .report
            .select_host_primary_interface(&dpus_explored_for_host, declared_primary);
        let declared_nic_selected = primary_interface_selection.is_some_and(|selection| {
            selection.source == BootInterfaceSelectionSource::ExpectedMachine
                && dpus_explored_for_host.iter().any(|dpu| {
                    dpu.hardware_info_with_host_inventory(&ep.report)
                        .is_ok_and(|info| info.is_bf4_dpu())
                })
                && !dpus_explored_for_host
                    .iter()
                    .any(|dpu| dpu.host_pf_mac_address == Some(selection.mac_address))
        });
        if let Some(selection) = primary_interface_selection {
            let mac_address = selection.mac_address;
            boot_interfaces.add(ep, mac_address);

            match apply_report_selection(&mut dpus_explored_for_host, selection.mac_address) {
                ReportSelectionOrdering::Applied => dpu_order_resolved = true,
                ReportSelectionOrdering::NoDpus => {}
                // A resolved BF4-host declared NIC controls boot while fallback ordering still chooses the first DPU.
                ReportSelectionOrdering::Mismatch if declared_nic_selected => {}
                ReportSelectionOrdering::Mismatch => {
                    let all_mac = dpus_explored_for_host
                        .iter()
                        .map(|x| {
                            x.host_pf_mac_address
                                .map(|x| x.to_string())
                                .unwrap_or_default()
                        })
                        .collect_vec()
                        .join(",");

                    tracing::error!(
                        target: "carbide_site_explorer",
                        %mac_address,
                        discovered_dpu_mac_addresses = %all_mac,
                        host_bmc_ip_address = %ep.address,
                        "MAC address not found in discovered DPU list"
                    );
                    metrics.increment_host_dpu_pairing_blocker(
                        PairingBlockerReason::BootInterfaceMacMismatch,
                    );
                    return None;
                }
            }
        }

        if !dpu_order_resolved && dpus_explored_for_host.len() > 1 {
            match sort_dpus_by_redfish_chassis_id(&mut dpus_explored_for_host) {
                Ok(selection) => {
                    if !declared_nic_selected {
                        primary_interface_selection = Some(selection);
                    }
                    dpu_order_resolved = true;
                }
                Err(error) => {
                    tracing::debug!(
                        target: "carbide_site_explorer",
                        host_bmc_ip_address = %ep.address,
                        host_chassis_ids = ?dpus_explored_for_host
                            .iter()
                            .map(|dpu| dpu.host_chassis_id.as_deref())
                            .collect_vec(),
                        %error,
                        "Cannot order DPUs using Redfish chassis IDs; falling back to Redfish serial numbers",
                    );
                }
            }
        }

        if !dpu_order_resolved {
            // Serial ordering is the existing deterministic fallback. Keep
            // its result beside the selected MAC so machine creation can
            // persist the exact reason only when that same MAC becomes the
            // durable desired target.
            let serial_selection = sort_dpus_by_redfish_serial(&mut dpus_explored_for_host);
            if !declared_nic_selected && let Some(selection) = serial_selection {
                primary_interface_selection = Some(selection);
            }
        }

        // Record one selection after report ordering, mismatch handling, or
        // serial fallback has finished for a managed host with multiple
        // DPUs. Sources produced by other components are outside this Site
        // Explorer metric.
        if dpus_explored_for_host.len() > 1
            && matches!(host_dpu_policy, HostDpuPolicy::Manage)
            && let Some(selection) = primary_interface_selection
            && let Some(event) = BootInterfaceSelected::from_selection_source(
                selection.source,
                ep.address,
                dpus_explored_for_host.len(),
                selection.mac_address,
                dpus_explored_for_host
                    .first()
                    .and_then(|dpu| dpu.report.systems.first())
                    .and_then(|system| system.serial_number.clone()),
            )
        {
            carbide_instrument::emit(event);
        }

        // For `Nic` hosts, don't attach DPUs even if matching
        // discovered some: the operator has declared "treat this host
        // as zero-DPU". Any DPU hardware has already had `set_nic_mode`
        // issued by the caller's check-and-configure step if it was in
        // DPU mode; this cycle we just emit a bare host.
        // For `Ignore` hosts, we should have already returned/continued
        // earlier on after detecting the host policy as such, so
        // this shouldn't fire.
        let dpus = match host_dpu_policy {
            HostDpuPolicy::Nic => {
                metrics.increment_dpu_migration_signal(DpuMigrationSignal::RegisteredZeroDpuForNic);
                Vec::new()
            }
            HostDpuPolicy::Manage => dpus_explored_for_host,
            // Now that we continue/return early for `Ignore` hosts,
            // we shouldn't actually get here. Probably could be
            // lazy and just leave it as Vec::new(), but I think
            // this firing would also surface a bug, which we
            // probably want.
            HostDpuPolicy::Ignore => {
                unreachable!("ignored-DPU hosts should have already returned early")
            }
        };

        Some((
            ExploredManagedHost {
                host_bmc_ip: ep.address,
                dpus,
            },
            primary_interface_selection,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use model::site_explorer::{
        ComputerSystem, EndpointExplorationReport, EthernetInterface, HostPrimaryInterfaceSelection,
    };

    use super::*;

    /// Keeps a resolved declared NIC as the BF4 host's boot target while retaining and ordering managed DPUs, so identification does not require that NIC's first lease.
    /// Verifies BF2/BF3-only hosts retain mismatch rejection instead of entering BF4-only initialization.
    #[test]
    fn declared_integrated_nic_retains_managed_dpus() {
        use model::site_explorer::Chassis;

        for (bmc_part, system_model, host_part, inventory_serial, supported) in [
            // A DPU product part directly establishes BF4 on a mixed-generation host.
            (Some("900-9D4B4-CWAA-TSA"), None, None, "zeta", true),
            // A serial-matched host part supplies BF4 evidence when the DPU product part is missing.
            (None, None, Some("900-9D4B4-CWAA-TSA"), "zeta", true),
            // Pairing's host part takes precedence over a conflicting DPU product part.
            (
                Some("900-9D3B6-00CV-AA0"),
                None,
                Some("900-9D4B4-CWAA-TSA"),
                "zeta",
                true,
            ),
            // The Redfish system model establishes BF4 before actual agent DMI arrives.
            (None, Some("BlueField-4 DPU"), None, "zeta", true),
            // An unrelated host BF4 part must not reclassify a serial-matched legacy DPU.
            (
                Some("900-9D3B6-00CV-AA0"),
                None,
                Some("900-9D4B4-CWAA-TSA"),
                "other",
                false,
            ),
            // BF3-only hosts retain baseline mismatch rejection rather than gaining new boot behavior.
            (Some("900-9D3B6-00CV-AA0"), None, None, "zeta", false),
            // A BF2/BF3 host also remains outside the BF4 identification exception.
            (Some("MBF2H516A-CEEO"), None, None, "zeta", false),
        ] {
            // Declare a host NIC distinct from both DPUs; reports retain ordinary serial fallback evidence.
            let declared_mac = MacAddress::new([2, 0, 0, 0, 0, 9]);
            let endpoint = crate::test_support::explored_endpoint(EndpointExplorationReport {
                systems: vec![ComputerSystem {
                    pcie_devices: host_part
                        .map(|part| model::site_explorer::PCIeDevice {
                            part_number: Some(part.to_string()),
                            serial_number: Some(inventory_serial.to_string()), // Only the pairing serial may supply BF4 classification.
                            description: None,
                            firmware_version: None,
                            gpu_vendor: None,
                            id: None,
                            manufacturer: None,
                            name: None,
                            status: None,
                        })
                        .into_iter()
                        .collect(),
                    ethernet_interfaces: vec![EthernetInterface {
                        mac_address: Some(declared_mac), // Its own Redfish ID makes this a resolved declaration.
                        id: Some("NIC.Integrated.1".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            });
            let dpus = [(2, "zeta"), (1, "alpha")]
                .into_iter()
                .map(|(last_byte, serial)| ExploredDpu {
                    bmc_ip: format!("192.0.2.{last_byte}").parse().expect("DPU address"),
                    host_pf_mac_address: Some(MacAddress::new([2, 0, 0, 0, 0, last_byte])),
                    host_chassis_id: None,
                    report: Arc::new(EndpointExplorationReport {
                        systems: vec![ComputerSystem {
                            id: "Bluefield".to_string(), // Establish DPU report identity for part-number classification.
                            serial_number: Some(serial.to_string()),
                            model: (last_byte == 2)
                                .then_some(system_model)
                                .flatten()
                                .map(str::to_string),
                            ..Default::default()
                        }],
                        chassis: vec![Chassis {
                            id: "Card1".to_string(),
                            part_number: if last_byte == 2 {
                                bmc_part
                            } else {
                                Some("900-9D3B6-00CV-AA0")
                            }
                            .map(str::to_string), // Only one BF4 is needed on a mixed host.
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                })
                .collect();

            // Exercise managed-host identification so legacy rejection and BF4 adaptation share the real caller.
            let mut metrics = SiteExplorationMetrics::default();
            let (hosts, _, _) = MachineCreator::identify_managed_hosts(
                vec![ConfiguredIngestionCandidate {
                    endpoint,
                    dpus,
                    host_dpu_policy: HostDpuPolicy::Manage,
                    declared_primary: Some(declared_mac),
                }],
                &mut metrics,
            );

            // Inspect both the selected declaration and retained attachments, or the legacy mismatch blocker.
            if supported {
                let host = hosts.first().expect("identified managed BF4 host");
                assert_eq!(hosts.len(), 1);
                assert_eq!(
                    host.primary_interface_selection,
                    Some(HostPrimaryInterfaceSelection {
                        mac_address: declared_mac,
                        source: BootInterfaceSelectionSource::ExpectedMachine,
                    })
                );
                assert_eq!(
                    host.explored_host
                        .dpus
                        .iter()
                        .filter_map(|dpu| dpu.host_pf_mac_address)
                        .collect_vec(),
                    vec![
                        MacAddress::new([2, 0, 0, 0, 0, 1]),
                        MacAddress::new([2, 0, 0, 0, 0, 2]),
                    ]
                );
                assert!(metrics.host_dpu_pairing_blockers.is_empty());
            } else {
                assert!(
                    hosts.is_empty(),
                    "legacy managed host must retain mismatch rejection"
                );
                assert!(!metrics.host_dpu_pairing_blockers.is_empty());
            }
        }
    }
}
