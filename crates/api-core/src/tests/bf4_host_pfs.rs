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

use carbide_machine_controller::io::MachineStateControllerIO;
use carbide_machine_controller::metrics::MachineMetrics;
use carbide_uuid::machine::{AsMachineId, HostMachineId, MachineInterfaceId};
use futures::FutureExt;
use mac_address::MacAddress;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::hardware_info::HardwareInfo;
use model::machine::{DpuInitState, ManagedHostState};
use model::machine_boot_interface::{BootInterfaceSelectionSource, MachineBootInterfaceTarget};
use rpc::forge::forge_server::Forge;
use state_controller::db_write_batch::DbWriteBatch;
use state_controller::io::StateControllerIO;
use state_controller::state_handler::{StateHandler, StateHandlerContext, StateHandlerOutcome};
use tonic::{Code, Request};

use super::common::api_fixtures::dpu::dpu_discover_dhcp;
use super::common::api_fixtures::test_managed_host::TestPredictedManagedHost;
use super::common::api_fixtures::{TestEnv, create_test_env, network_configured_with_health};

/// Registers a BF4 report through the public DpuAgent registration path so bootstrap and PF persistence remain part of the wiring proof.
async fn register_facts(
    env: &TestEnv,
    interface_id: MachineInterfaceId,
    hardware_info: HardwareInfo,
) -> Result<(), tonic::Status> {
    env.api
        .discover_machine(Request::new(rpc::MachineDiscoveryInfo {
            machine_interface_id: Some(interface_id),
            discovery_data: Some(rpc::DiscoveryData::Info(
                rpc::DiscoveryInfo::try_from(hardware_info).expect("wire hardware"),
            )),
            create_machine: true,
            discovery_reporter: rpc::MachineDiscoveryReporter::DpuAgent as i32,
            ..Default::default()
        }))
        .await
        .map(drop)
}

/// Associates checkpoint reports in either discovery order before controller finalization, keeping DMI identity stable and factual PF MACs distinct from bootstrap networking.
async fn explore_checkpoint_host<'a>(
    env: &'a TestEnv,
    agent_first: bool,
    declared_primary: Option<MacAddress>,
) -> (
    Box<super::common::api_fixtures::site_explorer::MockExploredHost<'a>>,
    HardwareInfo,
) {
    let mut config = env.managed_host_config();
    if let Some(mac_address) = declared_primary {
        // Keep this NIC unleased so boot selection must use the declared primary prediction.
        config.non_dpu_macs.push(mac_address);
        config
            .expected_machine_data
            .get_or_insert_with(Default::default)
            .interfaces
            .push(model::expected_machine::ExpectedInterface {
                mac_address: Some(mac_address),
                primary: Some(true),
                ..Default::default()
            });
    }
    let dpu = config.get_and_assert_single_dpu();
    let mut hardware_info = HardwareInfo::from(dpu);
    let dpu_info = hardware_info.dpu_info.as_mut().expect("DPU hardware");
    dpu_info.part_number = "900-9D4B4-CWAA-TSA".to_string(); // VPD supplies late BF4 classification without changing the DMI-derived ID.
    dpu_info.host_pfs = vec![model::hardware_info::DpuHostPf {
        pci_controller_id: env.config.dpu_host_pf_pci_controller_id,
        pf_id: 0,
        mac_address: "02:00:00:fa:00:01".parse().expect("corrected PF MAC"),
    }];
    if agent_first {
        // Let the API create provisional host networking before Explorer supplies host and DPU reports.
        let interface_id = dpu_discover_dhcp(env, &dpu.oob_mac_address.to_string()).await;
        register_facts(env, interface_id, hardware_info.clone())
            .await
            .expect("register BF4 facts");
    }
    // Register the declaration before Explorer receives host inventory and DPU reports.
    super::common::api_fixtures::site_explorer::register_expected_machine(env, &config, None).await;
    let mut explored =
        super::common::api_fixtures::site_explorer::MockExploredHost::new(env, config.clone())
            .discover_dhcp_dpu_bmc(0, |_, _| Ok(()))
            .await
            .expect("DPU BMC lease")
            .discover_dhcp_dpu_primary_iface(0)
            .await
            .discover_dhcp_host_bmc(|_, _| Ok(()))
            .boxed()
            .await
            .expect("host BMC lease")
            .insert_site_exploration_results()
            .expect("exploration reports");
    if declared_primary.is_some() {
        let mut report = model::site_explorer::EndpointExplorationReport::from(dpu.clone());
        report
            .chassis
            .iter_mut()
            .find(|chassis| chassis.id == "Card1")
            .expect("DPU product chassis")
            .part_number = None; // The DPU BMC omits BF4 part evidence; only the host inventory may establish it.
        let serial = report
            .dpu_pairing_serial_number()
            .expect("DPU pairing serial");
        let mut host_report = model::site_explorer::EndpointExplorationReport::from(config.clone());
        host_report
            .systems
            .iter_mut()
            .flat_map(|system| &mut system.pcie_devices)
            .find(|device| device.serial_number.as_deref().map(str::trim) == Some(serial))
            .expect("serial-matched host inventory")
            .part_number = Some("900-9D4B4-CWAA-TSA".to_string());
        assert!(
            !report
                .systems
                .first()
                .and_then(|system| system.model.as_deref())
                .is_some_and(|model| carbide_utils::is_bf4_dpu("", model))
        );
        env.endpoint_explorer.insert_endpoints(vec![
            (explored.dpu_bmc_ips[&0], report),
            (explored.host_bmc_ip.expect("host BMC"), host_report),
        ]);
    }
    // Run ordinary exploration and preingestion after publishing the report, before controller setup.
    explored = explored
        .run_site_explorer_iteration()
        .boxed()
        .await
        .mark_preingestion_complete()
        .boxed()
        .await
        .expect("preingestion")
        .run_site_explorer_iteration()
        .boxed()
        .await;
    (explored, hardware_info)
}

/// Advances an explored checkpoint fixture to its initial network acknowledgement so repair tests can distinguish a fresh required ACK from the provisional one.
async fn checkpoint_host(
    env: &TestEnv,
    agent_first: bool,
    declared_primary: Option<MacAddress>,
) -> (TestPredictedManagedHost, HardwareInfo, MachineInterfaceId) {
    let (explored, hardware_info) =
        explore_checkpoint_host(env, agent_first, declared_primary).await;
    acknowledge_checkpoint_host(env, explored, hardware_info, agent_first).await
}

/// Advances explored inventory to its initial ACK while modeling late VPD for Explorer-first repair, so callers can inspect production topology before the fixture replaces it.
async fn acknowledge_checkpoint_host(
    env: &TestEnv,
    explored: Box<super::common::api_fixtures::site_explorer::MockExploredHost<'_>>,
    hardware_info: HardwareInfo,
    agent_first: bool,
) -> (TestPredictedManagedHost, HardwareInfo, MachineInterfaceId) {
    // Finish ordinary DPU setup before introducing late classification and acknowledging provisional networking.
    let dpu = explored.managed_host.get_and_assert_single_dpu().clone();
    let explored = explored
        .dpu_state_controller_iterations_to_network_install()
        .boxed()
        .await;
    let dpu_id = explored.dpu_machine_ids[&0];
    let mut read = env.db_txn().await;
    let host_id = db::machine::find_host_by_dpu_machine_id(&mut read, &dpu_id)
        .await
        .expect("host lookup")
        .expect("paired host")
        .id;
    read.rollback().await.expect("close fixture read");
    let host = TestPredictedManagedHost {
        id: host_id.try_into().expect("predicted host"),
        dpu_ids: vec![dpu_id],
        api: env.api.clone(),
    };
    if !agent_first {
        // Model VPD arriving before the required authenticated PF report while preserving unrelated topology fields.
        let mut setup = env.db_txn().await;
        let mut topology =
            db::machine_topology::find_latest_by_machine_ids(&mut setup, &[host.dpu_ids[0]])
                .await
                .expect("topology")
                .remove(&host.dpu_ids[0])
                .expect("DPU topology");
        let mut partial = hardware_info.clone();
        partial
            .dpu_info
            .as_mut()
            .expect("DPU hardware")
            .host_pfs
            .clear();
        topology.topology.discovery_data.info = partial;
        sqlx::query("UPDATE machine_topologies SET topology = $2 WHERE machine_id = $1")
            .bind(host.dpu_ids[0])
            .bind(sqlx::types::Json(&topology.topology))
            .execute(setup.as_mut())
            .await
            .expect("late classification");
        setup.commit().await.expect("commit late classification");
    }
    let interface_id = dpu_discover_dhcp(env, &dpu.oob_mac_address.to_string()).await;
    host.dpu().forge_agent_control().await;
    network_configured_with_health(env, &host.dpu_ids[0], None).await;
    (host, hardware_info, interface_id)
}

/// Repairs Explorer-first bootstrap only after factual PF registration, rejects the old ACK, and resumes on ordinary controller sweeps without another discovery event.
///
/// Controller-7 selection rejects controller-1-only discovery and retains c7pf0 through finalization, exposing a hardcoded default in either registration or controller configuration.
/// An overdue unready DPU must reboot once and retain its cooldown without changing corrected networking before a fresh ACK admits HostInit.
#[crate::sqlx_test]
async fn explorer_first_checkpoint_waits_repairs_and_requires_new_ack(pool: sqlx::PgPool) {
    use carbide_redfish::libredfish::test_support::RedfishSimAction;
    use carbide_redfish::libredfish::{PowerState, SystemPowerControl};
    use model::machine::{MachineLastRebootRequested, MachineLastRebootRequestedMode};

    // Select controller 7 while the common fixture provides ordinary pairing, provisional networking and an initial ACK.
    let mut config = super::common::api_fixtures::get_config();
    config.dpu_host_pf_pci_controller_id = 7;
    let env = super::common::api_fixtures::create_test_env_with_overrides(
        pool,
        super::common::api_fixtures::TestEnvOverrides::with_config(config),
    )
    .await;
    let (host, mut hardware_info, interface_id) = checkpoint_host(&env, false, None).await;
    let corrected_mac = hardware_info.dpu_info.as_ref().expect("DPU").host_pfs[0].mac_address;

    // Observe the old networking version and persisted missing-facts wait after the first checkpoint pass.
    let mut read = env.db_txn().await;
    let before = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let waiting = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    assert!(
        matches!(&waiting.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. })
        if reason.contains("c7pf0") && reason.contains(&host.dpu_ids[0].to_string()))
    );
    assert_eq!(
        waiting.network_config.version,
        before.network_config.version
    );

    // Reject a report that supplies only the default controller before accepting competing c1pf0 and c7pf0 facts.
    let default_pf = model::hardware_info::DpuHostPf {
        pci_controller_id: 1, // A registration hardcoded to the default would incorrectly accept this report.
        pf_id: 0,
        mac_address: "02:00:00:fa:00:02"
            .parse()
            .expect("default-controller PF MAC"),
    };
    let mut default_only = hardware_info.clone();
    default_only.dpu_info.as_mut().expect("DPU").host_pfs = vec![default_pf.clone()];
    let rejected = register_facts(&env, interface_id, default_only)
        .await
        .expect_err("configured controller 7 must be present");
    // Require registration to identify the configured PF rather than fail for an unrelated discovery condition.
    assert_eq!(rejected.code(), Code::FailedPrecondition);
    assert!(rejected.message().contains("c7pf0"));

    // Include both observations so finalization must select c7pf0 over the default controller.
    hardware_info
        .dpu_info
        .as_mut()
        .expect("DPU")
        .host_pfs
        .push(default_pf);

    // Submit fresh authenticated facts and run one checkpoint pass, retaining the repair's immediate Wait outcome.
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    assert!(
        matches!(&repaired.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. })
        if reason.contains("finalized") && reason.contains("reload"))
    );
    assert_ne!(
        repaired.network_config.version,
        before.network_config.version
    );
    let found = host.host().rpc_machine().await;
    let status = found.status.expect("persisted host status");
    let primary = status
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("primary");
    assert_eq!(primary.mac_address, corrected_mac.to_string());
    assert_eq!(primary.attached_dpu_machine_id, Some(host.dpu_ids[0]));
    assert_eq!(
        db::machine_desired_boot_interface::get(&env.pool, &host.id.into())
            .await
            .expect("desired")
            .expect("initialized target")
            .value,
        MachineBootInterfaceTarget::MacOnly(corrected_mac)
    );

    // A later sweep must inspect persisted facts and the new version instead of admitting the pre-repair ACK.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let unacknowledged = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    assert!(
        matches!(&unacknowledged.state.value, ManagedHostState::DPUInit { dpu_states }
        if dpu_states.states.values().all(|state| matches!(state, DpuInitState::WaitingForNetworkConfig)))
    );
    assert_eq!(
        unacknowledged.network_config.version,
        repaired.network_config.version
    );
    assert!(
        matches!(&unacknowledged.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. })
        if reason.contains("current network config version"))
    );

    // Backdate the unready DPU's state and previous reboot to make one ordinary retry overdue without reaching the power-cycle interval.
    let mut setup = env.db_txn().await;
    let overdue_dpu = host.dpu().db_machine(&mut setup).await;
    let entered_at = chrono::Utc::now() - chrono::Duration::minutes(3);
    let overdue_version = format!(
        "V{}-T{}",
        overdue_dpu.state.version.version_nr(),
        entered_at.timestamp_micros()
    );
    sqlx::query("UPDATE machines SET controller_state_version = $2 WHERE id = $1")
        .bind(host.dpu_ids[0])
        .bind(overdue_version)
        .execute(setup.as_mut())
        .await
        .expect("backdate unready DPU state");
    let previous_reboot = MachineLastRebootRequested {
        time: entered_at + chrono::Duration::minutes(1), // One-minute retry cooldown has elapsed within the fifteen-cycle limit.
        mode: MachineLastRebootRequestedMode::Reboot,
        ..Default::default()
    };
    db::machine::record_reboot_request(
        host.dpu_ids[0].as_machine_id(),
        setup.as_mut(),
        &previous_reboot,
    )
    .await
    .expect("backdate prior DPU reboot");
    setup.commit().await.expect("commit overdue DPU");
    let dpu_bmc_ip = overdue_dpu
        .status
        .bmc_info
        .ip
        .expect("DPU BMC IP")
        .to_string();
    env.redfish_sim.set_power_state(&dpu_bmc_ip, PowerState::On);

    // Let the checkpoint release its publication transaction and request the overdue external reboot.
    let reboot_started_at = chrono::Utc::now();
    let reboot_checkpoint = env.redfish_sim.timepoint();
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let reboot_wait = host.host().db_machine(&mut read).await;
    let rebooted_dpu = host.dpu().db_machine(&mut read).await;
    let requested_reboot = rebooted_dpu
        .status
        .last_reboot_requested
        .expect("persisted checkpoint reboot");
    read.rollback().await.expect("close reboot read");
    assert_eq!(reboot_wait.state.value, unacknowledged.state.value);
    assert_eq!(
        reboot_wait.network_config.version,
        repaired.network_config.version
    );
    assert!(requested_reboot.time >= reboot_started_at);
    assert_eq!(
        requested_reboot.mode,
        MachineLastRebootRequestedMode::Reboot
    );
    assert!(matches!(&reboot_wait.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, source_ref: Some(source_ref) })
        if reason.contains("Rebooting again") && source_ref.file.ends_with("/handler/host_pfs.rs")));
    assert_eq!(
        env.redfish_sim
            .actions_since(&reboot_checkpoint)
            .for_host(&dpu_bmc_ip)
            .iter()
            .filter(|action| matches!(
                action,
                RedfishSimAction::Power(SystemPowerControl::ForceRestart)
            ))
            .count(),
        1
    );

    // Sweep again without an ACK to prove the persisted retry timestamp prevents another reboot or network version change.
    let cooldown_checkpoint = env.redfish_sim.timepoint();
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let cooldown_wait = host.host().db_machine(&mut read).await;
    let cooling_dpu = host.dpu().db_machine(&mut read).await;
    read.rollback().await.expect("close cooldown read");
    assert_eq!(cooldown_wait.state.value, unacknowledged.state.value);
    assert_eq!(
        cooldown_wait.network_config.version,
        repaired.network_config.version
    );
    assert_eq!(
        cooling_dpu
            .status
            .last_reboot_requested
            .expect("retained reboot cooldown")
            .time,
        requested_reboot.time
    );
    assert!(matches!(&cooldown_wait.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, source_ref: Some(source_ref) })
        if reason.contains("Will attempt next reboot at") && source_ref.file.ends_with("/handler/host_pfs.rs")));
    assert!(
        env.redfish_sim
            .actions_since(&cooldown_checkpoint)
            .for_host(&dpu_bmc_ip)
            .iter()
            .all(|action| !matches!(action, RedfishSimAction::Power(_)))
    );

    // Report the repaired version through the public network-status API, then admit HostInit on the next sweep.
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let admitted = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    assert!(matches!(
        admitted.state.value,
        ManagedHostState::HostInit {
            machine_state: model::machine::MachineState::EnableIpmiOverLan
        }
    ));
    assert_eq!(
        admitted.network_config.version,
        repaired.network_config.version
    );
}

