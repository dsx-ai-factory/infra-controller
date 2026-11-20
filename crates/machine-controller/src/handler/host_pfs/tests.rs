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

use carbide_test_harness::prelude::{sqlx_test, sqlx_testing};
use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use model::expected_machine::{ExpectedInterface, ExpectedMachine, ExpectedMachineData};
use model::hardware_info::{DpuData, DpuHostPf, HardwareInfo};
use model::machine::MachineInterfaceSnapshot;
use model::machine_boot_interface::BootInterfaceSelection;
use model::site_explorer::{Chassis, ComputerSystem, EndpointExplorationReport};

use super::*;

/// Provides two registered DPUs whose pairing serials come from product chassis while their first-system serial keys tie.
///
/// The snapshot order deliberately differs from host inventory encounter order to expose accidental reuse of a persisted selection order.
/// Both DPUs are synchronized at WaitingForNetworkConfig so every planner call satisfies its checkpoint precondition.
async fn planning_fixture(
    pool: &sqlx::PgPool,
    all_bf4: bool,
) -> eyre::Result<(ManagedHostStateSnapshot, Vec<MacAddress>)> {
    // Start without retained working history or selection decisions so automatic ordering remains eligible.
    let mut state = model::test_support::machine_snapshot::managed_host_state_snapshot();
    state.host_snapshot.history.clear();
    state.host_snapshot.last_machine_validation_time = None;
    state.host_snapshot.config.boot_interface_selection = None;
    state.host_snapshot.config.primary_interface_selection = None;
    state.host_snapshot.config.desired_boot_interface = None;
    // Use non-DPF ingestion so PF planning can be exercised independently of DPF lifecycle handling.
    state.host_snapshot.config.dpf.used_for_ingestion = false;
    // Position both DPUs at the synchronized checkpoint required by PF planning.
    state.managed_state = ManagedHostState::DPUInit {
        dpu_states: model::machine::DpuInitStates {
            states: state
                .dpu_snapshots
                .iter()
                .map(|dpu| (dpu.id, DpuInitState::WaitingForNetworkConfig))
                .collect(),
        },
    };
    state.host_snapshot.state.value = state.managed_state.clone();
    state.host_snapshot.status.bmc_info.ip = Some("192.0.2.10".parse()?);
    let macs = vec![
        MacAddress::new([2, 0, 0, 0, 1, 1]),
        MacAddress::new([2, 0, 0, 0, 1, 2]),
    ];
    let mut current_primary = MachineInterfaceSnapshot::mock_with_mac("02:00:00:00:ff:01".parse()?);
    current_primary.primary_interface = true;
    current_primary.machine_id = Some(state.host_snapshot.id.to_machine_id());
    current_primary.attached_dpu_machine_id = Some(state.dpu_snapshots[0].id);
    state.host_snapshot.status.interfaces = vec![current_primary];
    let mut txn = pool.begin().await?;
    for (index, dpu) in state.dpu_snapshots.iter_mut().enumerate() {
        let hardware_info = HardwareInfo {
            dpu_info: Some(DpuData {
                part_number: if all_bf4 || index == 0 {
                    "900-9D4B4-CWAA-TSA"
                } else {
                    "900-9D3B6-00CV-AA0"
                }
                .to_string(),
                factory_mac_address: format!("02:00:00:00:ff:0{}", index + 1),
                host_pfs: if all_bf4 || index == 0 {
                    vec![DpuHostPf {
                        pci_controller_id: 1,
                        pf_id: 0,
                        mac_address: macs[index],
                    }]
                } else {
                    Vec::new()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        dpu.status.hardware_info = Some(hardware_info.clone());
        let ip = format!("192.0.2.{}", index + 20).parse()?;
        dpu.status.bmc_info.ip = Some(ip);
        db::machine::create(
            &mut txn,
            None,
            dpu.id.as_machine_id(),
            ManagedHostState::Created,
            None,
            model::machine::CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
        db::machine_topology::create_or_update(&mut txn, dpu.id.as_machine_id(), &hardware_info)
            .await?;
        db::machine_topology::record_dpu_host_pfs(&mut txn, &dpu.id, &hardware_info, 1).await?;
        db::explored_endpoints::insert(
            ip,
            &EndpointExplorationReport {
                systems: vec![ComputerSystem {
                    id: "BlueField_0".to_string(),
                    serial_number: None, // BF4 pairing uses the product chassis; serial fallback still uses this empty key.
                    ..Default::default()
                }],
                chassis: vec![Chassis {
                    id: "BlueField_BMC_0".to_string(),
                    serial_number: Some(format!("paired-{index}")),
                    ..Default::default()
                }],
                ..Default::default()
            },
            false,
            &mut txn,
        )
        .await?;
    }
    db::explored_endpoints::insert(
        state.host_snapshot.status.bmc_info.ip.expect("host IP"),
        &EndpointExplorationReport {
            // Blank chassis IDs force the existing serial fallback; inventory still records the original pairing order.
            chassis: [1, 0]
                .into_iter()
                .map(|index| Chassis {
                    id: String::new(),
                    part_number: Some("900-9D4B4-CWAA-TSA".to_string()),
                    serial_number: Some(format!("paired-{index}")),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
        false,
        &mut txn,
    )
    .await?;
    txn.commit().await?;
    Ok((state, macs))
}

/// Uses host inventory order for tied first-system serials and preserves a working or Operator-selected primary DPU, so stable identity alone never fixes an interim winner.
/// A validation completion timestamp without qualifying lifecycle history must still follow the automatic winner because failed or skipped validation also sets that timestamp.
/// Preserved-DPU boot provenance transfers from its prior or corrected PF, while an unrelated automatic boot MAC yields LegacyUnknown.
#[sqlx_test]
async fn planning_restores_tie_order_and_preserves_qualified_primary_dpu(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let (state, macs) = planning_fixture(&pool, true).await?;
    for (history, completed_validation, primary_source, boot_source, expected_primary) in [
        // A never-Ready stable host must follow the original host inventory order rather than snapshot order.
        (Vec::new(), false, None, None, 1),
        // Validation completion alone cannot preserve provisional networking when the run may have failed or been skipped.
        (
            Vec::new(),
            true,
            Some(BootInterfaceSelectionSource::RedfishSerialNumber),
            None,
            1,
        ),
        // An explicit primary choice retains its DPU even when boot intent is automatic.
        (
            Vec::new(),
            false,
            Some(BootInterfaceSelectionSource::Operator),
            None,
            0,
        ),
        // A new boot-only Operator choice cannot preserve a provisional networking primary.
        (
            Vec::new(),
            false,
            Some(BootInterfaceSelectionSource::RedfishSerialNumber),
            Some(BootInterfaceSelectionSource::Operator),
            1,
        ),
        // Real Ready history qualifies primary-DPU preservation without requiring Operator provenance.
        (
            vec![model::state_history::StateHistoryRecord {
                state: serde_json::to_string(&ManagedHostState::Ready)?,
                state_version: config_version::ConfigVersion::initial(),
                time: None,
            }],
            false,
            None,
            None,
            0,
        ),
    ] {
        // Vary independent decisions, lifecycle history and validation completion over the same registered PFs.
        let mut state = state.clone();
        state.host_snapshot.history = history;
        state.host_snapshot.last_machine_validation_time =
            completed_validation.then(chrono::Utc::now);
        state.host_snapshot.config.primary_interface_selection =
            primary_source.map(|source| BootInterfaceSelection {
                source,
                updated_at: Some(chrono::Utc::now()),
            });
        state.host_snapshot.config.boot_interface_selection =
            boot_source.map(|source| BootInterfaceSelection {
                source,
                updated_at: Some(chrono::Utc::now()),
            });
        state.host_snapshot.config.desired_boot_interface =
            boot_source.map(|_| config_version::Versioned {
                value: MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 1, 9])),
                version: config_version::ConfigVersion::initial(),
            });
        let mut txn = pool.begin().await?;

        // Exercise the full planner, including authenticated inventory reads and original discovery-order reconstruction.
        let HostPfPlan::Ready(plan) = host_pf_interface_plan(&mut txn, &state, 1).await? else {
            panic!("complete facts should resolve");
        };
        txn.rollback().await?;

        // Check each DPU's own facts, the chosen network primary, and ID-free boot targeting.
        assert_eq!(
            plan.links.iter().map(|link| link.mac_address).collect_vec(),
            macs
        );
        assert_eq!(plan.network_primary_mac, macs[expected_primary]);
        assert_eq!(
            plan.boot_target,
            MachineBootInterfaceTarget::MacOnly(macs[expected_primary])
        );
        if completed_validation {
            // Completion alone must leave both automatic decisions attributed to the report that selected the competing DPU.
            assert_eq!(
                plan.network_selection_source,
                BootInterfaceSelectionSource::RedfishSerialNumber
            );
            assert_eq!(
                plan.boot_selection_source,
                BootInterfaceSelectionSource::RedfishSerialNumber
            );
        }
        assert!(
            plan.links
                .iter()
                .all(|link| link.boot_interface_id.is_none())
        );
    }
    // Give the current primary working history while the report still ranks the other DPU first.
    let mut working = state.clone();
    working.host_snapshot.history = vec![model::state_history::StateHistoryRecord {
        state: serde_json::to_string(&ManagedHostState::Ready)?,
        state_version: config_version::ConfigVersion::initial(),
        time: None,
    }];
    for (desired_mac, expected_source) in [
        // The retired primary PF's automatic authority follows that same DPU's corrected networking PF.
        (
            working.host_snapshot.status.interfaces[0].mac_address,
            BootInterfaceSelectionSource::RedfishUefiPci,
        ),
        // Already-corrected boot intent retains its own source even though the competing report uses serial order.
        (macs[0], BootInterfaceSelectionSource::RedfishUefiPci),
        // Independent boot attribution cannot describe why a different PF was selected for the preserved DPU.
        (
            MacAddress::new([2, 0, 0, 0, 1, 9]),
            BootInterfaceSelectionSource::LegacyUnknown,
        ),
    ] {
        // Vary only the prior automatic boot MAC; neither networking authority nor report order changes.
        working.host_snapshot.config.desired_boot_interface = Some(config_version::Versioned {
            value: MachineBootInterfaceTarget::MacOnly(desired_mac),
            version: config_version::ConfigVersion::initial(),
        });
        working.host_snapshot.config.boot_interface_selection = Some(BootInterfaceSelection {
            source: BootInterfaceSelectionSource::RedfishUefiPci,
            updated_at: Some(chrono::Utc::now()),
        });
        // Plan against the competing report before discarding the read-only transaction.
        let mut txn = pool.begin().await?;
        let HostPfPlan::Ready(plan) = host_pf_interface_plan(&mut txn, &working, 1).await? else {
            panic!("working host PF facts should resolve");
        };
        txn.rollback().await?;

        // Require corrected networking and boot targeting on DPU 0, with attribution tied to its own prior selection.
        assert_eq!(plan.network_primary_mac, macs[0]);
        assert_eq!(
            plan.boot_target,
            MachineBootInterfaceTarget::MacOnly(macs[0])
        );
        assert_eq!(plan.boot_selection_source, expected_source, "{desired_mac}");
    }
    Ok(())
}

/// Preserves the exact declared boot MAC while correcting a previously working DPU's networking PF, preventing automatic repair from transferring explicit intent to another MAC.
#[sqlx_test]
async fn planning_preserves_declared_boot_mac_independently_of_selected_dpu(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    // Start from Ready history, a DPU 0 primary, and reports whose serial fallback selects DPU 1.
    let (mut state, macs) = planning_fixture(&pool, true).await?;
    state.host_snapshot.history = vec![model::state_history::StateHistoryRecord {
        state: serde_json::to_string(&ManagedHostState::Ready)?,
        state_version: config_version::ConfigVersion::initial(),
        time: None,
    }];
    let factory_mac = state.host_snapshot.status.interfaces[0].mac_address;
    let independent_boot_mac = MacAddress::new([2, 0, 0, 0, 1, 9]);
    for declared_mac in [
        // A factory-MAC declaration remains exact even when that DPU's networking PF is corrected.
        factory_mac,
        // An unresolved independent declaration retains boot authority without selecting the networking DPU.
        independent_boot_mac,
    ] {
        // Keep the desired target and declaration together while the refreshed report supplies neither NIC's ID.
        state.host_snapshot.config.desired_boot_interface = Some(config_version::Versioned {
            value: MachineBootInterfaceTarget::MacOnly(declared_mac),
            version: config_version::ConfigVersion::initial(),
        });
        state.host_snapshot.config.boot_interface_selection = Some(BootInterfaceSelection {
            source: BootInterfaceSelectionSource::ExpectedMachine,
            updated_at: Some(chrono::Utc::now()),
        });
        let mut txn = pool.begin().await?;
        db::expected_machine::create(
            &mut txn,
            ExpectedMachine {
                id: None,
                bmc_mac_address: state
                    .host_snapshot
                    .status
                    .bmc_info
                    .mac
                    .expect("host BMC MAC"),
                data: ExpectedMachineData {
                    interfaces: vec![ExpectedInterface {
                        mac_address: Some(declared_mac), // Match or separate the declaration from the selected DPU's old PF.
                        primary: Some(true), // Resolve this MAC as the host's declared boot interface.
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            },
        )
        .await?;

        // Resolve the current declaration and reports through the planner before discarding each isolated declaration.
        let HostPfPlan::Ready(plan) = host_pf_interface_plan(&mut txn, &state, 1).await? else {
            panic!("registered PFs should resolve despite the missing declared NIC ID");
        };
        txn.rollback().await?;

        // Correct networking without replacing the declared boot MAC or moving its explicit attribution onto the PF.
        assert_eq!(plan.network_primary_mac, macs[0], "{declared_mac}");
        assert_eq!(
            plan.boot_target.mac_address(),
            declared_mac,
            "{declared_mac}"
        );
        assert_eq!(
            plan.boot_selection_source,
            BootInterfaceSelectionSource::ExpectedMachine,
            "{declared_mac}"
        );
    }
    Ok(())
}

/// Accepts lost DMI product metadata for a recognized BF3 after DPF platform configuration while retaining classification waits for unknown hardware and PF-evidence waits for current or retained BF4 identity.
#[sqlx_test]
async fn planning_recognizes_legacy_vpd_after_dpf_metadata_loss(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    for (part_number, retained_bf4, expected_wait) in [
        // Recognized BF3 VPD independently identifies the legacy path after the DMI product disappears.
        ("900-9D3B6-00CV-AA0", false, None),
        // An unknown part cannot turn missing DMI classification into permission to use legacy networking.
        ("unknown", false, Some("platform classification")),
        // Current BF4 VPD still requires authenticated PF facts even without DMI product metadata.
        ("900-9D4B4-CWAA-TSA", false, Some("c1pf0")),
        // Retained BF4 classification takes precedence over a later report carrying a legacy part number.
        ("900-9D3B6-00CV-AA0", true, Some("c1pf0")),
    ] {
        // Keep one DPU at the completed-platform checkpoint; only VPD and retained BF4 evidence vary.
        let mut state = model::test_support::machine_snapshot::managed_host_state_snapshot();
        state.dpu_snapshots.truncate(1);
        state.host_snapshot.config.dpf.used_for_ingestion = true;
        let dpu = &mut state.dpu_snapshots[0];
        // Remove only the product name, retaining the DMI serials and BlueField board identity accepted during discovery.
        let mut hardware_info = dpu.status.hardware_info.clone().expect("DPU hardware");
        hardware_info
            .dmi_data
            .as_mut()
            .expect("retained DMI")
            .product_name
            .clear();
        let dpu_info = hardware_info.dpu_info.as_mut().expect("DPU VPD");
        dpu_info.part_number = part_number.to_string();
        dpu_info.host_pfs.clear();
        dpu.status.hardware_info = Some(hardware_info.clone());
        let dpu_id = dpu.id;
        state.managed_state = ManagedHostState::DPUInit {
            dpu_states: model::machine::DpuInitStates {
                states: [(dpu_id, DpuInitState::WaitingForNetworkConfig)].into(),
            },
        };
        state.host_snapshot.state.value = state.managed_state.clone();
        let mut txn = pool.begin().await?;
        db::machine::create(
            &mut txn,
            None,
            dpu_id.as_machine_id(),
            ManagedHostState::Created,
            None,
            model::machine::CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
        db::machine_topology::create_or_update(&mut txn, dpu_id.as_machine_id(), &hardware_info)
            .await?;
        if retained_bf4 {
            // Retain the authenticated BF4 marker independently of the refreshed legacy VPD and absent PF facts.
            let mut accepted_bf4 = hardware_info.clone();
            accepted_bf4.dpu_info.as_mut().expect("DPU VPD").part_number =
                "900-9D4B4-CWAA-TSA".to_string();
            assert!(
                !db::machine_topology::record_dpu_host_pfs(&mut txn, &dpu_id, &accepted_bf4, 1)
                    .await?
            );
        }

        // Evaluate the same synchronized checkpoint without changing the initial DPF classifier.
        let plan = host_pf_interface_plan(&mut txn, &state, 1).await?;
        txn.rollback().await?;

        // Distinguish positive legacy recognition from waiting for classification or the selected BF4 PF.
        match expected_wait {
            None => assert!(matches!(plan, HostPfPlan::Legacy), "{part_number}"),
            Some(expected_reason) => assert!(
                matches!(plan, HostPfPlan::Waiting(reason) if reason.contains(expected_reason)),
                "{part_number}, retained BF4: {retained_bf4}"
            ),
        }
    }
    Ok(())
}

/// Waits for missing authenticated PF inventory even when raw topology and another paired DPU's accepted PF facts remain available, preventing unverified initialization.
#[sqlx_test]
async fn planning_waits_for_each_dpus_authenticated_selected_pf(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let (state, _) = planning_fixture(&pool, true).await?;
    // Leave only the second DPU without authenticated inventory, as an older registration can do.
    let missing_id = state.dpu_snapshots[1].id;
    let mut setup = pool.begin().await?;
    sqlx::query("UPDATE machine_topologies SET dpu_host_pf_inventory = NULL WHERE machine_id = $1")
        .bind(missing_id)
        .execute(&mut *setup)
        .await?;
    setup.commit().await?;

    // Recompute from persisted facts; the first DPU cannot supply the second DPU's MAC.
    let mut txn = pool.begin().await?;
    let outcome = host_pf_interface_plan(&mut txn, &state, 1).await?;
    txn.rollback().await?;

    // Name both the missing DPU and configured PF so the controller can publish an actionable wait.
    assert!(
        matches!(outcome, HostPfPlan::Waiting(reason) if reason.contains(&missing_id.to_string()) && reason.contains("c1pf0"))
    );
    Ok(())
}

/// Waits after a controller change until each BF4 DPU registers the newly configured PF, preventing an unprotected cached observation from becoming initialization authority.
#[sqlx_test]
async fn planning_waits_for_new_controller_pf_acceptance(pool: sqlx::PgPool) -> eyre::Result<()> {
    // Keep both DPUs at the checkpoint while controller 1 remains the accepted PF authority.
    let (state, _) = planning_fixture(&pool, true).await?;
    let new_controller_macs = vec![
        MacAddress::new([2, 0, 0, 0, 2, 1]),
        MacAddress::new([2, 0, 0, 0, 2, 2]),
    ];
    let mut registration_reports = Vec::new();
    let mut setup = pool.begin().await?;
    for (dpu, mac_address) in state.dpu_snapshots.iter().zip(&new_controller_macs) {
        let mut hardware_info = dpu.status.hardware_info.clone().expect("DPU hardware");
        hardware_info
            .dpu_info
            .as_mut()
            .expect("DPU PF inventory")
            .host_pfs
            .push(DpuHostPf {
                pci_controller_id: 2, // Remain nonselected during controller 1 registration.
                pf_id: 0, // Match the PF used by initialization after the controller change.
                mac_address: *mac_address,
            });
        assert!(
            db::machine_topology::record_dpu_host_pfs(&mut setup, &dpu.id, &hardware_info, 1)
                .await?
        );
        registration_reports.push((dpu.id, hardware_info));
    }
    setup.commit().await?;

    // Reload accepted observations to prove the new target exists without configured-PF protection.
    let dpu_ids = state.dpu_snapshots.iter().map(|dpu| dpu.id).collect_vec();
    let mut read = pool.begin().await?;
    let observed = db::machine_topology::find_latest_by_machine_ids(&mut read, &dpu_ids).await?;
    for (dpu_id, mac_address) in dpu_ids.iter().zip(&new_controller_macs) {
        let inventory = observed[dpu_id]
            .dpu_host_pf_inventory
            .as_ref()
            .expect("registered inventory");
        assert!(inventory.host_pfs.iter().any(|pf| {
            pf.pci_controller_id == 2 && pf.pf_id == 0 && pf.mac_address == *mac_address
        }));
        assert!(
            !inventory
                .protected_host_pfs
                .iter()
                .any(|pf| pf.pci_controller_id == 2 && pf.pf_id == 0)
        );
    }

    // Resolve controller 2 without another registration, as a Core restart can do with the agent still running.
    let outcome = host_pf_interface_plan(&mut read, &state, 2).await?;
    read.rollback().await?;

    // Wait for selected-PF acceptance even though the requested controller already has cached MACs.
    assert!(
        matches!(outcome, HostPfPlan::Waiting(reason) if reason.contains(&dpu_ids[0].to_string()) && reason.contains("c2pf0"))
    );

    // Accept current controller 2 reports before allowing those PFs to settle the host's interfaces.
    let mut accepting = pool.begin().await?;
    for (dpu_id, hardware_info) in &registration_reports {
        assert!(
            db::machine_topology::record_dpu_host_pfs(&mut accepting, dpu_id, hardware_info, 2)
                .await?
        );
    }
    accepting.commit().await?;

    // Recompute from persisted facts without updating the checkpoint snapshot or its cached hardware.
    let mut read = pool.begin().await?;
    let HostPfPlan::Ready(plan) = host_pf_interface_plan(&mut read, &state, 2).await? else {
        panic!("accepted controller 2 PFs should resolve");
    };
    read.rollback().await?;

    // Use each DPU's accepted controller 2 MAC while retaining the fixture's serial fallback winner.
    assert_eq!(
        plan.links.iter().map(|link| link.mac_address).collect_vec(),
        new_controller_macs
    );
    assert_eq!(plan.network_primary_mac, new_controller_macs[1]);
    assert_eq!(
        plan.boot_target,
        MachineBootInterfaceTarget::MacOnly(new_controller_macs[1])
    );
    Ok(())
}

/// Waits when exploration is absent or cannot pair every DPU, then resumes with the registered PFs when usable reports return, preventing incomplete inventory from selecting a provisional winner.
#[sqlx_test]
async fn planning_recovers_missing_exploration_and_pairing(pool: sqlx::PgPool) -> eyre::Result<()> {
    // Vary report availability and pairing while retaining complete authenticated PF facts for both DPUs.
    let (state, registered_macs) = planning_fixture(&pool, true).await?;
    let host_address = state.host_snapshot.status.bmc_info.ip.expect("host BMC");
    let second_dpu = &state.dpu_snapshots[1];
    let second_dpu_address = second_dpu.status.bmc_info.ip.expect("second DPU BMC");
    let mut report_without_pairing = db::explored_endpoints::find_by_ips(&pool, vec![host_address])
        .await?
        .remove(0)
        .report;
    // Omit only the second DPU's serial from host inventory so its complete exploration cannot reconstruct discovery order.
    report_without_pairing
        .chassis
        .retain(|chassis| chassis.serial_number.as_deref() != Some("paired-1"));

    check_cases_async(
        [
            // Missing host exploration cannot supply the inventory order even when every DPU report remains available.
            Case {
                scenario: "host exploration is unavailable",
                input: (host_address, None),
                expect: Yields(
                    "Waiting for host exploration to resolve registered host PF ordering".to_string(),
                ),
            },
            // An available host report cannot substitute for the second DPU's own exploration and pairing serial.
            Case {
                scenario: "DPU exploration is unavailable",
                input: (second_dpu_address, None),
                expect: Yields(format!(
                    "Waiting for DPU {} exploration to reconstruct discovery order",
                    second_dpu.id,
                )),
            },
            // Present reports still cannot establish a complete order when host inventory omits one paired serial.
            Case {
                scenario: "host inventory cannot pair the second DPU",
                input: (host_address, Some(report_without_pairing)),
                expect: Yields(format!(
                    "Waiting for unambiguous host inventory or configured pairing serial for DPU {}",
                    second_dpu.id,
                )),
            },
        ],
        |(report_address, replacement_report)| {
            let state = &state;
            let registered_macs = &registered_macs;
            let pool = &pool;
            async move {
                // Retain the complete report before replacing only this case's required exploration evidence.
                let mut txn = pool.begin().await.expect("report scenario transaction");
                let original_report =
                    db::explored_endpoints::find_by_ips(&mut *txn, vec![report_address])
                        .await
                        .expect("original report")
                        .remove(0)
                        .report;
                db::explored_endpoints::delete(&mut txn, report_address)
                    .await
                    .expect("remove complete report");
                if let Some(report) = replacement_report {
                    db::explored_endpoints::insert(report_address, &report, false, &mut txn)
                        .await
                        .expect("publish incomplete report");
                }

                // Ask the production planner to distinguish absent exploration from unusable pairing evidence.
                let HostPfPlan::Waiting(reason) = host_pf_interface_plan(&mut txn, state, 1)
                    .await
                    .expect("plan with incomplete exploration")
                else {
                    panic!("incomplete exploration must wait for usable ordering evidence");
                };

                // Restore the report before a fresh plan so recovery requires neither another PF registration nor a changed snapshot.
                db::explored_endpoints::delete(&mut txn, report_address)
                    .await
                    .expect("remove incomplete report");
                db::explored_endpoints::insert(report_address, &original_report, false, &mut txn)
                    .await
                    .expect("restore complete report");
                let HostPfPlan::Ready(recovered) = host_pf_interface_plan(&mut txn, state, 1)
                    .await
                    .expect("plan with restored exploration")
                else {
                    panic!("restored reports must resolve the registered PFs");
                };
                txn.rollback().await.expect("isolate report scenario");

                // Recover the original discovery-order winner with each DPU's own registered MAC and exact fallback source.
                assert_eq!(
                    recovered.links.iter().map(|link| link.mac_address).collect_vec(),
                    *registered_macs,
                );
                assert_eq!(recovered.network_primary_mac, registered_macs[1]);
                assert_eq!(
                    recovered.boot_target,
                    MachineBootInterfaceTarget::MacOnly(registered_macs[1]),
                );
                assert_eq!(
                    recovered.boot_selection_source,
                    BootInterfaceSelectionSource::RedfishSerialNumber,
                );
                Ok::<_, ()>(reason)
            }
        },
    )
    .await;
    Ok(())
}

/// Combines a registered BF4 MAC with an unchanged BF3 factory candidate, so mixed-host fallback never requires nonexistent BF3 PF inventory.
#[sqlx_test]
async fn planning_mixed_dpus_retains_legacy_factory_mac(pool: sqlx::PgPool) -> eyre::Result<()> {
    // The second DPU has BF3 VPD and no host-PF observations; tied serial keys still follow host inventory order.
    let (state, macs) = planning_fixture(&pool, false).await?;
    let legacy_factory = state.dpu_snapshots[1]
        .status
        .hardware_info
        .as_ref()
        .expect("legacy hardware")
        .factory_mac_address()?;

    // Exercise the same planner used for initial BF4 finalization.
    let mut txn = pool.begin().await?;
    let HostPfPlan::Ready(plan) = host_pf_interface_plan(&mut txn, &state, 1).await? else {
        panic!("mixed facts should resolve");
    };
    txn.rollback().await?;

    // Each candidate uses its own generation's factual MAC source.
    assert_eq!(plan.links[0].mac_address, macs[0]);
    assert_eq!(plan.links[1].mac_address, legacy_factory);
    assert_eq!(plan.network_primary_mac, legacy_factory);
    assert_eq!(
        plan.boot_target,
        MachineBootInterfaceTarget::MacOnly(legacy_factory)
    );
    Ok(())
}

/// Applies successful UEFI and chassis ordering ahead of competing fallback winners through the full PF planner, protecting its report and inventory wiring.
#[sqlx_test]
async fn planning_applies_uefi_and_chassis_precedence_with_competing_winners(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    // Reuse tied serial keys and encounter order [1, 0], with DPU 1 as the provisional networking primary.
    let (mut state, macs) = planning_fixture(&pool, true).await?;
    state.host_snapshot.status.interfaces[0].attached_dpu_machine_id =
        Some(state.dpu_snapshots[1].id);
    state.host_snapshot.status.interfaces[0].mac_address = "02:00:00:00:ff:02".parse()?;
    for (source, chassis_ids, with_uefi) in [
        // UEFI chooses DPU 0 while successful chassis and serial ordering both choose DPU 1.
        (
            BootInterfaceSelectionSource::RedfishUefiPci,
            ["Slot10", "Slot2"],
            true,
        ),
        // Without a UEFI selection, successful chassis ordering chooses DPU 0 over serial winner DPU 1.
        (
            BootInterfaceSelectionSource::RedfishChassisId,
            ["Slot2", "Slot10"],
            false,
        ),
    ] {
        // Change only the competing report evidence; every candidate retains its own registered PF.
        let report = EndpointExplorationReport {
            systems: if with_uefi {
                vec![ComputerSystem {
                    ethernet_interfaces: macs
                        .iter()
                        .enumerate()
                        .map(|(index, mac)| model::site_explorer::EthernetInterface {
                            id: Some(format!("NIC.{index}")),
                            mac_address: Some(*mac),
                            uefi_device_path: Some(
                                format!("PciRoot(0x0)/Pci(0x{},0x0)", index + 1)
                                    .parse()
                                    .expect("UEFI path"),
                            ),
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                }]
            } else {
                Vec::new()
            },
            chassis: [1, 0]
                .into_iter()
                .map(|index| Chassis {
                    id: chassis_ids[index].to_string(),
                    part_number: Some("900-9D4B4-CWAA-TSA".to_string()),
                    serial_number: Some(format!("paired-{index}")),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let mut txn = pool.begin().await?;
        sqlx::query("UPDATE explored_endpoints SET exploration_report = $2 WHERE address = $1")
            .bind(state.host_snapshot.status.bmc_info.ip.expect("host BMC"))
            .bind(sqlx::types::Json(report))
            .execute(&mut *txn)
            .await?;

        // Resolve persisted reports through the production planner rather than invoking a lower-level selector.
        let HostPfPlan::Ready(plan) = host_pf_interface_plan(&mut txn, &state, 1).await? else {
            panic!("complete competing facts must resolve");
        };
        txn.rollback().await?;

        // The winning registered MAC and both attributed sources must agree, independently of snapshot primary order.
        assert_eq!(plan.network_primary_mac, macs[0], "{source:?}");
        assert_eq!(plan.boot_target.mac_address(), macs[0], "{source:?}");
        assert_eq!(plan.network_selection_source, source);
        assert_eq!(plan.boot_selection_source, source);
    }
    Ok(())
}
