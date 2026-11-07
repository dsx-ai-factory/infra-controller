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

use carbide_uuid::machine::{MachineIdSource, MachineType};
use model::address_selection_strategy::AddressSelectionStrategy;
use model::hardware_info::{DpuData, DpuHostPf, HardwareInfo};
use model::machine::{CURRENT_STATE_MODEL_VERSION, DpuInitState, DpuInitStates, MachineState};
use model::machine_interface::InterfaceType;
use model::machine_interface_address::MachineInterfaceAssociation;
use model::network_segment::NetworkSegmentControllerState;
use model::site_explorer::{ComputerSystem, EthernetInterface};

use super::*;

/// Retains the registered DPU and automatic target needed to isolate persisted readiness checks.
struct BootInterfaceFixture {
    host_id: HostMachineId,
    dpu_id: DpuMachineId,
    target: MachineBootInterfaceTarget,
}

/// Persists complete BF4 boot evidence plus one unattached NIC so a broken DPU association can retain the paired DPU.
async fn ready_boot_interface(pool: &PgPool, seed: u8) -> BootInterfaceFixture {
    // Only the later case mutation should change readiness; the remaining fields provide current, consistent boot evidence.
    let host_id = HostMachineId::try_from(MachineId::new(
        MachineIdSource::Tpm,
        [seed; 32],
        MachineType::Host,
    ))
    .expect("host ID");
    let dpu_id = DpuMachineId::try_from(MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [seed; 32],
        MachineType::Dpu,
    ))
    .expect("DPU ID");
    let primary_mac = MacAddress::new([2, 0, 0, seed, 0, 1]);
    let spare_mac = MacAddress::new([2, 0, 0, seed, 0, 2]);
    let state = ManagedHostState::DPUInit {
        dpu_states: DpuInitStates {
            states: HashMap::from([(dpu_id, DpuInitState::WaitingForNetworkConfig)]),
        },
    };
    let mut txn = pool.begin().await.expect("begin readiness fixture");
    for id in [MachineId::from(host_id), MachineId::from(dpu_id)] {
        crate::machine::create(
            &mut txn,
            None,
            &id,
            state.clone(),
            None,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await
        .expect("create machine");
    }
    let segment = crate::network_segment::persist(
        crate::test_support::network_segment::admin_segment(
            &format!("boot-readiness-{seed}"),
            &format!("192.0.{seed}.0/24"),
            &format!("192.0.{seed}.1"),
            2,
        ),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await
    .expect("create admin segment");
    let mut bmc_ip = None;
    for (mac, primary, kind, boot_id) in [
        // The registered MAC is the sole networking primary in every otherwise-ready case.
        (
            primary_mac,
            true,
            InterfaceType::Data,
            Some("Registered.HostPF0"),
        ),
        // Keep another host NIC available when moving the DPU association off the selected row.
        (
            spare_mac,
            false,
            InterfaceType::Data,
            Some("Integrated.Other"),
        ),
        // A live BMC address lets readiness reload the persisted Redfish report.
        (
            MacAddress::new([2, 0, 0, seed, 0, 3]),
            false,
            InterfaceType::Bmc,
            None,
        ),
    ] {
        let interface = crate::machine_interface::create_with_type(
            &mut txn,
            std::slice::from_ref(&segment),
            &mac,
            primary,
            AddressSelectionStrategy::NextAvailableIp,
            kind,
            None,
        )
        .await
        .expect("create host interface");
        crate::machine_interface::associate_interface_with_machine(
            &interface.id,
            MachineInterfaceAssociation::Machine(host_id.into()),
            &mut txn,
        )
        .await
        .expect("attach host interface");
        if primary {
            crate::machine_interface::associate_interface_with_dpu_machine(
                &interface.id,
                &dpu_id.into(),
                &mut txn,
            )
            .await
            .expect("pair registered DPU");
        }
        if let Some(boot_id) = boot_id {
            crate::machine_interface::set_boot_interface_id(mac, boot_id, &mut txn)
                .await
                .expect("record boot ID");
        } else {
            bmc_ip = interface.addresses.first().copied();
        }
    }

    // Initialize agent-owned inventory through its repository operation rather than generic topology refresh.
    let registered = DpuData {
        part_number: "900-9D4B4-CWAA-TSA".to_string(), // Enable the BF4 readiness policy.
        factory_mac_address: spare_mac.to_string(), // Keep factory identity distinct from the registered target.
        host_pfs: vec![DpuHostPf {
            pci_controller_id: 1, // Match the omitted controller setting.
            pf_id: 0,
            mac_address: primary_mac, // Match the fixture's sole primary.
            physical_port_name: "c1pf0".to_string(),
            netdev_name: None,
            pci_address: None,
        }],
        ..Default::default()
    };
    crate::machine_topology::create_or_update(
        &mut txn,
        &dpu_id.into(),
        &HardwareInfo {
            dpu_info: Some(registered.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("record BF4 platform");
    crate::machine_topology::record_dpu_host_pfs(&mut txn, &dpu_id, &registered)
        .await
        .expect("record registered host-PF");
    // Publish host Redfish evidence and desired intent together so readiness can reload the complete automatic decision.
    let report = EndpointExplorationReport {
        systems: vec![ComputerSystem {
            ethernet_interfaces: vec![EthernetInterface {
                mac_address: Some(primary_mac),
                id: Some("Registered.HostPF0".to_string()),
                uefi_device_path: Some("PciRoot(0x0)/Pci(0x2,0x0)".parse().expect("PCI path")),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    crate::explored_endpoints::insert(bmc_ip.expect("live BMC address"), &report, false, &mut txn)
        .await
        .expect("publish Redfish boot evidence");
    let target = MachineBootInterfaceTarget::Pair(MachineBootInterface {
        mac_address: primary_mac,
        interface_id: "Registered.HostPF0".to_string(),
    });
    crate::machine_desired_boot_interface::set(
        &mut txn,
        &host_id,
        &target,
        BootInterfaceSelectionSource::RedfishUefiPci,
    )
    .await
    .expect("publish automatic boot intent");
    txn.commit().await.expect("commit ready boot fixture");
    BootInterfaceFixture {
        host_id,
        dpu_id,
        target,
    }
}

/// Rejects each independently inconsistent persisted boot fact so an earlier failure cannot conceal a missing automatic-readiness guard.
#[crate::sqlx_test]
async fn persisted_automatic_readiness_rejects_distinct_inconsistencies(pool: PgPool) {
    struct Case {
        scenario: &'static str,
        mutation: &'static str,
        check: fn(HostBootInterfaceNotReady, &BootInterfaceFixture),
    }
    let cases = [
        // Retain pairing through the wrong MAC so the association guard runs instead of making BF4 policy disappear.
        Case {
            scenario: "registered MAC is primary but the DPU is associated with another host NIC",
            mutation: "UPDATE machine_interfaces SET attached_dpu_machine_id = CASE WHEN primary_interface THEN NULL ELSE (SELECT attached_dpu_machine_id FROM machine_interfaces WHERE machine_id = $1 AND primary_interface LIMIT 1) END WHERE machine_id = $1 AND interface_type <> 'Bmc'",
            check: |reason, fixture| {
                assert!(
                    matches!(reason, HostBootInterfaceNotReady::DpuAssociation(id) if id == fixture.dpu_id)
                )
            },
        },
        // A correct MAC and association do not make a stale Redfish boot ID usable.
        Case {
            scenario: "selected row carries a stale Redfish interface ID",
            mutation: "UPDATE machine_interfaces SET boot_interface_id = 'Stale.HostPF0' WHERE machine_id = $1 AND primary_interface",
            check: |reason, fixture| {
                assert!(
                    matches!(reason, HostBootInterfaceNotReady::InterfaceId(mac) if mac == fixture.target.mac_address())
                )
            },
        },
        // The selected row can be exact while the separately persisted desired Pair still targets an old Redfish ID.
        Case {
            scenario: "desired Pair disagrees while the selected interface is current",
            mutation: "UPDATE machine_boot_interfaces SET desired_interface_id = 'Stale.Desired' WHERE machine_id = $1",
            check: |reason, _| assert!(matches!(reason, HostBootInterfaceNotReady::DesiredTarget)),
        },
        // Preserve the valid decision time to isolate source attribution from timestamp coverage.
        Case {
            scenario: "selection source does not describe the automatic decision",
            mutation: "UPDATE machine_boot_interfaces SET selection_source = 'redfish_chassis_id' WHERE machine_id = $1",
            check: |reason, _| {
                assert!(matches!(
                    reason,
                    HostBootInterfaceNotReady::SelectionMetadata
                ))
            },
        },
    ];
    for (index, case) in cases.into_iter().enumerate() {
        // Prove the committed fixture is fully ready before changing only this case's persisted fact.
        let fixture = ready_boot_interface(&pool, 80 + index as u8).await;
        assert!(
            not_ready_reason(&pool, &fixture.host_id, 1)
                .await
                .expect(case.scenario)
                .is_none(),
            "{}",
            case.scenario
        );
        // Corrupt only the named persisted fact so earlier readiness checks cannot mask this guard.
        sqlx::query(case.mutation)
            .bind(fixture.host_id)
            .execute(&pool)
            .await
            .expect(case.scenario);

        // The public readiness operation reloads persisted state and must identify this exact inconsistency.
        let reason = not_ready_reason(&pool, &fixture.host_id, 1)
            .await
            .expect(case.scenario)
            .expect(case.scenario);
        (case.check)(reason, &fixture);
    }
}

/// Allows an upgraded HostInit target that agrees with registered evidence while preserving its legacy history and existing networking.
#[crate::sqlx_test]
async fn legacy_boot_order_target_preserves_networking_and_selection_history(pool: PgPool) {
    let fixture = ready_boot_interface(&pool, 90).await;
    // Model an upgraded HostInit with truthful unknown source metadata and a networking primary on a different NIC.
    let mut txn = pool.begin().await.expect("begin legacy state");
    crate::machine::update_state(
        &mut txn,
        &fixture.host_id,
        &ManagedHostState::HostInit {
            machine_state: MachineState::SetBootOrder {
                set_boot_order_info: None,
            },
        },
    )
    .await
    .expect("resume legacy HostInit");
    sqlx::query("UPDATE machine_boot_interfaces SET selection_source = 'legacy_unknown', selection_updated_at = NULL WHERE machine_id = $1")
        .bind(fixture.host_id).execute(&mut *txn).await.expect("retain legacy attribution");
    // Demote before promotion because the database permits only one primary interface per machine.
    sqlx::query("UPDATE machine_interfaces SET primary_interface = false WHERE machine_id = $1 AND primary_interface")
        .bind(fixture.host_id).execute(&mut *txn).await.expect("demote previous primary");
    sqlx::query("UPDATE machine_interfaces SET primary_interface = true WHERE machine_id = $1 AND mac_address = $2")
        .bind(fixture.host_id).bind(MacAddress::new([2, 0, 0, 90, 0, 2])).execute(&mut *txn).await.expect("retain existing networking primary");
    txn.commit().await.expect("commit legacy state");
    let mut reader = pool.acquire().await.expect("read legacy snapshot");
    let before = load_facts(&mut reader, &fixture.host_id, 1)
        .await
        .expect("find legacy facts")
        .expect("BF4 applies")
        .snapshot
        .host_snapshot;

    // A matching automatic target can progress, but a different provisional MAC still requires explicit recovery.
    assert!(
        boot_order_not_ready_reason(&pool, &fixture.host_id, 1, &fixture.target)
            .await
            .expect("check preserved target")
            .is_none()
    );
    let provisional = MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 90, 0, 2]));
    let blocked = boot_order_not_ready_reason(&pool, &fixture.host_id, 1, &provisional)
        .await
        .expect("check provisional target")
        .expect("provisional MAC must wait");
    assert!(matches!(
        blocked,
        HostBootInterfaceNotReady::PreservedAutomaticTarget
    ));
    assert!(blocked.to_string().contains("SetDpuFirstBootOrder"));

    // The permitted boot check and skipped network repair must not rewrite active networking, target, or legacy history.
    let mut txn = pool.begin().await.expect("begin skipped repair");
    reconcile_pre_host_init(&mut txn, &fixture.host_id, 1, false, None)
        .await
        .expect("skip HostInit network repair");
    txn.commit().await.expect("commit skipped repair");
    let after = load_facts(&mut reader, &fixture.host_id, 1)
        .await
        .expect("find preserved facts")
        .expect("BF4 applies")
        .snapshot
        .host_snapshot;
    assert_eq!(
        serde_json::to_value(&after.status.interfaces).expect("serialize preserved interfaces"),
        serde_json::to_value(&before.status.interfaces).expect("serialize original interfaces")
    );
    assert_eq!(after.network_config.version, before.network_config.version);
    assert_eq!(after.network_config.value, before.network_config.value);
    assert_eq!(
        after
            .config
            .desired_boot_interface
            .as_ref()
            .map(|desired| (&desired.value, desired.version)),
        before
            .config
            .desired_boot_interface
            .as_ref()
            .map(|desired| (&desired.value, desired.version))
    );
    assert_eq!(
        after.config.boot_interface_selection,
        before.config.boot_interface_selection
    );
}

/// Preserves both supported Operator choices (boot-only MAC and changed networking primary) because their shared source does not authorize automatic network repair.
#[crate::sqlx_test]
async fn operator_boot_intent_preserves_explicit_networking_choices(pool: PgPool) {
    for (seed, change_primary) in [
        // Boot-only intent may select another NIC while administrative traffic remains on the registered primary.
        (91, false),
        // SetPrimaryInterface intentionally moves networking as well as boot intent, so repair must retain that choice.
        (92, true),
    ] {
        let fixture = ready_boot_interface(&pool, seed).await;
        let operator_mac = MacAddress::new([2, 0, 0, seed, 0, 2]);
        let operator_target = if change_primary {
            MachineBootInterfaceTarget::Pair(MachineBootInterface {
                mac_address: operator_mac,
                interface_id: "Integrated.Other".to_string(),
            })
        } else {
            MachineBootInterfaceTarget::MacOnly(operator_mac)
        };
        // Persist explicit authority and optionally model the networking change made by SetPrimaryInterface.
        let mut txn = pool.begin().await.expect("begin explicit selection");
        crate::machine_desired_boot_interface::set(
            &mut txn,
            &fixture.host_id,
            &operator_target,
            BootInterfaceSelectionSource::Operator,
        )
        .await
        .expect("record operator choice");
        if change_primary {
            // Match SetPrimaryInterface's demote-before-promote ordering to preserve the unique primary constraint.
            sqlx::query("UPDATE machine_interfaces SET primary_interface = false WHERE machine_id = $1 AND primary_interface")
                .bind(fixture.host_id).execute(&mut *txn).await.expect("demote previous primary");
            sqlx::query("UPDATE machine_interfaces SET primary_interface = true WHERE machine_id = $1 AND mac_address = $2")
                .bind(fixture.host_id).bind(operator_target.mac_address()).execute(&mut *txn).await.expect("model operator networking choice");
        }
        txn.commit().await.expect("commit explicit choice");
        let mut reader = pool.acquire().await.expect("read operator baseline");
        let before = load_facts(&mut reader, &fixture.host_id, 1)
            .await
            .expect("find operator baseline")
            .expect("BF4 applies")
            .snapshot
            .host_snapshot;

        // Explicit intent passes initial readiness and authorizes only its actual resolved boot target.
        assert!(
            not_ready_reason(&pool, &fixture.host_id, 1)
                .await
                .expect("check explicit readiness")
                .is_none()
        );
        assert!(
            boot_order_not_ready_reason(&pool, &fixture.host_id, 1, &operator_target)
                .await
                .expect("check explicit boot target")
                .is_none()
        );
        // A current owned Redfish ID can strengthen MacOnly intent without selecting a different physical NIC.
        let enriched_target = MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address: operator_target.mac_address(),
            interface_id: "Integrated.Other".to_string(),
        });
        assert!(
            boot_order_not_ready_reason(&pool, &fixture.host_id, 1, &enriched_target)
                .await
                .expect("check same-MAC enrichment")
                .is_none()
        );
        if change_primary {
            // Once explicit intent includes an ID, another ID for the same MAC cannot borrow that authority.
            let conflicting_target = MachineBootInterfaceTarget::Pair(MachineBootInterface {
                mac_address: operator_mac,
                interface_id: "Stale.Other".to_string(),
            });
            assert!(matches!(
                boot_order_not_ready_reason(&pool, &fixture.host_id, 1, &conflicting_target)
                    .await
                    .expect("check conflicting ID"),
                Some(HostBootInterfaceNotReady::DesiredTarget)
            ));
        }
        assert!(matches!(
            boot_order_not_ready_reason(&pool, &fixture.host_id, 1, &fixture.target)
                .await
                .expect("check stale target"),
            Some(HostBootInterfaceNotReady::DesiredTarget)
        ));

        // Pre-HostInit repair must preserve either operator networking choice together with boot authority and decision time.
        let mut txn = pool.begin().await.expect("begin operator repair");
        reconcile_pre_host_init(&mut txn, &fixture.host_id, 1, false, None)
            .await
            .expect("skip operator network repair");
        txn.commit().await.expect("commit skipped operator repair");
        let after = load_facts(&mut reader, &fixture.host_id, 1)
            .await
            .expect("find operator preservation")
            .expect("BF4 applies")
            .snapshot
            .host_snapshot;
        assert_eq!(
            serde_json::to_value(&after.status.interfaces).expect("serialize preserved interfaces"),
            serde_json::to_value(&before.status.interfaces).expect("serialize operator interfaces")
        );
        assert_eq!(after.network_config.version, before.network_config.version);
        assert_eq!(after.network_config.value, before.network_config.value);
        // A boot-only choice gains its known ID while an already complete Operator pair stays unchanged.
        let after_desired = after
            .config
            .desired_boot_interface
            .as_ref()
            .expect("preserved operator MAC");
        let before_version = before
            .config
            .desired_boot_interface
            .as_ref()
            .expect("operator baseline")
            .version;
        assert_eq!(after_desired.value, enriched_target);
        // Preserve the complete pair's timestamp and compare enrichment's numeric increment separately
        // because enrichment receives its timestamp during the database write.
        if change_primary {
            assert_eq!(after_desired.version, before_version);
        } else {
            assert_eq!(
                after_desired.version.version_nr(),
                before_version.version_nr() + 1
            );
        }
        assert_eq!(
            after.config.boot_interface_selection,
            before.config.boot_interface_selection
        );
    }
}

/// Blocks both BF4 readiness checks on dedicated identity drift even with Operator intent, while keeping other health sources and nonblocking reports independent.
#[crate::sqlx_test]
async fn host_pf_drift_precedes_operator_boot_intent(pool: PgPool) {
    for (index, (source, blocking, expect_blocked)) in [
        // Keep a dedicated alert without PreventHostStateChanges from preventing new boot work.
        ("dpu-host-pf", false, false),
        // Leave other sources to their existing health checks so the BF4 guard preserves their precedence.
        ("unrelated-health", true, false),
        // Explicit boot authority cannot override the dedicated report left by a rejected registration.
        ("dpu-host-pf", true, true),
    ]
    .into_iter()
    .enumerate()
    {
        // Keep all registered facts ready and change only the report source or lifecycle classification under explicit boot intent.
        let fixture = ready_boot_interface(&pool, 93 + index as u8).await;
        let mut report = health_report::HealthReport::empty(source.to_string());
        report.alerts.push(health_report::HealthProbeAlert {
            id: "DpuHostPfIdentityDrift".parse().expect("probe ID"),
            target: Some(fixture.dpu_id.to_string()),
            in_alert_since: None,
            message: "registered host-PF changed".to_string(),
            tenant_message: None,
            classifications: if blocking {
                vec![health_report::HealthAlertClassification::prevent_host_state_changes()]
            } else {
                vec![]
            },
        });
        let mut txn = pool.begin().await.expect("begin explicit drift fixture");
        crate::machine_desired_boot_interface::set(
            &mut txn,
            &fixture.host_id,
            &fixture.target,
            BootInterfaceSelectionSource::Operator,
        )
        .await
        .expect("set explicit authority");
        crate::health_report::insert_health_report(
            &mut txn,
            "machines",
            &fixture.dpu_id,
            health_report::HealthReportApplyMode::Merge,
            &report,
        )
        .await
        .expect("record identity report");
        txn.commit().await.expect("commit explicit drift fixture");

        // Both public checks reload the committed report before honoring the otherwise valid Operator target.
        for reason in [
            not_ready_reason(&pool, &fixture.host_id, 1)
                .await
                .expect("check HostInit readiness"),
            boot_order_not_ready_reason(&pool, &fixture.host_id, 1, &fixture.target)
                .await
                .expect("check new boot-order readiness"),
        ] {
            if expect_blocked {
                assert!(
                    matches!(reason, Some(HostBootInterfaceNotReady::DpuHostPfIdentityDrift(id)) if id == fixture.dpu_id)
                );
            } else {
                assert!(reason.is_none(), "{source}: {reason:?}");
            }
        }
    }
}

/// Keeps an Operator target MAC-only when owned IDs conflict, preventing a prediction from concealing ambiguous physical-interface evidence.
#[crate::sqlx_test]
async fn operator_enrichment_rejects_conflicting_owned_ids(pool: PgPool) {
    // The spare NIC carries the Operator MAC; the registered primary supplies otherwise complete BF4 evidence.
    let fixture = ready_boot_interface(&pool, 94).await;
    let operator_mac = MacAddress::new([2, 0, 0, 94, 0, 2]);
    let operator_target = MachineBootInterfaceTarget::MacOnly(operator_mac);
    let mut txn = pool
        .begin()
        .await
        .expect("begin ambiguous operator evidence");
    crate::machine_desired_boot_interface::set(
        &mut txn,
        &fixture.host_id,
        &operator_target,
        BootInterfaceSelectionSource::Operator,
    )
    .await
    .expect("record operator MAC");
    // Another segment permits the same owned MAC to report a conflicting ID without violating segment-local uniqueness.
    let segment = crate::network_segment::persist(
        crate::test_support::network_segment::admin_segment(
            "operator-ambiguity",
            "192.0.194.0/24",
            "192.0.194.1",
            2,
        ),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await
    .expect("create second admin segment");
    let duplicate = crate::machine_interface::create_with_type(
        &mut txn,
        &[segment],
        &operator_mac, // The repeated MAC must disagree with the existing spare NIC's ID.
        false,
        AddressSelectionStrategy::NextAvailableIp,
        InterfaceType::Data,
        None,
    )
    .await
    .expect("create conflicting owned NIC");
    crate::machine_interface::associate_interface_with_machine(
        &duplicate.id,
        MachineInterfaceAssociation::Machine(fixture.host_id.into()),
        &mut txn,
    )
    .await
    .expect("attach conflicting NIC");
    // Change only the duplicate row's ID so the original row retains the conflicting value.
    sqlx::query(
        "UPDATE machine_interfaces SET boot_interface_id = 'Conflicting.Other' WHERE id = $1",
    )
    .bind(duplicate.id)
    .execute(&mut *txn)
    .await
    .expect("record conflicting owned ID");
    // A valid prediction must not resolve the ambiguity in owned interface IDs.
    crate::predicted_machine_interface::create(
        model::predicted_machine_interface::NewPredictedMachineInterface {
            machine_id: &fixture.host_id.into(),
            mac_address: operator_mac,
            expected_network_segment_type: model::network_segment::NetworkSegmentType::HostInband,
            boot_interface_id: Some("Predicted.Other"), // A tempting fallback must not bypass conflicting owned IDs.
            primary_interface: false,
        },
        &mut txn,
    )
    .await
    .expect("record fallback prediction");
    txn.commit()
        .await
        .expect("commit ambiguous operator evidence");
    // Reload the committed ambiguity before repair so the comparison includes both real interfaces.
    let mut reader = pool
        .acquire()
        .await
        .expect("read operator ambiguity baseline");
    let before = load_facts(&mut reader, &fixture.host_id, 1)
        .await
        .expect("load ambiguity baseline")
        .expect("BF4 applies")
        .snapshot
        .host_snapshot;

    // Run the guarded BF4 transaction with both conflicting owned IDs and a usable prediction.
    let mut txn = pool.begin().await.expect("begin ambiguity repair");
    reconcile_pre_host_init(&mut txn, &fixture.host_id, 1, false, None)
        .await
        .expect("leave ambiguous Operator target unresolved");
    txn.commit().await.expect("commit ambiguity repair");

    // Reload persisted facts to prove neither target authority nor networking changed after resolution refused to guess.
    let after = load_facts(&mut reader, &fixture.host_id, 1)
        .await
        .expect("reload ambiguity result")
        .expect("BF4 applies")
        .snapshot
        .host_snapshot;
    assert_eq!(
        after
            .config
            .desired_boot_interface
            .as_ref()
            .map(|desired| (&desired.value, desired.version)),
        before
            .config
            .desired_boot_interface
            .as_ref()
            .map(|desired| (&desired.value, desired.version))
    );
    assert_eq!(
        after.config.boot_interface_selection,
        before.config.boot_interface_selection
    );
    assert_eq!(
        serde_json::to_value(&after.status.interfaces).expect("serialize preserved interfaces"),
        serde_json::to_value(&before.status.interfaces).expect("serialize ambiguous interfaces")
    );
    assert_eq!(after.network_config.value, before.network_config.value);
    assert_eq!(after.network_config.version, before.network_config.version);
}