/// Defensively waits for periodic retry when persisted DPU-state keys disagree with unchanged attachments, preserving initialization data until correction while ordinary contention still requests immediate retry.
#[crate::sqlx_test]
async fn checkpoint_waits_for_structural_dpu_state_mismatch_without_requeue(pool: sqlx::PgPool) {
    use std::collections::HashMap;

    use carbide_uuid::machine::{DpuMachineId, MachineId, MachineIdSource, MachineType};

    // Keep authenticated PF facts and actual attachments complete, then replace only the persisted DPU-state key.
    let env = create_test_env(pool).await;
    let (host, hardware_info, interface_id) = checkpoint_host(&env, false, None).await;
    let corrected_mac = hardware_info.dpu_info.as_ref().expect("DPU facts").host_pfs[0].mac_address;
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register PF facts");
    let host_id: HostMachineId = host.id.into();
    let dpu_id = host.dpu_ids[0];
    let unexpected_dpu: DpuMachineId = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0xfd; 32],
        MachineType::Dpu,
    )
    .try_into()
    .expect("unattached DPU ID");
    let mismatched_state = ManagedHostState::DPUInit {
        dpu_states: model::machine::DpuInitStates {
            states: HashMap::from([(unexpected_dpu, DpuInitState::WaitingForNetworkConfig)]),
        },
    };
    let mut setup = env.db_txn().await;
    db::machine::update_state(&mut setup, &host_id, &mismatched_state)
        .await
        .expect("persist defensive DPU-state mismatch");
    setup.commit().await.expect("commit mismatched state");

    // Freeze state, networking, boot intent and interface allocations so an ordinary Wait cannot conceal repair writes.
    let machine_ids: Vec<MachineId> = vec![host_id.into(), dpu_id.into()];
    let snapshot_query = "SELECT jsonb_build_object(
        'interfaces', (SELECT jsonb_agg(to_jsonb(i) ORDER BY i.id) FROM machine_interfaces i
            WHERE i.machine_id = ANY($1) OR i.attached_dpu_machine_id = ANY($1)),
        'allocations', (SELECT jsonb_agg(to_jsonb(a) ORDER BY a.id) FROM machine_interface_addresses a
            JOIN machine_interfaces i ON i.id = a.interface_id WHERE i.machine_id = ANY($1)),
        'boot', (SELECT to_jsonb(b) FROM machine_boot_interfaces b WHERE b.machine_id = $2),
        'versions', (SELECT jsonb_agg(jsonb_build_object('id', m.id, 'version', m.version,
            'controller_state', m.controller_state, 'controller_state_version', m.controller_state_version,
            'network_config_version', m.network_config_version, 'network_config', m.network_config)
            ORDER BY m.id) FROM machines m WHERE m.id = ANY($1)))";
    let before: serde_json::Value = sqlx::query_scalar(snapshot_query)
        .bind(&machine_ids)
        .bind(host_id)
        .fetch_one(&env.pool)
        .await
        .expect("initialization before mismatch wait");
    let mut read = env.db_txn().await;
    let mut snapshot = host.snapshot(&mut read).await;
    let controller_state = snapshot.host_snapshot.state.clone();
    read.rollback()
        .await
        .expect("close mismatched snapshot read");
    let mut services = env.machine_state_handler_services();
    let mut metrics = MachineMetrics::default();
    let mut pending_db_writes = DbWriteBatch::new();
    let mut ctx = StateHandlerContext {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut pending_db_writes,
    };

    // Inspect the production handler's scheduling request before the processor persists its outcome.
    let outcome = env
        .machine_state_handler
        .handle_object_state(&host_id, &mut snapshot, &controller_state.value, &mut ctx)
        .await
        .expect("defer structural mismatch");
    let mismatch_reason = "BF4 host initialization DPU states do not match attached DPUs";
    assert!(matches!(outcome, StateHandlerOutcome::Wait {
        reason, requeue_immediately: false, ..
    } if reason == mismatch_reason));

    // Enable normal requeue handling so an empty queue proves ordinary-Wait policy instead of test-helper suppression.
    env.run_machine_state_controller_iteration().await;
    let mut read = env.db_txn().await;
    let waiting = host.host().db_machine(&mut read).await;
    assert_eq!(waiting.state.value, mismatched_state);
    assert_eq!(waiting.state.version, controller_state.version);
    assert!(matches!(&waiting.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, source_ref: Some(source_ref) })
        if reason == mismatch_reason && source_ref.file.ends_with("/handler/host_pfs.rs")));
    let queued: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM machine_state_controller_queued_objects WHERE object_id = $1)",
    )
    .bind(host_id.to_string())
    .fetch_one(read.as_mut())
    .await
    .expect("mismatch queue state");
    assert!(!queued);
    read.rollback()
        .await
        .expect("close persisted mismatch wait read");
    let after: serde_json::Value = sqlx::query_scalar(snapshot_query)
        .bind(&machine_ids)
        .bind(host_id)
        .fetch_one(&env.pool)
        .await
        .expect("initialization after mismatch wait");
    assert_eq!(after, before);

    // Restore the actual DPU key, then hold its host row to distinguish a transient admission retry from the stored mismatch.
    let valid_state = ManagedHostState::DPUInit {
        dpu_states: model::machine::DpuInitStates {
            states: HashMap::from([(dpu_id, DpuInitState::WaitingForNetworkConfig)]),
        },
    };
    let mut setup = env.db_txn().await;
    db::machine::update_state(&mut setup, &host_id, &valid_state)
        .await
        .expect("restore attached DPU state");
    setup.commit().await.expect("commit corrected state map");
    let mut read = env.db_txn().await;
    let mut corrected_snapshot = host.snapshot(&mut read).await;
    assert_eq!(corrected_snapshot.managed_state, valid_state);
    assert_ne!(
        corrected_snapshot.host_snapshot.state.version,
        controller_state.version
    );
    read.rollback()
        .await
        .expect("close corrected snapshot read");
    let mut busy_host = env.db_txn().await;
    sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
        .bind(host_id)
        .execute(busy_host.as_mut())
        .await
        .expect("hold host admission row");
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        env.machine_state_handler.handle_object_state(
            &host_id,
            &mut corrected_snapshot,
            &valid_state,
            &mut ctx,
        ),
    )
    .await
    .expect("busy host admission must not wait on the held row")
    .expect("defer busy host admission");
    assert!(matches!(outcome, StateHandlerOutcome::Wait {
        reason, requeue_immediately: true, ..
    } if reason == "BF4 host initialization changed or is busy; reload"));
    busy_host
        .rollback()
        .await
        .expect("release host admission row");

    // A later ordinary sweep must repair corrected networking, then admit HostInit after the fresh network ACK.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    let primary = repaired
        .status
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("corrected primary after state-map recovery");
    assert_eq!(primary.mac_address, corrected_mac);
    assert_eq!(primary.attached_dpu_machine_id, Some(dpu_id));
    assert_ne!(
        repaired.network_config.version,
        waiting.network_config.version
    );
    read.rollback()
        .await
        .expect("close repaired state-map read");
    network_configured_with_health(&env, &dpu_id, None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let admitted = host.host().db_machine(&mut read).await;
    assert!(matches!(
        admitted.state.value,
        ManagedHostState::HostInit { .. }
    ));
    assert_eq!(
        admitted.network_config.version,
        repaired.network_config.version
    );
    read.rollback()
        .await
        .expect("close recovered HostInit read");
}

