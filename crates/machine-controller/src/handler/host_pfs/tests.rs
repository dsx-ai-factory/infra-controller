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
use model::hardware_info::{DpuData, DpuHostPf, HardwareInfo};
use model::machine::MachineInterfaceSnapshot;
use model::machine_boot_interface::BootInterfaceSelection;
use model::site_explorer::{Chassis, ComputerSystem, EndpointExplorationReport};

use super::*;

/// Provides two registered DPUs whose pairing serials come from product chassis while their first-system serial keys tie.
///
/// The snapshot order deliberately differs from host inventory encounter order to expose accidental reuse of a persisted selection order.
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
