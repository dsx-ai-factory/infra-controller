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

use super::*;

/// Verifies controller-2 registration repairs a pre-HostInit host and permits HostInit and boot-order
/// publication, so both the API and machine controller honor the root setting instead of controller 1.
#[crate::sqlx_test]
async fn test_bf4_controller_two_registration_advances_host_init(pool: sqlx::PgPool) {
    // Select controller 2 only through the root config; the remaining defaults provide the real API and machine controller.
    let mut config = get_config();
    config.dpu_host_pf_pci_controller_id = 2;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let mut host_config = env.managed_host_config();
    host_config.dpus[0].override_hosts_uefi_device_path = Some(
        "PciRoot(0x0)/Pci(0x1,0x0)"
            .parse()
            .expect("factory PCI path"),
    );
    let mh = create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;
    let dpu_id = mh.dpu_ids[0];
    let _ = mh.dpu().forge_agent_control().await;

    // Retain a valid controller-1 candidate with a lower PCI path so selecting it cannot satisfy controller-2 assertions.
    let target_mac = MacAddress::new([2, 0, 0, 0, 0xbf, 2]);
    let factory_mac = host_config.dpus[0].host_mac_address;
    assert_ne!(target_mac, factory_mac);
    let before = mh.host().rpc_machine().await.status.expect("host status");
    let provisional = before
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("provisional primary");
    assert_eq!(provisional.mac_address, factory_mac.to_string());
    assert!(
        before
            .interfaces
            .iter()
            .all(|interface| interface.mac_address != target_mac.to_string())
    );
    let mut txn = env.db_txn().await;
    let host = mh.host().db_machine(&mut txn).await;
    assert!(
        matches!(host.current_state(), ManagedHostState::DPUInit { dpu_states }
        if dpu_states.states.get(&dpu_id) == Some(&DpuInitState::WaitingForNetworkConfig))
    );
    let host_bmc_ip = host.status.bmc_info.ip.expect("host BMC IP");
    let mut report = db::explored_endpoints::find_by_ips(txn.as_mut(), vec![host_bmc_ip])
        .await
        .expect("host report")
        .remove(0)
        .report;
    report.systems[0]
        .ethernet_interfaces
        .push(model::site_explorer::EthernetInterface {
            mac_address: Some(target_mac), // Controller 2 must create a different host interface row.
            id: Some("BF4.Controller2".to_string()),
            uefi_device_path: Some(
                "PciRoot(0x0)/Pci(0x2,0x0)"
                    .parse()
                    .expect("controller-2 PCI path"),
            ),
            ..Default::default()
        });
    // Publish the exact Redfish evidence before registration; no Site Explorer pass runs after this point.
    sqlx::query("UPDATE explored_endpoints SET exploration_report = $2 WHERE address = $1")
        .bind(host_bmc_ip)
        .bind(sqlx::types::Json(&report))
        .execute(txn.as_mut())
        .await
        .expect("publish controller-2 boot evidence");
    txn.commit().await.expect("commit boot evidence");

    // Submit trusted inventory through DiscoverMachine while the host still permits immediate repair.
    let registered_dpu = mh.dpu().rpc_machine().await;
    let caller = registered_dpu
        .bmc
        .as_ref()
        .and_then(|bmc| bmc.machine_interface_id)
        .expect("owned DPU BMC interface");
    let mut discovery = registered_dpu
        .status
        .expect("DPU status")
        .discovery_info
        .expect("DPU hardware");
    let dpu = discovery.dpu_info.as_mut().expect("DpuData");
    dpu.part_number = "900-9D4B4-CWAA-TSA".to_string(); // Apply BF4 registration policy to the existing DPU identity.
    dpu.host_pfs = vec![
        // A usable controller-1 PF prevents the test from passing through missing-inventory fallback.
        rpc::machine_discovery::DpuHostPf {
            pci_controller_id: 1,
            pf_id: 0,
            mac_address: factory_mac.to_string(),
            physical_port_name: "c1pf0".to_string(),
            ..Default::default()
        },
        // The configured controller must replace the provisional factory-MAC association.
        rpc::machine_discovery::DpuHostPf {
            pci_controller_id: 2,
            pf_id: 0,
            mac_address: target_mac.to_string(),
            physical_port_name: "c2pf0".to_string(),
            ..Default::default()
        },
    ];
    let expected_inventory = dpu.host_pfs.clone();
    let registered = env
        .api
        .discover_machine(Request::new(MachineDiscoveryInfo {
            machine_interface_id: Some(caller),
            discovery_data: Some(DiscoveryData::Info(discovery)),
            discovery_reporter: rpc::forge::MachineDiscoveryReporter::DpuAgent as i32,
            ..Default::default()
        }))
        .await
        .expect("register controller-2 inventory")
        .into_inner();
    assert_eq!(registered.machine_id, Some(dpu_id.into()));

    // Reload accepted inventory and host links through public reads before any controller or Site Explorer work can repair them.
    let persisted_inventory = mh
        .dpu()
        .rpc_machine()
        .await
        .status
        .expect("registered DPU")
        .discovery_info
        .expect("registered hardware")
        .dpu_info
        .expect("registered DpuData");
    assert_eq!(persisted_inventory.host_pfs, expected_inventory);
    let repaired = mh.host().rpc_machine().await.status.expect("repaired host");
    let links = repaired
        .interfaces
        .iter()
        .filter(|interface| interface.attached_dpu_machine_id == Some(dpu_id))
        .collect::<Vec<_>>();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].mac_address, target_mac.to_string());
    assert!(links[0].primary_interface);
    assert_eq!(
        repaired
            .interfaces
            .iter()
            .filter(|interface| interface.primary_interface)
            .count(),
        1
    );
    let boot = env
        .api
        .get_machine_boot_interfaces(Request::new(rpc::forge::GetMachineBootInterfacesRequest {
            machine_id: Some(mh.host().id.into()),
        }))
        .await
        .expect("persisted boot interfaces")
        .into_inner();
    assert_eq!(
        boot.machine_interfaces
            .iter()
            .find(|interface| interface.interface_id == links[0].id)
            .expect("registered boot row")
            .boot_interface_id
            .as_deref(),
        Some("BF4.Controller2")
    );
    let selection = boot.reconciliation.expect("registered boot selection");
    assert_eq!(
        selection.selection_source,
        rpc::forge::BootInterfaceSelectionSource::RedfishUefiPci as i32
    );
    let desired = selection
        .desired_boot_interface
        .expect("registered desired target");
    assert_eq!(desired.mac_address, target_mac.to_string());
    assert_eq!(desired.interface_id.as_deref(), Some("BF4.Controller2"));

    // Report the repaired network version, then let the actual machine controller resolve readiness from the root config.
    network_configured_with_health(&env, &dpu_id, None).await;
    env.run_machine_state_controller_iteration_no_requeue()
        .await;
    let mut txn = env.db_txn().await;
    // Reload the committed state and outcome to prove the configured readiness check allowed HostInit.
    let initialized = mh.host().db_machine(&mut txn).await;
    assert!(matches!(
        initialized.current_state(),
        ManagedHostState::HostInit {
            machine_state: MachineState::EnableIpmiOverLan,
        }
    ));
    assert!(matches!(
        initialized.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
    txn.rollback()
        .await
        .expect("close HostInit transition read");

    // Place the host at SetBootOrder to exercise its separate readiness check with the same root-derived controller services.
    set_host_stuck_in_set_boot_order(&env, mh.host().id).await;
    env.redfish_sim.set_is_bios_setup(true);
    env.redfish_sim.set_is_boot_order_setup(false);
    let checkpoint = env.redfish_sim.timepoint();
    env.run_machine_state_controller_iteration_no_requeue()
        .await;
    let mut txn = env.db_txn().await;
    // Reload the committed job phase to distinguish successful boot-order publication from an unchanged wait.
    let programmed = mh.host().db_machine(&mut txn).await;
    assert!(
        matches!(programmed.current_state(), ManagedHostState::HostInit {
        machine_state: MachineState::SetBootOrder { set_boot_order_info: Some(info) },
    } if info.set_boot_order_state == SetBootOrderState::WaitForSetBootOrderJobScheduled)
    );
    assert!(matches!(
        programmed.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
    txn.rollback()
        .await
        .expect("close boot-order transition read");
    // Observe the target sent to Redfish so a successful write for controller 1 cannot pass.
    assert!(
        env.redfish_sim
            .actions_since(&checkpoint)
            .all_hosts()
            .iter()
            .any(|action| matches!(
                action, RedfishSimAction::SetBootOrderDpuFirst { boot_interface_mac }
                    if boot_interface_mac == &target_mac.to_string()
            ))
    );
}