/// Requeues PF repair when a supported fixed reservation appears after validation, preserving the original leases, links and boot decisions instead of adopting an unadmitted HostInband row.
#[crate::sqlx_test]
async fn checkpoint_requeues_unadmitted_pf_row_and_preserves_networking(pool: sqlx::PgPool) {
    use super::common::api_fixtures::site_explorer::{MockExploredHost, register_expected_machine};
    use crate::test_support::fixture_config::ManagedHostConfigExt;

    // Pair two DPUs so the first PF creation can pause repair before the second PF adoption lookup.
    let env = create_test_env(pool).await;
    let config = env.managed_host_config().with_dpu_count(2);
    register_expected_machine(&env, &config, None).await;
    let mut explored = MockExploredHost::new(&env, config.clone());
    for index in 0..2 {
        explored = explored
            .discover_dhcp_dpu_bmc(index, |_, _| Ok(()))
            .await
            .expect("DPU BMC lease")
            .discover_dhcp_dpu_primary_iface(index)
            .await;
    }
    let explored = explored
        .discover_dhcp_host_bmc(|_, _| Ok(()))
        .boxed()
        .await
        .expect("host BMC lease")
        .insert_site_exploration_results()
        .expect("exploration reports")
        .run_site_explorer_iteration()
        .boxed()
        .await
        .mark_preingestion_complete()
        .boxed()
        .await
        .expect("preingestion")
        .run_site_explorer_iteration()
        .boxed()
        .await
        .dpu_state_controller_iterations_to_network_install()
        .boxed()
        .await;
    let mut read = env.db_txn().await;
    let host_id: HostMachineId =
        db::machine::find_host_by_dpu_machine_id(read.as_mut(), &explored.dpu_machine_ids[&0])
            .await
            .expect("host lookup")
            .expect("paired host")
            .id;
    let host = TestPredictedManagedHost {
        id: host_id.try_into().expect("predicted host"),
        dpu_ids: explored.dpu_machine_ids.values().copied().collect(),
        api: env.api.clone(),
    };
    let initial = host.snapshot(&mut read).await;
    read.rollback().await.expect("close initial read");
    assert_eq!(initial.dpu_snapshots.len(), 2);
    let dpu_ids = initial
        .dpu_snapshots
        .iter()
        .map(|dpu| dpu.id)
        .collect::<Vec<_>>();
    let corrected_macs: [MacAddress; 2] = [
        "02:00:00:fa:01:01".parse().expect("first corrected MAC"),
        "02:00:00:fa:01:02".parse().expect("second corrected MAC"),
    ];

    // Keep snapshot order fixed by the old primary while registering both corrected MACs as previously absent facts.
    let mut second_factory_mac = None;
    for (position, dpu_id) in dpu_ids.iter().enumerate() {
        let index = (0..2)
            .find(|index| explored.dpu_machine_ids[index] == *dpu_id)
            .expect("configured DPU");
        let dpu = &config.dpus[usize::from(index)];
        if position == 1 {
            second_factory_mac = Some(dpu.host_mac_address);
        }
        let mut hardware_info = HardwareInfo::from(dpu);
        let dpu_info = hardware_info.dpu_info.as_mut().expect("DPU hardware");
        dpu_info.part_number = "900-9D4B4-CWAA-TSA".to_string(); // Require BF4 PF facts without changing DMI identity.
        dpu_info.host_pfs = vec![model::hardware_info::DpuHostPf {
            pci_controller_id: env.config.dpu_host_pf_pci_controller_id,
            pf_id: 0,
            mac_address: corrected_macs[position], // Both corrected rows are absent when repair starts.
        }];
        let interface_id = dpu_discover_dhcp(&env, &dpu.oob_mac_address.to_string()).await;
        register_facts(&env, interface_id, hardware_info)
            .await
            .expect("register BF4 PF facts");
    }

    // Resolve the second PF in the host report and declare it primary so late adoption would displace Admin networking.
    let host_bmc_ip = explored.host_bmc_ip.expect("host BMC address");
    let mut setup = env.db_txn().await;
    let mut endpoint = db::explored_endpoints::find_by_ips(setup.as_mut(), vec![host_bmc_ip])
        .await
        .expect("host report lookup")
        .remove(0);
    endpoint
        .report
        .systems
        .iter_mut()
        .flat_map(|system| &mut system.ethernet_interfaces)
        .find(|interface| interface.mac_address == second_factory_mac)
        .expect("second DPU host interface")
        .mac_address = Some(corrected_macs[1]);
    assert!(
        endpoint
            .report
            .find_interface_id_for_mac(corrected_macs[1])
            .is_some()
    );
    assert_eq!(
        db::explored_endpoints::try_update(
            host_bmc_ip,
            endpoint.report_version,
            &endpoint.report,
            false,
            setup.as_mut()
        )
        .await
        .expect("update host report"),
        db::ConditionalWrite::Applied(())
    );
    setup.commit().await.expect("commit resolved PF report");
    let mut declared = env
        .api
        .get_expected_machine(Request::new(rpc::ExpectedMachineRequest {
            bmc_mac_address: initial
                .host_snapshot
                .status
                .bmc_info
                .mac
                .expect("host BMC MAC")
                .to_string(),
            id: None,
        }))
        .await
        .expect("find host declaration")
        .into_inner();
    for interface in &mut declared.host_nics {
        interface.primary = None;
    }
    declared.host_nics.push(rpc::ExpectedInterface {
        mac_address: Some(corrected_macs[1].to_string()),
        role: Some(rpc::ExpectedInterfaceRole::Host as i32),
        primary: Some(true),
        ..Default::default()
    });
    env.api
        .update_expected_machine(Request::new(declared))
        .await
        .expect("declare second PF");

    // Prepare another inventory record and a domain-free HostInband segment without reserving either PF yet.
    let reservation_segment = crate::test_support::network_segment::create_network_segment(
        &env.api,
        "late-pf-reservation",
        "192.0.77.0/24",
        "192.0.77.1",
        rpc::NetworkSegmentType::HostInband,
        None,
        false,
    )
    .await;
    env.api
        .add_expected_machine(Request::new(rpc::ExpectedMachine {
            bmc_mac_address: "02:00:00:fc:00:01".to_string(),
            bmc_username: "ADMIN".to_string(),
            bmc_password: "test-password".to_string(),
            chassis_serial_number: "LATEPFRESERVE".to_string(),
            ..Default::default()
        }))
        .await
        .expect("create separate inventory record");
    let mut reservation = env
        .api
        .get_expected_machine(Request::new(rpc::ExpectedMachineRequest {
            bmc_mac_address: "02:00:00:fc:00:01".to_string(),
            id: None,
        }))
        .await
        .expect("find separate inventory record")
        .into_inner();
    reservation.host_nics.push(rpc::ExpectedInterface {
        mac_address: Some(corrected_macs[1].to_string()),
        role: Some(rpc::ExpectedInterfaceRole::Host as i32),
        primary: Some(true),
        ip_allocation: Some(rpc::ExpectedInterfaceIpAllocation::Fixed as i32),
        network_segment_type: Some(rpc::NetworkSegmentType::HostInband as i32),
        fixed_ip: Some("192.0.77.80".to_string()), // The supported update path creates an anonymous Static row.
        ..Default::default()
    });

    // Acknowledge both provisional networks so the later wait is caused by the late reservation rather than an old ACK.
    for dpu_id in &dpu_ids {
        network_configured_with_health(&env, dpu_id, None).await;
    }

    // Capture this host's rows; the separate reservation is a legitimate committed change outside this host.
    let machine_ids = std::iter::once(host_id.to_machine_id())
        .chain(dpu_ids.iter().map(|id| id.to_machine_id()))
        .collect::<Vec<_>>();
    let snapshot_query = "SELECT jsonb_build_object(
        'interfaces', (SELECT jsonb_agg(to_jsonb(i) ORDER BY i.id) FROM machine_interfaces i
            WHERE i.machine_id = ANY($1) OR i.attached_dpu_machine_id = ANY($1)),
        'allocations', (SELECT jsonb_agg(to_jsonb(a) ORDER BY a.id) FROM machine_interface_addresses a
            JOIN machine_interfaces i ON i.id = a.interface_id WHERE i.machine_id = ANY($1)),
        'boot', (SELECT to_jsonb(b) FROM machine_boot_interfaces b WHERE b.machine_id = $2),
        'versions', (SELECT jsonb_agg(jsonb_build_object('id', m.id, 'version', m.version,
            'controller_state', m.controller_state, 'controller_state_version', m.controller_state_version,
            'network_config_version', m.network_config_version, 'network_config', m.network_config)
            ORDER BY m.id) FROM machines m WHERE m.id = ANY($1)))";
    let before: serde_json::Value = sqlx::query_scalar(snapshot_query)
        .bind(&machine_ids)
        .bind(host_id)
        .fetch_one(&env.pool)
        .await
        .expect("original host persistence");
    let mut read = env.db_txn().await;
    let mut snapshot = host.snapshot(&mut read).await;
    assert!(snapshot.managed_host_network_config_version_synced());
    let primary = snapshot
        .host_snapshot
        .status
        .interfaces
        .iter()
        .find(|interface| {
            interface.primary_interface && interface.attached_dpu_machine_id == Some(dpu_ids[0])
        })
        .expect("original primary");
    assert!(!primary.addresses.is_empty());
    assert!(
        db::machine_interface_address::find_for_interface(read.as_mut(), primary.id)
            .await
            .expect("original DHCP allocation")
            .iter()
            .any(|address| address.allocation_type == model::allocation_type::AllocationType::Dhcp)
    );
    for mac in corrected_macs {
        assert!(
            db::machine_interface::find_by_mac_address(read.as_mut(), mac)
                .await
                .expect("corrected MAC lookup")
                .is_empty()
        );
    }
    let admin_domain: Option<carbide_uuid::domain::DomainId> =
        sqlx::query_scalar("SELECT subdomain_id FROM network_segments WHERE id = $1")
            .bind(primary.segment_id)
            .fetch_one(read.as_mut())
            .await
            .expect("Admin domain");
    read.rollback().await.expect("close original state read");
    let controller_state = snapshot.host_snapshot.state.clone();
    let rendered_before = env
        .api
        .get_managed_host_network_config(Request::new(rpc::ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_ids[0]),
        }))
        .await
        .expect("render original Admin networking")
        .into_inner();
    assert!(rendered_before.use_admin_network);
    assert!(
        !rendered_before
            .admin_interface
            .as_ref()
            .expect("original Admin interface")
            .addresses
            .is_empty()
    );

    // Pause the first PF's domain reference after all validation reads, without taking a third transaction or machine lock.
    let mut gate = env.pool.acquire().await.expect("domain gate connection");
    gate.close_on_drop(); // Cancellation must close the session instead of returning an advisory lock to the pool.
    sqlx::query("SELECT pg_advisory_lock(hashtextextended('domains:id:' || $1::text, 0))")
        .bind(admin_domain.expect("Admin segment has a domain"))
        .execute(&mut *gate)
        .await
        .expect("hold domain reference gate");
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *gate)
        .await
        .expect("gate backend");
    let mut services = env.machine_state_handler_services();
    let mut metrics = MachineMetrics::default();
    let mut pending_db_writes = DbWriteBatch::new();
    let mut ctx = StateHandlerContext {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut pending_db_writes,
    };

    // Commit the supported reservation only once the first PF creation proves both initial validations finished.
    let writer_env = &env;
    let (outcome, reserved_id) = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::join!(
            env.machine_state_handler.handle_object_state(
                &host_id,
                &mut snapshot,
                &controller_state.value,
                &mut ctx
            ),
            async move {
                db::test_support::postgres::wait_for_blocked_query(
                    &writer_env.pool,
                    gate_pid,
                    "pg_advisory_xact_lock_shared",
                )
                .await;
                writer_env
                    .api
                    .update_expected_machine(Request::new(reservation))
                    .await
                    .expect("commit concurrent fixed reservation");

                // Find the committed row before releasing repair, distinguishing adoption from an INSERT conflict.
                let mut read = writer_env.db_txn().await;
                let rows =
                    db::machine_interface::find_by_mac_address(read.as_mut(), corrected_macs[1])
                        .await
                        .expect("late reservation lookup");
                assert_eq!(rows.len(), 1);
                let reserved = &rows[0];
                assert_eq!(reserved.segment_id, reservation_segment);
                assert_eq!(
                    reserved.network_segment_type,
                    Some(model::network_segment::NetworkSegmentType::HostInband)
                );
                assert_eq!(
                    reserved.interface_type,
                    model::machine_interface::InterfaceType::Data
                );
                assert!(reserved.machine_id.is_none());
                assert!(reserved.attached_dpu_machine_id.is_none());
                assert!(reserved.primary_interface);
                assert_eq!(reserved.addresses.len(), 1);
                assert_eq!(reserved.addresses[0].to_string(), "192.0.77.80");
                let allocations =
                    db::machine_interface_address::find_for_interface(read.as_mut(), reserved.id)
                        .await
                        .expect("reservation allocations");
                assert_eq!(allocations.len(), 1);
                assert_eq!(
                    allocations[0].allocation_type,
                    model::allocation_type::AllocationType::Static
                );
                let reserved_id = reserved.id;
                read.rollback()
                    .await
                    .expect("close committed reservation read");
                gate.close().await.expect("release domain gate");
                reserved_id
            }
        )
    })
    .await
    .expect("reservation and repair complete without lock inversion");
    // Require immediate retry so a later admission can lock and validate the committed reservation in allocator order.
    assert!(
        matches!(outcome.expect("late row requests retry"), StateHandlerOutcome::Wait {
        requeue_immediately: true, reason, ..
    } if reason.contains("interfaces changed after admission"))
    );

    // Reload original leases, attachments, primary/boot provenance and versions after the handler's rollback.
    let after: serde_json::Value = sqlx::query_scalar(snapshot_query)
        .bind(&machine_ids)
        .bind(host_id)
        .fetch_one(&env.pool)
        .await
        .expect("host after rejected adoption");
    assert_eq!(after, before);
    let mut read = env.db_txn().await;
    assert!(
        db::machine_interface::find_by_mac_address(read.as_mut(), corrected_macs[0])
            .await
            .expect("first PF rollback")
            .is_empty()
    );
    let reserved = db::machine_interface::find_one(read.as_mut(), reserved_id)
        .await
        .expect("retained reservation");
    assert!(reserved.machine_id.is_none());
    assert!(reserved.attached_dpu_machine_id.is_none());
    let mut retry_snapshot = host.snapshot(&mut read).await;
    read.rollback().await.expect("close rollback read");

    // A fresh attempt admits the reservation's row first, then rejects its HostInband segment through normal validation.
    let retry_state = retry_snapshot.host_snapshot.state.value.clone();
    let error = env
        .machine_state_handler
        .handle_object_state(&host_id, &mut retry_snapshot, &retry_state, &mut ctx)
        .await
        .err()
        .expect("reserved HostInband PF conflicts with Admin networking");
    assert!(
        error
            .to_string()
            .contains("conflicting ownership or network segment")
    );
    let after_retry: serde_json::Value = sqlx::query_scalar(snapshot_query)
        .bind(&machine_ids)
        .bind(host_id)
        .fetch_one(&env.pool)
        .await
        .expect("host after segment rejection");
    assert_eq!(after_retry, before);
    let rendered_after = env
        .api
        .get_managed_host_network_config(Request::new(rpc::ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_ids[0]),
        }))
        .await
        .expect("original Admin networking remains renderable")
        .into_inner();
    assert!(rendered_after.use_admin_network);
    assert_eq!(
        rendered_after.host_interface_id,
        rendered_before.host_interface_id
    );
    assert_eq!(
        rendered_after.admin_interface,
        rendered_before.admin_interface
    );
    assert_eq!(
        rendered_after.managed_host_config_version,
        rendered_before.managed_host_config_version
    );
}

/// Requeues PF finalization when ExpectedMachine and its matching host report change the declared boot NIC after initial planning, preventing repair from using interface admission for an obsolete target.
#[crate::sqlx_test]
async fn checkpoint_requeues_changed_declared_boot_nic_before_repair(pool: sqlx::PgPool) {
    use std::collections::HashMap;

    // Keep one unleased declaration in the original plan while authenticated PF facts supply complete DPU networking.
    let env = super::common::api_fixtures::create_test_env_with_host_inband(pool).await;
    let original_mac = MacAddress::new([2, 0, 0, 0xfa, 0, 8]);
    let changed_mac = MacAddress::new([2, 0, 0, 0xfa, 0, 9]);
    let (host, hardware_info, interface_id) =
        checkpoint_host(&env, false, Some(original_mac)).await;
    let corrected_mac = hardware_info.dpu_info.as_ref().expect("DPU facts").host_pfs[0].mac_address;
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register PF facts");
    let mut read = env.db_txn().await;
    let before = host.host().db_machine(&mut read).await;
    let host_bmc_ip = before.status.bmc_info.ip.expect("host BMC address");
    let host_bmc_mac = before.status.bmc_info.mac.expect("host BMC MAC");
    let original_interfaces = before
        .status
        .interfaces
        .iter()
        .map(|interface| {
            (
                interface.id,
                (
                    interface.mac_address,
                    interface.primary_interface,
                    interface.attached_dpu_machine_id,
                    interface.addresses.clone(),
                ),
            )
        })
        .collect::<HashMap<_, _>>();
    read.rollback().await.expect("close initial plan read");

    // Block allocator admission after initial planning, before the finalizer acquires interface and machine locks for its reread.
    let mut admission_gate = env.db_txn().await;
    db::machine_interface::lock_all_admin_segments(admission_gate.as_mut())
        .await
        .expect("hold PF admission");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(admission_gate.as_mut())
        .await
        .expect("admission backend");
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::join!(
            env.run_machine_state_controller_iteration_without_requeue(),
            async {
                db::test_support::postgres::wait_for_blocked_query(
                    &env.pool,
                    blocker_pid,
                    "pg_advisory_xact_lock",
                )
                .await;

                // Commit matching declaration and report changes while the old plan is waiting for admission.
                let mut update = env.db_txn().await;
                let mut expected =
                    db::expected_machine::find_by_bmc_mac_address(update.as_mut(), host_bmc_mac)
                        .await
                        .expect("ExpectedMachine lookup")
                        .expect("ExpectedMachine");
                expected
                    .data
                    .interfaces
                    .iter_mut()
                    .find(|interface| interface.mac_address == Some(original_mac))
                    .expect("original declaration")
                    .mac_address = Some(changed_mac);
                db::expected_machine::update(update.as_mut(), &expected)
                    .await
                    .expect("change declared MAC");
                let mut endpoint =
                    db::explored_endpoints::find_by_ips(update.as_mut(), vec![host_bmc_ip])
                        .await
                        .expect("host report lookup")
                        .remove(0);
                endpoint
                    .report
                    .systems
                    .iter_mut()
                    .flat_map(|system| &mut system.ethernet_interfaces)
                    .find(|interface| interface.mac_address == Some(original_mac))
                    .expect("original declared NIC report")
                    .mac_address = Some(changed_mac);
                assert_eq!(
                    db::explored_endpoints::try_update(
                        host_bmc_ip,
                        endpoint.report_version,
                        &endpoint.report,
                        false,
                        update.as_mut(),
                    )
                    .await
                    .expect("change declared NIC report"),
                    db::ConditionalWrite::Applied(())
                );
                update.commit().await.expect("commit changed boot evidence");
                admission_gate
                    .rollback()
                    .await
                    .expect("release PF admission");
            }
        );
    })
    .await
    .expect("changed declaration must release and requeue PF finalization");

    // Reload the precise candidate-change wait and every original interface to prove no repair ran with obsolete admission.
    let mut read = env.db_txn().await;
    let deferred = host.host().db_machine(&mut read).await;
    let deferred_interfaces = deferred
        .status
        .interfaces
        .iter()
        .map(|interface| {
            (
                interface.id,
                (
                    interface.mac_address,
                    interface.primary_interface,
                    interface.attached_dpu_machine_id,
                    interface.addresses.clone(),
                ),
            )
        })
        .collect::<HashMap<_, _>>();
    assert_eq!(deferred.state.value, before.state.value);
    assert_eq!(
        deferred.network_config.version,
        before.network_config.version
    );
    assert_eq!(deferred_interfaces, original_interfaces);
    assert_eq!(
        deferred.config.boot_interface_selection,
        before.config.boot_interface_selection
    );
    assert_eq!(
        deferred
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
    assert!(matches!(&deferred.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, source_ref: Some(source_ref) })
        if reason == "Host PF candidates changed before repair; reload"
            && source_ref.file.ends_with("/handler/host_pfs.rs")));
    assert!(
        db::predicted_machine_interface::find_by_mac_address(&mut read, changed_mac)
            .await
            .expect("unadmitted boot NIC lookup")
            .is_none()
    );
    read.rollback().await.expect("close deferred repair read");

    // Plan again from committed evidence so the new boot NIC is admitted alongside the same corrected network PF.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    let primary = repaired
        .status
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("corrected primary");
    assert_eq!(primary.mac_address, corrected_mac);
    assert_eq!(primary.attached_dpu_machine_id, Some(host.dpu_ids[0]));
    assert_ne!(
        repaired.network_config.version,
        before.network_config.version
    );
    assert_eq!(
        repaired
            .config
            .desired_boot_interface
            .expect("changed declared boot target")
            .value
            .mac_address(),
        changed_mac
    );
    assert_eq!(
        repaired
            .config
            .boot_interface_selection
            .expect("changed declaration source")
            .source,
        BootInterfaceSelectionSource::ExpectedMachine
    );
    assert!(matches!(
        repaired.state.value,
        ManagedHostState::DPUInit { .. }
    ));
    read.rollback()
        .await
        .expect("close changed declaration repair read");
}

/// Requeues a declaration changed after unchanged repair and applies it before HostInit, preventing the last publication pass from permanently retaining an obsolete automatic boot target.
#[crate::sqlx_test]
async fn checkpoint_requeues_changed_declaration_before_publication(pool: sqlx::PgPool) {
    // Keep the declared integrated NIC unleased; use the bootstrap PF as another already-resolved boot candidate.
    let env = super::common::api_fixtures::create_test_env_with_host_inband(pool).await;
    let original_mac = MacAddress::new([2, 0, 0, 0xfa, 0, 8]);
    let (host, hardware_info, interface_id) =
        checkpoint_host(&env, false, Some(original_mac)).await;
    let mut read = env.db_txn().await;
    let bootstrap = host.host().db_machine(&mut read).await;
    let changed_mac = bootstrap
        .status
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("bootstrap networking primary")
        .mac_address;
    read.rollback().await.expect("close bootstrap read");
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;

    // Find the settled target and declaration before gating the no-op pass after repair has committed.
    let mut read = env.db_txn().await;
    let settled = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close settled read");
    let original_target = settled
        .config
        .desired_boot_interface
        .as_ref()
        .expect("settled target");
    assert_eq!(original_target.value.mac_address(), original_mac);
    let mut declaration = env
        .api
        .get_expected_machine(Request::new(rpc::ExpectedMachineRequest {
            bmc_mac_address: settled
                .status
                .bmc_info
                .mac
                .expect("host BMC MAC")
                .to_string(),
            id: None,
        }))
        .await
        .expect("find declaration")
        .into_inner();
    // Change only boot intent so the public patch leaves the acknowledged network configuration intact.
    declaration
        .host_nics
        .iter_mut()
        .find(|interface| interface.primary == Some(true))
        .expect("original declared NIC")
        .mac_address = Some(changed_mac.to_string());
    // Hold the history trigger so unchanged repair commits before publication acquires its machine locks.
    let mut publication_gate = env.db_txn().await;
    db::state_history::persist(
        publication_gate.as_mut(),
        db::state_history::StateHistoryTableId::Machine,
        &host.id,
        &settled.state.value,
        settled.state.version.increment(),
    )
    .await
    .expect("gate HostInit publication after repair");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(publication_gate.as_mut())
        .await
        .expect("publication gate backend");

    // Commit a supported automatic-allocation patch while publication waits without changing either host version.
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::join!(
            env.run_machine_state_controller_iteration_without_requeue(),
            async {
                db::test_support::postgres::wait_for_blocked_query(
                    &env.pool,
                    blocker_pid,
                    "machine_state_history",
                )
                .await;
                env.api
                    .patch_expected_machine(Request::new(rpc::PatchExpectedMachineRequest {
                        expected_machine: Some(declaration),
                        update_mask: Some(prost_types::FieldMask {
                            paths: vec!["host_nics".to_string()],
                        }),
                    }))
                    .await
                    .expect("change boot declaration after repair");

                // Find the committed declaration before releasing publication so its final planner must observe it.
                let found = env
                    .api
                    .get_expected_machine(Request::new(rpc::ExpectedMachineRequest {
                        bmc_mac_address: settled
                            .status
                            .bmc_info
                            .mac
                            .expect("host BMC MAC")
                            .to_string(),
                        id: None,
                    }))
                    .await
                    .expect("find committed declaration")
                    .into_inner();
                assert_eq!(
                    found
                        .host_nics
                        .iter()
                        .find(|interface| interface.primary == Some(true))
                        .expect("committed declared primary")
                        .mac_address,
                    Some(changed_mac.to_string())
                );
                publication_gate
                    .rollback()
                    .await
                    .expect("release publication after declaration commits");
            }
        );
    })
    .await
    .expect("late declaration must not deadlock publication");

    // Find the selection-specific wait and original desired version so stale evidence cannot admit HostInit.
    let mut read = env.db_txn().await;
    let deferred = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close publication wait read");
    assert_eq!(deferred.state.value, settled.state.value);
    assert_eq!(deferred.state.version, settled.state.version);
    assert_eq!(
        deferred.network_config.version,
        settled.network_config.version
    );
    let deferred_target = deferred
        .config
        .desired_boot_interface
        .as_ref()
        .expect("deferred target");
    assert_eq!(deferred_target.value, original_target.value);
    assert_eq!(deferred_target.version, original_target.version);
    assert_eq!(
        deferred.config.boot_interface_selection,
        settled.config.boot_interface_selection
    );
    assert!(matches!(&deferred.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, source_ref: Some(source_ref) })
        if reason == "Host PF selection inputs changed or are busy before HostInit publication; reload"
            && source_ref.file.ends_with("/handler/host_pfs.rs")));

    // Let ordinary repair settle the changed declaration before a later pass can publish HostInit using it.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close changed target read");
    assert_eq!(repaired.state.value, settled.state.value);
    assert_eq!(repaired.state.version, settled.state.version);
    assert_eq!(
        repaired.network_config.version,
        settled.network_config.version
    );
    let repaired_target = repaired
        .config
        .desired_boot_interface
        .as_ref()
        .expect("changed target");
    assert_eq!(repaired_target.value.mac_address(), changed_mac);
    assert_eq!(
        repaired
            .config
            .boot_interface_selection
            .expect("declaration source")
            .source,
        BootInterfaceSelectionSource::ExpectedMachine
    );

    // Find the eventual HostInit transition so a permanent selection wait cannot satisfy recovery.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let admitted = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close HostInit read");
    assert!(matches!(
        admitted.state.value,
        ManagedHostState::HostInit {
            machine_state: model::machine::MachineState::EnableIpmiOverLan
        }
    ));
    let admitted_target = admitted
        .config
        .desired_boot_interface
        .as_ref()
        .expect("admitted target");
    assert_eq!(admitted_target.value, repaired_target.value);
    assert_eq!(admitted_target.version, repaired_target.version);
}

/// Holds a concurrent network writer behind the production handler's returned transaction until HostInit publication commits, preventing ACK validation from racing a later network version.
/// Declaration and host/DPU report writers must also remain blocked after the state CAS until the same transaction commits.
#[crate::sqlx_test]
async fn checkpoint_handler_retains_network_fence_until_publication(pool: sqlx::PgPool) {
    // Settle corrected PF networking and acknowledge it before calling the handler directly.
    let env = create_test_env(pool).await;
    let (host, hardware_info, interface_id) = checkpoint_host(&env, false, None).await;
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    let host_id: HostMachineId = host.id.into();
    let controller_io = MachineStateControllerIO {
        host_health: env.config.host_health,
        sla_config: model::machine::slas::MachineSlaConfig::new(
            env.config.machine_state_controller.failure_retry_time,
        ),
    };
    let mut read = env.db_txn().await;
    let mut snapshot = controller_io
        .load_object_state(read.as_mut(), &host_id)
        .await
        .expect("load acknowledged host")
        .expect("host snapshot");
    read.rollback().await.expect("close snapshot read");
    let controller_state = snapshot.host_snapshot.state.clone();
    let acknowledged_network = snapshot.host_snapshot.network_config.clone();
    let acknowledged_network_version = acknowledged_network.version;
    // The host report selects boot while each paired DPU report supplies pairing/order; every report is a publication input.
    let mut bmc_ips = snapshot
        .dpu_snapshots
        .iter()
        .map(|dpu| dpu.status.bmc_info.ip.expect("DPU BMC address"))
        .collect::<Vec<_>>();
    bmc_ips.push(
        snapshot
            .host_snapshot
            .status
            .bmc_info
            .ip
            .expect("host BMC address"),
    );
    let reports = db::explored_endpoints::find_by_ips(&env.pool, bmc_ips.clone())
        .await
        .expect("selection reports");
    assert_eq!(reports.len(), bmc_ips.len());
    let report_versions = reports
        .iter()
        .map(|endpoint| (endpoint.address, endpoint.report_version))
        .collect::<Vec<_>>();
    let declaration = env
        .api
        .get_expected_machine(Request::new(rpc::ExpectedMachineRequest {
            bmc_mac_address: snapshot
                .host_snapshot
                .status
                .bmc_info
                .mac
                .expect("host BMC MAC")
                .to_string(),
            id: None,
        }))
        .await
        .expect("find declaration")
        .into_inner();
    let declaration_id = declaration.id.clone();

    // Retain the exact transaction supplied by the production handler instead of constructing a fence in the test.
    let mut handler_services = env.machine_state_handler_services();
    let mut metrics = MachineMetrics::default();
    let mut pending_db_writes = DbWriteBatch::new();
    let mut ctx = StateHandlerContext {
        services: &mut handler_services,
        metrics: &mut metrics,
        pending_db_writes: &mut pending_db_writes,
    };
    let mut outcome = env
        .machine_state_handler
        .handle_object_state(&host_id, &mut snapshot, &controller_state.value, &mut ctx)
        .await
        .expect("admit acknowledged host");
    let next = ManagedHostState::HostInit {
        machine_state: model::machine::MachineState::EnableIpmiOverLan,
    };
    assert!(
        matches!(&outcome, StateHandlerOutcome::Transition { next_state, .. } if next_state == &next)
    );
    let mut publication = outcome
        .take_transaction()
        .expect("handler must return its publication fence");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(publication.as_mut())
        .await
        .expect("publication backend");

    // Attempt a real network-version update while the returned transaction still holds the validated host row.
    let writer_pool = env.pool.clone();
    let writer = tokio::spawn(async move {
        let mut txn = writer_pool.begin().await.expect("network writer");
        let result = db::machine::try_update_network_config(
            &mut txn,
            host_id.as_machine_id(),
            acknowledged_network.version,
            &acknowledged_network.value,
        )
        .await
        .expect("network update");
        txn.commit().await.expect("commit network update");
        result
    });
    db::test_support::postgres::wait_for_blocked_query(
        &env.pool,
        blocker_pid,
        "network_config_version",
    )
    .await;

    // Attempt public declaration mutation and production report writes while the returned transaction retains selection evidence.
    let writer_api = env.api.clone();
    let declaration_writer = tokio::spawn(async move {
        writer_api
            .patch_expected_machine(Request::new(rpc::PatchExpectedMachineRequest {
                expected_machine: Some(rpc::ExpectedMachine {
                    id: declaration.id,
                    metadata: Some(rpc::Metadata {
                        name: "after-publication".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                update_mask: Some(prost_types::FieldMask {
                    paths: vec!["metadata.name".to_string()],
                }),
            }))
            .await
            .expect("patch declaration after publication");
    });
    db::test_support::postgres::wait_for_blocked_query(&env.pool, blocker_pid, "expected_machines")
        .await;
    // Republish the captured host and DPU reports so both input row locks are exercised independently.
    let report_writers = reports
        .into_iter()
        .map(|endpoint| {
            let writer_pool = env.pool.clone();
            tokio::spawn(async move {
                let mut txn = writer_pool.begin().await.expect("report writer");
                let result = db::explored_endpoints::try_update(
                    endpoint.address,
                    endpoint.report_version,
                    &endpoint.report,
                    false,
                    &mut txn,
                )
                .await
                .expect("publish exploration report");
                txn.commit().await.expect("commit exploration report");
                result
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        db::test_support::postgres::wait_for_blocked_queries(
            &env.pool,
            blocker_pid,
            "UPDATE explored_endpoints",
            report_writers.len(),
        )
        .await,
        report_writers.len()
    );

    // Apply queued writes and publish through the returned transaction, matching the controller processor's commit order.
    let publication_version = controller_state.version.increment();
    pending_db_writes
        .apply_all(&mut publication)
        .await
        .expect("apply handler writes");
    assert_eq!(
        controller_io
            .persist_controller_state(
                publication.as_mut(),
                &host_id,
                controller_state.version,
                publication_version,
                &next,
            )
            .await
            .expect("publish HostInit"),
        db::ConditionalWrite::Applied(())
    );
    assert_eq!(
        db::test_support::postgres::count_blocked_queries(
            &env.pool,
            blocker_pid,
            "network_config_version",
        )
        .await,
        1
    );
    assert!(!writer.is_finished());
    assert!(!declaration_writer.is_finished());
    assert!(report_writers.iter().all(|writer| !writer.is_finished()));
    assert_eq!(
        db::test_support::postgres::count_blocked_queries(
            &env.pool,
            blocker_pid,
            "UPDATE explored_endpoints",
        )
        .await,
        report_writers.len()
    );
    publication
        .commit()
        .await
        .expect("commit HostInit publication");
    assert_eq!(
        writer.await.expect("writer task"),
        db::ConditionalWrite::Applied(())
    );
    declaration_writer.await.expect("declaration writer task");
    for writer in report_writers {
        assert_eq!(
            writer.await.expect("report writer task"),
            db::ConditionalWrite::Applied(())
        );
    }

    // Find committed selection inputs so blocked writers cannot merely disappear when publication releases its locks.
    let found = env
        .api
        .get_expected_machine(Request::new(rpc::ExpectedMachineRequest {
            id: declaration_id,
            bmc_mac_address: String::new(),
        }))
        .await
        .expect("find updated declaration")
        .into_inner();
    assert_eq!(
        found.metadata.expect("updated metadata").name,
        "after-publication"
    );
    let updated_reports = db::explored_endpoints::find_by_ips(&env.pool, bmc_ips)
        .await
        .expect("find updated reports");
    for (address, version) in report_versions {
        assert_eq!(
            updated_reports
                .iter()
                .find(|endpoint| endpoint.address == address)
                .expect("published report")
                .report_version,
            version.increment()
        );
    }

    // Reload both versions to prove state publication succeeded before the blocked writer advanced networking.
    let mut read = env.db_txn().await;
    let admitted = host.host().db_machine(&mut read).await;
    assert_eq!(admitted.state.value, next);
    assert_eq!(admitted.state.version, publication_version);
    assert_eq!(
        admitted.network_config.version.version_nr(),
        acknowledged_network_version.version_nr() + 1
    );
    read.rollback().await.expect("close publication read");
}

/// Keeps BF4 initialization in DPUInit when a blocking DPU health report commits after the repair snapshot, then admits HostInit after recovery so publication cannot trust stale health.
#[crate::sqlx_test]
async fn checkpoint_reloads_dpu_health_before_publication(pool: sqlx::PgPool) {
    // Settle corrected PF networking and acknowledge it with a healthy report before the publication attempt.
    let env = create_test_env(pool).await;
    let (host, hardware_info, interface_id) = checkpoint_host(&env, false, None).await;
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let dpu_id = host.dpu_ids[0];
    network_configured_with_health(&env, &dpu_id, None).await;

    // Find the settled state and clean report so a stale ACK or earlier alert cannot explain the later wait.
    let mut read = env.db_txn().await;
    let acknowledged = host.snapshot(&mut read).await;
    read.rollback().await.expect("close acknowledged read");
    assert!(
        matches!(&acknowledged.managed_state, ManagedHostState::DPUInit { dpu_states }
        if dpu_states.states.get(&dpu_id) == Some(&DpuInitState::WaitingForNetworkConfig))
    );
    assert!(acknowledged.managed_host_network_config_version_synced());
    assert!(
        acknowledged.dpu_snapshots[0]
            .dpu_agent_health_report()
            .expect("initial DPU health report")
            .alerts
            .is_empty()
    );
    let controller_state = acknowledged.host_snapshot.state;
    let network_version = acknowledged.host_snapshot.network_config.version;

    // Persist uncommitted history so its retention trigger lets repair commit before publication waits without locking machine rows.
    let mut publication_gate = env.db_txn().await;
    db::state_history::persist(
        publication_gate.as_mut(),
        db::state_history::StateHistoryTableId::Machine,
        &host.id,
        &controller_state.value,
        controller_state.version.increment(),
    )
    .await
    .expect("hold HostInit publication through the history trigger");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(publication_gate.as_mut())
        .await
        .expect("publication gate backend");

    // Commit the changed report after the repair snapshot and before the publication reload, bounding unexpected lock waits.
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::join!(
            env.run_machine_state_controller_iteration_without_requeue(),
            async {
                db::test_support::postgres::wait_for_blocked_query(
                    &env.pool,
                    blocker_pid,
                    "machine_state_history",
                )
                .await;

                // Preserve the matching network ACK while adding the lifecycle-blocking alert through the public API.
                let blocking_health = rpc::health::HealthReport {
                    source: "forge-dpu-agent".to_string(),
                    alerts: vec![rpc::health::HealthProbeAlert {
                        id: "ContainerExists".to_string(),
                        message: "HBN container not found".to_string(),
                        classifications: vec![
                            health_report::HealthAlertClassification::prevent_host_state_changes()
                                .to_string(), // This classification must prevent HostInit even with a current ACK.
                        ],
                        ..Default::default()
                    }],
                    ..Default::default()
                };
                network_configured_with_health(&env, &dpu_id, Some(blocking_health)).await;

                // Find the committed alert while publication is blocked and prove neither host version changed.
                let mut read = env.db_txn().await;
                let reported = host.snapshot(&mut read).await;
                read.rollback().await.expect("close changed health read");
                assert_eq!(
                    reported.host_snapshot.state.version,
                    controller_state.version
                );
                assert_eq!(
                    reported.host_snapshot.network_config.version,
                    network_version
                );
                assert!(reported.managed_host_network_config_version_synced());
                assert!(
                    reported.dpu_snapshots[0]
                        .dpu_agent_health_report()
                        .expect("committed blocking report")
                        .has_classification(
                            &health_report::HealthAlertClassification::prevent_host_state_changes()
                        )
                );
                // Release publication only after finding the committed alert so its reload must observe the changed health.
                publication_gate
                    .rollback()
                    .await
                    .expect("release HostInit publication");
            }
        );
    })
    .await
    .expect("publication reload and health reporting must complete");

    // Find the controller's persisted health wait so a generic busy wait cannot stand in for the publication reload.
    let mut read = env.db_txn().await;
    let waiting = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close health wait read");
    assert_eq!(waiting.state.value, controller_state.value);
    assert_eq!(waiting.state.version, controller_state.version);
    assert_eq!(waiting.network_config.version, network_version);
    assert!(
        matches!(&waiting.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, source_ref: Some(source_ref) })
        if reason.contains(&dpu_id.to_string())
            && reason.contains("check the DPU health alerts")
            && source_ref.file.ends_with("/handler/host_pfs.rs"))
    );

    // Clear the reported alert and retry through the ordinary sweep to prove healthy recovery needs no networking change.
    network_configured_with_health(&env, &dpu_id, None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let admitted = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close recovered host read");
    assert!(matches!(
        admitted.state.value,
        ManagedHostState::HostInit {
            machine_state: model::machine::MachineState::EnableIpmiOverLan
        }
    ));
    assert_eq!(admitted.network_config.version, network_version);
}

/// Excludes an expired retained ID from PF repair so HostInit cannot resurrect an obsolete Pair before Explorer's next sweep.
#[crate::sqlx_test]
async fn checkpoint_respects_retained_boot_interface_expiry(pool: sqlx::PgPool) {
    // Configure a finite window while the usual checkpoint fixture supplies a corrected MAC with no current report ID.
    let mut config = super::common::api_fixtures::get_config();
    config.retained_boot_interface_window = Some(chrono::Duration::hours(1));
    let env = super::common::api_fixtures::create_test_env_with_overrides(
        pool,
        super::common::api_fixtures::TestEnvOverrides::with_config(config),
    )
    .await;
    let (host, hardware_info, interface_id) = checkpoint_host(&env, false, None).await;
    let corrected_mac = hardware_info.dpu_info.as_ref().expect("DPU").host_pfs[0].mac_address;
    let mut setup = env.db_txn().await;
    db::retained_boot_interface::upsert(&mut setup, corrected_mac, "NIC.Obsolete.1")
        .await
        .expect("retain old ID");
    sqlx::query("UPDATE retained_boot_interfaces SET recorded_at = NOW() - INTERVAL '2 hours' WHERE mac_address = $1")
        .bind(corrected_mac)
        .execute(setup.as_mut())
        .await.expect("backdate retained ID beyond policy");
    assert_eq!(
        db::retained_boot_interface::find_by_mac(&mut setup, corrected_mac, None)
            .await
            .expect("retained record before repair")
            .as_deref(),
        Some("NIC.Obsolete.1"),
    );
    setup.commit().await.expect("commit expired observation");

    // Repair and acknowledge through the real controller without running an Explorer expiry sweep.
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    // Reload the desired target and corrected row, then prove boot resolution remains MacOnly.
    let mut read = env.db_txn().await;
    let snapshot = host.snapshot(&mut read).await;
    assert!(matches!(
        snapshot.managed_state,
        ManagedHostState::HostInit { .. }
    ));
    assert_eq!(
        snapshot
            .host_snapshot
            .config
            .desired_boot_interface
            .as_ref()
            .expect("desired boot target")
            .value,
        MachineBootInterfaceTarget::MacOnly(corrected_mac)
    );
    let corrected = snapshot
        .host_snapshot
        .status
        .interfaces
        .iter()
        .find(|row| row.mac_address == corrected_mac)
        .expect("corrected PF row");
    assert!(corrected.boot_interface_id.is_none());
    assert_eq!(
        carbide_machine_controller::boot_interface::boot_interface_target(&snapshot, &[]),
        Some(carbide_redfish::boot_interface::BootInterfaceTarget::MacOnly(corrected_mac)),
    );
    assert!(
        db::retained_boot_interface::find_by_mac(&mut read, corrected_mac, None)
            .await
            .expect("consumed retained record")
            .is_none(),
    );
    read.rollback().await.expect("close read");
}

/// Applies the same checkpoint after API-first bootstrap and later Explorer association so discovery order cannot restore the provisional factory-MAC target.
#[crate::sqlx_test]
async fn api_first_checkpoint_uses_registered_pf_after_exploration(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let (host, hardware_info, _) = checkpoint_host(&env, true, None).await;
    let corrected_mac = hardware_info.dpu_info.as_ref().expect("DPU").host_pfs[0].mac_address;

    // Process only the checkpoint so the provisional owned link cannot authorize immediate HostInit.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");

    // Find the host again through the public API and prove the registered PF replaced the factory bootstrap link.
    assert!(matches!(
        repaired.state.value,
        ManagedHostState::DPUInit { .. }
    ));
    assert!(matches!(
        repaired.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { .. })
    ));
    let found = host.host().rpc_machine().await;
    let primary = found
        .status
        .expect("host status")
        .interfaces
        .into_iter()
        .find(|interface| interface.primary_interface)
        .expect("primary");
    assert_eq!(primary.mac_address, corrected_mac.to_string());
    assert_eq!(primary.attached_dpu_machine_id, Some(host.dpu_ids[0]));
    assert_eq!(
        repaired
            .config
            .desired_boot_interface
            .expect("corrected boot target")
            .value,
        MachineBootInterfaceTarget::MacOnly(corrected_mac)
    );
}

/// Rejects both implicit admin boot paths with absent or automatic intent during pending BF4 initialization while explicit MAC selection and no-MAC reapply preserve independent Operator intent.
#[crate::sqlx_test]
async fn pending_admin_boot_requires_explicit_intent(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let (host, mut hardware_info, interface_id) = checkpoint_host(&env, false, None).await;
    // Persist authenticated BF4 classification even though selected PF acquisition still needs a retry.
    hardware_info
        .dpu_info
        .as_mut()
        .expect("DPU hardware")
        .host_pfs
        .clear();
    let pending = register_facts(&env, interface_id, hardware_info)
        .await
        .expect_err("missing selected PF");
    assert_eq!(pending.code(), Code::FailedPrecondition);
    // Preserve the preclassification automatic target for the first pair of requests.
    let mut read = env.db_txn().await;
    let automatic = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close automatic read");
    assert!(automatic.config.desired_boot_interface.is_some());
    assert!(!matches!(
        automatic
            .config
            .boot_interface_selection
            .expect("automatic source")
            .source,
        BootInterfaceSelectionSource::Operator | BootInterfaceSelectionSource::ExpectedMachine
    ));

    for (scenario, keep_automatic) in [
        // Existing provisional intent must not authorize either implicit admin operation.
        ("existing automatic provisional intent", true),
        // Removing intent retains the original fallback-rejection and Explorer deferral proof.
        ("absent intent after Explorer refresh", false),
    ] {
        if !keep_automatic {
            // Refresh older Explorer hardware after deletion so it cannot republish a provisional target.
            sqlx::query("DELETE FROM machine_boot_interfaces WHERE machine_id = $1")
                .bind(host.id)
                .execute(&env.pool)
                .await
                .expect("unset provisional intent");
            env.run_site_explorer_iteration().await;
        }
        let mut read = env.db_txn().await;
        let before = host.host().db_machine(&mut read).await;
        read.rollback().await.expect("close before read");
        assert_eq!(
            before.config.desired_boot_interface.is_some(),
            keep_automatic,
            "{scenario}"
        );
        let timepoint = env.redfish_sim.timepoint();

        for (command, setup) in [
            // MachineSetup must not fall through to raw managed BIOS work.
            ("MachineSetup", true),
            // Boot-order-only requests must not promote automatic intent to Operator authority.
            ("SetDpuFirstBootOrder", false),
        ] {
            // Exercise the public no-MAC command while the selected fixture's desired state remains persisted.
            let result = if setup {
                env.api
                    .machine_setup(Request::new(rpc::forge::MachineSetupRequest {
                        machine_id: Some(host.id.to_string()),
                        bmc_endpoint_request: None,
                        boot_interface_mac: None,
                    }))
                    .await
                    .map(drop)
            } else {
                env.api
                    .set_dpu_first_boot_order(Request::new(
                        rpc::forge::SetDpuFirstBootOrderRequest {
                            machine_id: Some(host.id.to_string()),
                            bmc_endpoint_request: None,
                            boot_interface_mac: None,
                        },
                    ))
                    .await
                    .map(drop)
            };
            let error = result.expect_err("initialization is pending");
            assert_eq!(
                error.code(),
                Code::FailedPrecondition,
                "{scenario}: {command}"
            );
            assert!(error.message().contains("initialization is pending"));

            // Reload the desired target/version, source and decision time after each rejection to detect premature publication.
            let mut read = env.db_txn().await;
            let after = host.host().db_machine(&mut read).await;
            read.rollback().await.expect("close after read");
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
                    .map(|desired| (&desired.value, desired.version)),
                "{scenario}: {command}"
            );
            assert_eq!(
                after.config.boot_interface_selection, before.config.boot_interface_selection,
                "{scenario}: {command}"
            );
        }
        assert!(
            env.redfish_sim
                .actions_since(&timepoint)
                .all_hosts()
                .is_empty(),
            "{scenario}"
        );
    }

    // Explicit intent can be recorded before PF acquisition, and omission later reapplies that same selection.
    let explicit_mac = MacAddress::new([2, 0, 0, 0xfa, 0, 9]);
    env.api
        .set_dpu_first_boot_order(Request::new(rpc::forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(host.id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: Some(explicit_mac.to_string()),
        }))
        .await
        .expect("explicit boot target");
    let mut read = env.db_txn().await;
    let boot_order_selected = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    assert_eq!(
        boot_order_selected
            .config
            .desired_boot_interface
            .expect("boot-order selection")
            .value,
        MachineBootInterfaceTarget::MacOnly(explicit_mac)
    );

    // Select a different MAC through MachineSetup so its pending-initialization bypass cannot rely on existing Operator intent alone.
    let setup_mac = MacAddress::new([2, 0, 0, 0xfa, 0, 10]);
    env.api
        .machine_setup(Request::new(rpc::forge::MachineSetupRequest {
            machine_id: Some(host.id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: Some(setup_mac.to_string()),
        }))
        .await
        .expect("explicit setup target while PF acquisition is pending");
    let mut read = env.db_txn().await;
    let selected = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close setup selection read");
    assert_eq!(
        selected
            .config
            .desired_boot_interface
            .as_ref()
            .expect("setup selection")
            .value,
        MachineBootInterfaceTarget::MacOnly(setup_mac)
    );

    // Reapply without a MAC after finding MachineSetup's persisted Operator selection.
    env.api
        .machine_setup(Request::new(rpc::forge::MachineSetupRequest {
            machine_id: Some(host.id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await
        .expect("reapply explicit intent");
    let mut read = env.db_txn().await;
    let reapplied = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");

    // Find persisted selection metadata so reapply cannot silently replace Operator provenance or its timestamp.
    assert_eq!(
        selected
            .config
            .boot_interface_selection
            .expect("selected source")
            .source,
        BootInterfaceSelectionSource::Operator
    );
    assert_eq!(
        reapplied.config.boot_interface_selection,
        selected.config.boot_interface_selection
    );
    assert_eq!(
        reapplied
            .config
            .desired_boot_interface
            .expect("desired")
            .value,
        MachineBootInterfaceTarget::MacOnly(setup_mac)
    );
}

/// Boots a declared NIC before its first lease and keeps DPU networking selected when DHCP later adopts it, preventing boot predictions from becoming networking authority.
/// A pending BF4 host also permits no-MAC reapply of that ExpectedMachine target without replacing the declaration's source or decision time.
#[crate::sqlx_test]
async fn declared_integrated_nic_boots_before_its_first_lease(pool: sqlx::PgPool) {
    let env = super::common::api_fixtures::create_test_env_with_host_inband(pool).await;
    let declared_mac = MacAddress::new([2, 0, 0, 0xfa, 0, 8]);
    let (host, hardware_info, interface_id) =
        checkpoint_host(&env, false, Some(declared_mac)).await;

    // A declaration cannot bypass managed-DPU readiness while authenticated PF facts are missing.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let waiting = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    assert!(
        matches!(waiting.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason.contains("c1pf0"))
    );

    // Finalize registered PF networking and acknowledge it before the separate HostInit admission pass.
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    // Keep the fresh ACK outstanding so a no-MAC request must admit ExpectedMachine intent during pending BF4 initialization.
    let mut read = env.db_txn().await;
    let declared = host.host().db_machine(&mut read).await;
    assert!(
        db::machine::bf4_host_pf_initialization_pending(&mut read, &host.id.into())
            .await
            .expect("pending declared boot selection")
    );
    let declared_selection = declared
        .config
        .boot_interface_selection
        .expect("declared source");
    assert_eq!(
        declared_selection.source,
        BootInterfaceSelectionSource::ExpectedMachine
    );
    let declared_target = declared
        .config
        .desired_boot_interface
        .expect("declared target");
    read.rollback()
        .await
        .expect("close declared selection read");
    env.api
        .machine_setup(Request::new(rpc::forge::MachineSetupRequest {
            machine_id: Some(host.id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await
        .expect("reapply pending ExpectedMachine target");

    // Reload the forced generation to prove reapply retains the declaration's target, source and decision time.
    let mut read = env.db_txn().await;
    let reapplied = host.host().db_machine(&mut read).await;
    let reapplied_target = reapplied
        .config
        .desired_boot_interface
        .expect("reapplied declaration");
    assert_eq!(reapplied_target.value, declared_target.value);
    assert_ne!(reapplied_target.version, declared_target.version);
    assert_eq!(
        reapplied.config.boot_interface_selection,
        Some(declared_selection)
    );
    assert_eq!(
        reapplied.network_config.version,
        declared.network_config.version
    );
    read.rollback()
        .await
        .expect("close declaration reapply read");
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    // Reload persisted prediction and controller state; no DHCP-created row may be needed to target the declared NIC.
    let mut read = env.db_txn().await;
    let host_id: carbide_uuid::machine::HostMachineId = host.id.into();
    let snapshot = db::managed_host::load_snapshot(
        &mut *read,
        host_id.as_machine_id(),
        model::machine::LoadSnapshotOptions::default(),
    )
    .await
    .expect("managed snapshot")
    .expect("host");
    let predictions =
        db::predicted_machine_interface::find_by_machine_id(&mut read, host_id.as_machine_id())
            .await
            .expect("predictions");
    assert!(matches!(
        snapshot.managed_state,
        ManagedHostState::HostInit { .. }
    ));
    assert!(
        db::machine_interface::find_by_mac_address(&mut *read, declared_mac)
            .await
            .expect("declared interfaces")
            .is_empty()
    );
    let target =
        carbide_machine_controller::boot_interface::boot_interface_target(&snapshot, &predictions)
            .expect("initial boot target");
    assert!(
        matches!(target, carbide_redfish::boot_interface::BootInterfaceTarget::Pair(pair)
        if pair.mac_address == declared_mac && pair.interface_id.starts_with("NIC.Embedded."))
    );
    let network_primary = snapshot
        .host_snapshot
        .status
        .interfaces
        .iter()
        .find(|row| row.primary_interface)
        .expect("HBN primary")
        .clone();
    assert!(network_primary.attached_dpu_machine_id.is_some());
    assert!(!network_primary.addresses.is_empty());
    assert!(
        !predictions
            .iter()
            .find(|row| row.mac_address == declared_mac)
            .expect("boot prediction")
            .primary_interface
    );
    read.rollback().await.expect("close read");

    // Lease after HostInit through ordinary prediction promotion, without changing any flags in the fixture.
    env.api
        .discover_dhcp(
            rpc::forge::DhcpDiscovery::builder(declared_mac, "192.0.3.1").tonic_request(),
        )
        .await
        .expect("late boot-NIC lease");
    let mut read = env.db_txn().await;
    let leased = host.host().db_machine(&mut read).await;
    let nic = db::machine_interface::find_by_mac_address(&mut *read, declared_mac)
        .await
        .expect("leased NIC")
        .remove(0);

    // Reload both decisions so a late lease cannot silently move networking onto the separate boot NIC.
    assert!(!nic.primary_interface);
    assert!(!nic.addresses.is_empty());
    let after_primary = leased
        .status
        .interfaces
        .iter()
        .find(|row| row.primary_interface)
        .expect("retained primary");
    assert_eq!(after_primary.id, network_primary.id);
    assert_eq!(after_primary.addresses, network_primary.addresses);
    assert_eq!(
        leased.config.primary_interface_selection,
        snapshot.host_snapshot.config.primary_interface_selection
    );
    let before_boot = snapshot
        .host_snapshot
        .config
        .desired_boot_interface
        .as_ref()
        .expect("boot intent before lease");
    let after_boot = leased
        .config
        .desired_boot_interface
        .as_ref()
        .expect("boot intent after lease");
    assert_eq!(after_boot.value, before_boot.value);
    assert_eq!(after_boot.version, before_boot.version);
    assert_eq!(
        leased.config.boot_interface_selection,
        snapshot.host_snapshot.config.boot_interface_selection
    );
    read.rollback().await.expect("close late lease read");
    // Fetch and acknowledge real agent networking again to detect late loss of its required primary Admin interface.
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
}

/// Preserves the previously Ready primary DPU and its networking source and decision time after the controller reloads a history-free polling snapshot, even when corrected PF facts make another DPU the report-selected candidate.
#[crate::sqlx_test]
async fn reinitialization_loads_ready_history_before_preserving_primary_dpu(pool: sqlx::PgPool) {
    use std::collections::HashMap;

    // Start from a working legacy host so actual persisted Ready history, rather than stable identity, qualifies preservation.
    let env = create_test_env(pool).await;
    let host = super::common::api_fixtures::create_managed_host_multi_dpu(&env, 2).await;
    let mut read = env.db_txn().await;
    let current_host = host.host().db_machine(&mut read).await;
    let original_primary_selection = current_host
        .config
        .primary_interface_selection
        .expect("working networking selection");
    assert_ne!(
        original_primary_selection.source,
        BootInterfaceSelectionSource::Operator
    );
    assert!(original_primary_selection.updated_at.is_some());
    let primary_dpu = current_host
        .primary_attached_dpu_machine_id()
        .expect("working primary DPU");
    let primary_index = host
        .dpu_ids
        .iter()
        .position(|id| *id == primary_dpu)
        .expect("primary DPU snapshot");
    let dpu = host.dpu_n(primary_index).db_machine(&mut read).await;
    let interface_id = dpu
        .status
        .interfaces
        .iter()
        .find(|row| row.primary_interface)
        .expect("DPU OOB")
        .id;
    let mut hardware_info = dpu.status.hardware_info.expect("DPU hardware");
    read.rollback().await.expect("close read");
    assert_ne!(
        current_host
            .config
            .boot_interface_selection
            .expect("automatic source")
            .source,
        BootInterfaceSelectionSource::Operator
    );
    // Give the old primary a corrected PF MAC absent from the host UEFI report, leaving the other DPU as the report-selected candidate.
    let corrected_mac = "02:00:00:fa:00:02".parse().expect("corrected PF MAC");
    let dpu_info = hardware_info.dpu_info.as_mut().expect("DPU data");
    dpu_info.part_number = "900-9D4B4-CWAA-TSA".to_string();
    dpu_info.host_pfs = vec![model::hardware_info::DpuHostPf {
        pci_controller_id: 1,
        pf_id: 0,
        mac_address: corrected_mac,
    }];

    // Register corrected facts while Ready, then deliberately reenter initialization without the validation-time shortcut.
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    let mut setup = env.db_txn().await;
    sqlx::query("UPDATE machines SET last_machine_validation_time = NULL WHERE id = $1")
        .bind(host.id)
        .execute(setup.as_mut())
        .await
        .expect("isolate Ready history");
    db::machine::update_state(
        &mut setup,
        &host.id.into(),
        &ManagedHostState::DPUInit {
            dpu_states: model::machine::DpuInitStates {
                states: host
                    .dpu_ids
                    .iter()
                    .copied()
                    .map(|id| (id, DpuInitState::WaitingForNetworkConfig))
                    .collect::<HashMap<_, _>>(),
            },
        },
    )
    .await
    .expect("reenter initialization");
    setup.commit().await.expect("commit initialization");

    // A normal controller iteration starts without history; the repair must reload it under its locks.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    // Reload persisted state and confirm the old DPU identity survived while its provisional MAC changed.
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    let primary = repaired
        .status
        .interfaces
        .iter()
        .find(|row| row.primary_interface)
        .expect("repaired primary");
    assert_eq!(primary.attached_dpu_machine_id, Some(primary_dpu));
    assert_eq!(primary.mac_address, corrected_mac);
    assert_eq!(
        repaired.config.primary_interface_selection,
        Some(original_primary_selection)
    );
    assert_ne!(
        repaired.network_config.version,
        current_host.network_config.version
    );
    assert!(
        matches!(repaired.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. })
        if reason.contains("finalized") && reason.contains("reload"))
    );
}

/// Honors Operator boot intent over an owned declared NIC during HostInit while preserving the selected DPU's corrected networking and provenance.
#[crate::sqlx_test]
async fn operator_boot_selection_overrides_owned_declared_nic(pool: sqlx::PgPool) {
    use carbide_redfish::libredfish::test_support::RedfishSimAction;
    use model::machine::{MachineState, SetBootOrderInfo, SetBootOrderState};

    // Supply an in-band segment so the declared NIC can obtain its own row through ordinary DHCP promotion.
    let env = super::common::api_fixtures::create_test_env_with_host_inband(pool).await;
    let declared_mac = MacAddress::new([2, 0, 0, 0xfa, 0, 8]);
    let (host, hardware_info, interface_id) =
        checkpoint_host(&env, false, Some(declared_mac)).await;
    let corrected_mac = hardware_info.dpu_info.as_ref().expect("DPU").host_pfs[0].mac_address;
    // Observe the production-created boot-only prediction before ordinary DHCP adoption.
    let mut setup = env.db_txn().await;
    let declared_prediction =
        db::predicted_machine_interface::find_by_mac_address(&mut setup, declared_mac)
            .await
            .expect("declared prediction lookup")
            .expect("declared prediction");
    assert!(!declared_prediction.primary_interface);
    setup.rollback().await.expect("close prediction read");
    let leased = env
        .api
        .discover_dhcp(
            rpc::forge::DhcpDiscovery::builder(declared_mac, "192.0.3.1").tonic_request(),
        )
        .await
        .expect("lease declared NIC")
        .into_inner();
    let declared_interface = leased.machine_interface_id.expect("owned declared NIC");

    // Seed the legacy Operator primary baseline because this predicted-ID fixture cannot use the stable-only primary API.
    let mut setup = env.db_txn().await;
    db::machine_interface::set_primary_interface_selection(
        &mut setup,
        &host.id,
        BootInterfaceSelectionSource::Operator,
        true,
    )
    .await
    .expect("record legacy primary choice");
    setup.commit().await.expect("commit primary provenance");

    // Record independent Operator boot intent through the public API without changing ExpectedMachine's declaration.
    let host_id: carbide_uuid::machine::HostMachineId = host.id.into();
    env.api
        .set_dpu_first_boot_order(Request::new(rpc::forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(host.id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: Some(corrected_mac.to_string()),
        }))
        .await
        .expect("record Operator boot intent");
    // Model an older owned row without an ID so finalization must capture the declared NIC's own report-resolved ID.
    sqlx::query("UPDATE machine_interfaces SET boot_interface_id = NULL WHERE id = $1")
        .bind(declared_interface)
        .execute(&env.pool)
        .await
        .expect("clear uncaptured ID");
    let mut read = env.db_txn().await;
    let operator_selected = host.host().db_machine(&mut read).await;
    let primary_selection = operator_selected
        .config
        .primary_interface_selection
        .expect("Operator primary source");
    let operator_target = operator_selected
        .config
        .desired_boot_interface
        .expect("Operator target");
    let operator_selection = operator_selected
        .config
        .boot_interface_selection
        .expect("Operator source");
    assert_eq!(
        operator_selection.source,
        BootInterfaceSelectionSource::Operator
    );
    read.rollback().await.expect("close read");

    // Publish corrected PF networking before acknowledging it; finalization must preserve DPU identity and Operator intent.
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let found = host.host().rpc_machine().await;
    let primary = found
        .status
        .expect("host status")
        .interfaces
        .into_iter()
        .find(|row| row.primary_interface)
        .expect("primary interface");
    assert_eq!(primary.attached_dpu_machine_id, Some(host.dpu_ids[0]));
    assert_eq!(primary.mac_address, corrected_mac.to_string());
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    let declared = db::machine_interface::find_one(&mut *read, declared_interface)
        .await
        .expect("declared NIC");
    assert_eq!(declared.machine_id, Some(host_id.into()));
    assert!(!declared.primary_interface);
    assert!(
        declared
            .boot_interface_id
            .as_deref()
            .expect("captured declared ID")
            .starts_with("NIC.Embedded.")
    );
    assert!(
        db::predicted_machine_interface::find_by_mac_address(&mut read, declared_mac)
            .await
            .expect("consumed prediction")
            .is_none()
    );
    let repaired_target = repaired
        .config
        .desired_boot_interface
        .expect("retained Operator target");
    assert_eq!(repaired_target.value, operator_target.value);
    assert_eq!(repaired_target.version, operator_target.version);
    assert_eq!(
        repaired.config.boot_interface_selection,
        Some(operator_selection)
    );
    // Correcting this DPU's PF must retain networking provenance even when ExpectedMachine declares another NIC.
    assert_eq!(
        repaired.config.primary_interface_selection,
        Some(primary_selection)
    );
    assert!(
        matches!(repaired.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. })
        if reason.contains("finalized") && reason.contains("reload"))
    );
    read.rollback().await.expect("close read");

    // Admit HostInit only after acknowledging the repaired version, then isolate its boot-order actuator from unrelated initial setup.
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut setup = env.db_txn().await;
    let admitted = host.host().db_machine(&mut setup).await;
    assert!(matches!(
        admitted.state.value,
        ManagedHostState::HostInit { .. }
    ));
    db::machine::update_state(
        &mut setup,
        &host_id,
        &ManagedHostState::HostInit {
            machine_state: MachineState::SetBootOrder {
                set_boot_order_info: Some(SetBootOrderInfo {
                    set_boot_order_jid: None,
                    set_boot_order_state: SetBootOrderState::SetBootOrder,
                    retry_count: 0,
                }),
            },
        },
    )
    .await
    .expect("isolate initial boot-order step");
    setup
        .commit()
        .await
        .expect("commit initial boot-order state");
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    env.redfish_sim.set_is_bios_setup(true);
    env.redfish_sim.set_is_boot_order_setup(false);
    env.redfish_sim
        .set_lockdown(libredfish::EnabledDisabled::Disabled);
    let timepoint = env.redfish_sim.timepoint();

    // Exercise the runtime resolver through a real initial boot-order controller step with no remaining prediction.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    // Observe the stored Operator target and persisted progress even though the declared NIC remains owned with an ID.
    let expected_mac = corrected_mac.to_string();
    assert!(env.redfish_sim.actions_since(&timepoint).all_hosts().iter().any(|action|
        matches!(action, RedfishSimAction::SetBootOrderDpuFirst { boot_interface_mac } if boot_interface_mac == &expected_mac)
    ));
    let mut read = env.db_txn().await;
    let booting = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    assert!(matches!(
        booting.state.value,
        ManagedHostState::HostInit {
            machine_state: MachineState::SetBootOrder {
                set_boot_order_info: Some(SetBootOrderInfo {
                    set_boot_order_state: SetBootOrderState::WaitForSetBootOrderJobScheduled,
                    ..
                })
            }
        }
    ));
    assert_eq!(
        booting.primary_attached_dpu_machine_id(),
        Some(host.dpu_ids[0])
    );
    let booting_target = booting
        .config
        .desired_boot_interface
        .expect("retained Operator target");
    assert_eq!(booting_target.value, operator_target.value);
    assert_eq!(booting_target.version, operator_target.version);
    assert_eq!(
        booting.config.boot_interface_selection,
        Some(operator_selection)
    );
}

/// Withholds API-first automatic boot publication until PF finalization, so a later repair cannot mask premature Site Explorer authority over the factory bootstrap MAC.
#[crate::sqlx_test]
async fn api_first_exploration_withholds_automatic_boot_intent_before_checkpoint(
    pool: sqlx::PgPool,
) {
    // Register corrected facts before exploration and stop before any controller finalization or network ACK.
    let env = create_test_env(pool).await;
    let (explored, hardware_info) = explore_checkpoint_host(&env, true, None).await;
    let dpu_id = explored.dpu_machine_ids[&0];
    let mut read = env.db_txn().await;
    let host_id = db::machine::find_host_by_dpu_machine_id(&mut read, &dpu_id)
        .await
        .expect("host lookup")
        .expect("paired host")
        .id;
    let host = db::machine::find_one(&mut *read, &host_id, Default::default())
        .await
        .expect("host read")
        .expect("host");
    let topology = db::machine_topology::find_latest_by_machine_ids(&mut read, &[dpu_id])
        .await
        .expect("DPU topology")
        .remove(&dpu_id)
        .expect("registered topology");

    // Prove complete facts coexist with provisional networking; no finalization has had a chance to rewrite boot intent.
    assert_eq!(
        topology
            .dpu_host_pf_inventory
            .expect("authenticated inventory")
            .host_pfs,
        hardware_info.dpu_info.as_ref().expect("DPU facts").host_pfs
    );
    assert!(
        db::machine::bf4_host_pf_initialization_pending(&mut read, &host_id)
            .await
            .expect("pending classification")
    );
    assert_eq!(
        host.status
            .interfaces
            .iter()
            .find(|row| row.primary_interface)
            .expect("bootstrap primary")
            .mac_address,
        hardware_info.factory_mac_address().expect("factory MAC")
    );
    assert!(host.config.desired_boot_interface.is_none());
    assert!(host.config.boot_interface_selection.is_none());
    read.rollback().await.expect("close observation");
}

/// Persists host-only BF4 evidence and adopts a declared NIC leased before BF4 ingestion without selecting it for networking, then renders corrected HBN configuration and reaches HostInit after a fresh ACK.
/// Re-exploration may refresh host BF4 inventory without backfilling an existing DPU's provisional hardware or fabricating authenticated PF facts.
#[crate::sqlx_test]
async fn leased_declared_boot_nic_keeps_automatic_bf4_hbn_networking(pool: sqlx::PgPool) {
    // Lease the NIC before ExpectedMachine ingestion so Explorer must adopt an anonymous owned-address row.
    let env = super::common::api_fixtures::create_test_env_with_host_inband(pool).await;
    let declared_mac = MacAddress::new([2, 0, 0, 0xfa, 0, 8]);
    let lease = env
        .api
        .discover_dhcp(
            rpc::forge::DhcpDiscovery::builder(declared_mac, "192.0.3.1").tonic_request(),
        )
        .await
        .expect("anonymous boot-NIC lease")
        .into_inner();
    let nic_id = lease.machine_interface_id.expect("leased interface");
    let mut read = env.db_txn().await;
    let anonymous = db::machine_interface::find_one(&mut *read, nic_id)
        .await
        .expect("anonymous NIC");
    assert!(anonymous.machine_id.is_none());
    assert!(!anonymous.addresses.is_empty());
    read.rollback().await.expect("close anonymous read");
    let (explored, hardware_info) = explore_checkpoint_host(&env, false, Some(declared_mac)).await;
    let dpu_id = explored.dpu_machine_ids[&0];
    let host_bmc_ip = explored.host_bmc_ip.expect("host BMC address");
    let pairing_serial = &explored.managed_host.get_and_assert_single_dpu().serial;

    // Inspect Explorer's committed host-only BF4 evidence before controller setup or the fixture's late-VPD rewrite.
    let mut read = env.db_txn().await;
    let provisional_host = db::machine::find_host_by_dpu_machine_id(&mut read, &dpu_id)
        .await
        .expect("provisional host lookup")
        .expect("paired host");
    let topology = db::machine_topology::find_latest_by_machine_ids(&mut read, &[dpu_id])
        .await
        .expect("Explorer topology")
        .remove(&dpu_id)
        .expect("DPU topology");
    let provisional_hardware = &topology.topology.discovery_data.info;
    assert!(provisional_hardware.is_bf4_dpu());
    let provisional_part = provisional_hardware
        .dpu_info
        .as_ref()
        .expect("provisional DPU hardware")
        .part_number
        .clone();
    assert_eq!(provisional_part, "900-9D4B4-CWAA-TSA");
    assert!(
        provisional_hardware
            .dpu_info
            .as_ref()
            .expect("provisional DPU hardware")
            .host_pfs
            .is_empty()
    );
    assert!(topology.dpu_host_pf_inventory.is_none());
    assert!(
        db::machine::bf4_host_pf_initialization_pending(&mut read, &provisional_host.id)
            .await
            .expect("pending Explorer PF facts")
    );
    assert!(provisional_host.config.desired_boot_interface.is_none());
    let mut endpoint = db::explored_endpoints::find_by_ips(read.as_mut(), vec![host_bmc_ip])
        .await
        .expect("host report")
        .remove(0);
    read.rollback().await.expect("close Explorer observation");

    // Re-explore changed host BF4 inventory after the DPU exists so report refresh cannot backfill its topology.
    let changed_part = "900-9D4B4-CWAA-TSB";
    endpoint
        .report
        .systems
        .iter_mut()
        .flat_map(|system| &mut system.pcie_devices)
        .find(|device| {
            device.serial_number.as_deref().map(str::trim) == Some(pairing_serial.as_str())
        })
        .expect("serial-matched host inventory")
        .part_number = Some(changed_part.to_string());
    env.endpoint_explorer
        .insert_endpoints(vec![(host_bmc_ip, endpoint.report)]);
    env.api
        .re_explore_endpoint(Request::new(rpc::forge::ReExploreEndpointRequest {
            ip_address: host_bmc_ip.to_string(),
            if_version_match: None,
        }))
        .await
        .expect("request changed host exploration");
    env.run_site_explorer_iteration().await;

    // Reload both stores to prove the report changed while existing DPU hardware and authenticated-inventory absence stayed intact.
    let mut read = env.db_txn().await;
    let refreshed = db::explored_endpoints::find_by_ips(read.as_mut(), vec![host_bmc_ip])
        .await
        .expect("refreshed host report")
        .remove(0);
    assert_eq!(
        refreshed
            .report
            .systems
            .iter()
            .flat_map(|system| &system.pcie_devices)
            .find(|device| device.serial_number.as_deref().map(str::trim)
                == Some(pairing_serial.as_str()))
            .expect("refreshed host inventory")
            .part_number
            .as_deref(),
        Some(changed_part)
    );
    let topology = db::machine_topology::find_latest_by_machine_ids(&mut read, &[dpu_id])
        .await
        .expect("existing DPU topology")
        .remove(&dpu_id)
        .expect("DPU topology");
    assert_eq!(
        topology
            .topology
            .discovery_data
            .info
            .dpu_info
            .expect("retained provisional hardware")
            .part_number,
        provisional_part
    );
    assert!(topology.dpu_host_pf_inventory.is_none());
    read.rollback()
        .await
        .expect("close re-exploration observation");
    let (host, hardware_info, interface_id) =
        acknowledge_checkpoint_host(&env, explored, hardware_info, false).await;
    let host_id: carbide_uuid::machine::HostMachineId = host.id.into();
    let corrected_mac = hardware_info
        .dpu_info
        .as_ref()
        .expect("registered PF")
        .host_pfs[0]
        .mac_address;

    // Reload actual history and provenance to exclude the Operator and previously-working preservation branches.
    let mut read = env.db_txn().await;
    let before = db::machine::find_one(
        &mut *read,
        &host_id,
        model::machine::machine_search_config::MachineSearchConfig {
            include_history: true,
            ..Default::default()
        },
    )
    .await
    .expect("initial host read")
    .expect("initial host");
    assert!(before.last_machine_validation_time.is_none());
    assert!(!before.history.is_empty());
    assert!(before.history.iter().all(|record| !matches!(
        serde_json::from_str::<ManagedHostState>(&record.state).expect("history state"),
        ManagedHostState::Ready
            | ManagedHostState::Assigned { .. }
            | ManagedHostState::BootConfiguring { .. }
    )));
    assert!(
        before
            .config
            .primary_interface_selection
            .is_none_or(|selection| selection.source != BootInterfaceSelectionSource::Operator)
    );
    let adopted = db::machine_interface::find_one(&mut *read, nic_id)
        .await
        .expect("adopted boot NIC");
    assert_eq!(adopted.machine_id, Some(host_id.into()));
    assert!(!adopted.primary_interface);
    assert_eq!(adopted.addresses, anonymous.addresses);
    read.rollback().await.expect("close initial read");

    // Finalize the authenticated PF while the separate declared NIC is already leased and owned.
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register PF facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    let primary = repaired
        .status
        .interfaces
        .iter()
        .find(|row| row.primary_interface)
        .expect("HBN primary");
    assert_eq!(primary.mac_address, corrected_mac);
    assert_eq!(primary.attached_dpu_machine_id, Some(host.dpu_ids[0]));
    assert_eq!(
        primary.network_segment_type,
        Some(model::network_segment::NetworkSegmentType::Admin)
    );
    let admin_ip = primary
        .addresses
        .iter()
        .find(|ip| ip.is_ipv4())
        .expect("HBN admin address")
        .to_string();
    assert_eq!(
        repaired
            .config
            .primary_interface_selection
            .expect("automatic networking source")
            .source,
        BootInterfaceSelectionSource::RedfishSerialNumber
    );
    let desired = repaired
        .config
        .desired_boot_interface
        .as_ref()
        .expect("declared boot intent");
    assert_eq!(desired.value.mac_address(), declared_mac);
    assert!(
        desired
            .value
            .interface_id()
            .expect("declared Redfish ID")
            .starts_with("NIC.Embedded.")
    );
    assert_eq!(
        repaired
            .config
            .boot_interface_selection
            .expect("declared source")
            .source,
        BootInterfaceSelectionSource::ExpectedMachine
    );
    assert_ne!(
        repaired.network_config.version,
        before.network_config.version
    );
    assert!(matches!(
        repaired.state.value,
        ManagedHostState::DPUInit { .. }
    ));
    read.rollback().await.expect("close repaired read");

    // Render the real agent response before acknowledging the corrected version and advancing the controller.
    let networking = env
        .api
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(host.dpu_ids[0]),
            },
        ))
        .await
        .expect("render HBN admin networking")
        .into_inner();
    assert!(networking.use_admin_network);
    assert_eq!(
        networking
            .admin_interface
            .expect("rendered admin interface")
            .addresses
            .iter()
            .find(|address| address.address_family == rpc::forge::AddressFamily::V4 as i32)
            .expect("rendered IPv4 admin address")
            .ip,
        admin_ip
    );
    assert_eq!(
        networking.managed_host_config_version,
        repaired.network_config.version.to_string()
    );
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    // Reload admission and the leased secondary NIC to prove the automatic path completed with independent boot intent.
    let mut read = env.db_txn().await;
    let admitted = host.host().db_machine(&mut read).await;
    let boot_nic = db::machine_interface::find_one(&mut *read, nic_id)
        .await
        .expect("secondary boot NIC");
    assert!(matches!(
        admitted.state.value,
        ManagedHostState::HostInit { .. }
    ));
    assert_eq!(
        admitted.primary_attached_dpu_machine_id(),
        Some(host.dpu_ids[0])
    );
    assert_eq!(
        admitted.network_config.version,
        repaired.network_config.version
    );
    assert_eq!(
        admitted
            .config
            .desired_boot_interface
            .expect("admitted boot target")
            .value,
        desired.value
    );
    assert!(!boot_nic.primary_interface);
    assert_eq!(boot_nic.addresses, anonymous.addresses);
    read.rollback().await.expect("close admitted read");
}
