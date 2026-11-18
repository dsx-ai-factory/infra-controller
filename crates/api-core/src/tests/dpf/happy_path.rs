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

//! DPF happy-path and inventory integration tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ::rpc::forge::forge_server::Forge;
use ::rpc::{DiscoveryData, DiscoveryInfo, forge as rpc};
use carbide_dpf::{DpfError, DpuDeploymentType, DpuPhase, DpuServiceVersion};
use carbide_machine_controller::dpf::{DpfOperations, MockDpfOperations};
use carbide_redfish::libredfish::test_support::{RedfishSimAction, RedfishSimPlatformAction};
use futures::FutureExt;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::hardware_info::{DpuHostPf, HardwareInfo};
use model::machine::{DpfState, DpuInitState, ManagedHostState};
use model::test_support::HardwareInfoTemplate;
use model::test_support::dpu::DPU_BF4_INFO_JSON;
use tokio::time::timeout;
use tonic::Request;

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

use super::{bf4_deployment_config, dpf_config, expect_dpf_service_inventory, phase_observation};
use crate::tests::common::api_fixtures::dpu::{create_machine_inventory, dpu_discover_dhcp};
use crate::tests::common::api_fixtures::site_explorer::{
    MockExploredHost, register_expected_machine, seed_bmc_root_credentials,
};
use crate::tests::common::api_fixtures::test_managed_host::TestPredictedManagedHost;
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_managed_host_with_dpf, create_managed_host_with_dpf_bf4,
    create_test_env_with_overrides, discovery_completed, get_config,
    network_configured_with_health,
};

fn default_mock(deployment_type: DpuDeploymentType) -> MockDpfOperations {
    let mut mock = MockDpfOperations::new();
    mock.expect_register_dpu_device()
        .returning(|_, _, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_get_dpu_phase_observation()
        .returning(|_, _| Ok(phase_observation(DpuPhase::Ready)));
    mock.expect_deployment_type_for_dpu()
        .returning(move |__, _| Ok(deployment_type));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    mock
}

#[crate::sqlx_test]
async fn test_dpu_and_host_till_ready(pool: sqlx::PgPool) {
    let mut mock = default_mock(DpuDeploymentType::Bf3);
    expect_dpf_service_inventory(&mut mock);
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);

    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;
    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    let mut txn = env.db_txn().await;
    let host = mh.host().db_machine(&mut txn).await;
    let dpu = mh.dpu().db_machine(&mut txn).await;

    assert!(host.config.dpf.used_for_ingestion);
    assert!(matches!(dpu.current_state(), ManagedHostState::Ready));

    let carbide_machines_per_state = env.test_meter.parsed_metrics("carbide_machines_per_state");

    assert!(carbide_machines_per_state.contains(&(
        "{fresh=\"true\",state=\"ready\",substate=\"\"}".to_string(),
        "3".to_string()
    )));
}

/// Verifies DPF ingestion replaces a distinct bootstrap MAC in rendered networking and admits HostInit only after the corrected version is acknowledged.
/// Keeping the provisional ACK exposes DPF wiring that could otherwise admit networking for the retired host interface.
#[crate::sqlx_test]
async fn bf4_dpf_pf_correction_requires_rendered_network_ack(pool: sqlx::PgPool) {
    // Select BF4 DPF provisioning while the ordinary host fixture supplies bootstrap networking and BMC identity.
    let mut mock = default_mock(DpuDeploymentType::Bf4Generic);
    expect_dpf_service_inventory(&mut mock);
    let mut config = get_config();
    config.dpf = dpf_config();
    config.dpf.deployments.bf4_generic = Some(bf4_deployment_config("bf4-generic"));
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock)),
    )
    .await;
    let mut host_config = env.managed_host_config();
    host_config.dpus[0].hardware_info_template = HardwareInfoTemplate::Custom(DPU_BF4_INFO_JSON); // Supplies BF4 classification to DPF and agent discovery.
    let dpu = host_config.get_and_assert_single_dpu().clone();
    register_expected_machine(&env, &host_config, Some(true)).await;
    seed_bmc_root_credentials(&env, &host_config)
        .await
        .expect("BMC credentials");

    // Run Explorer pairing before DPF so its ordinary transitions must preserve the provisional host link.
    let explored = MockExploredHost::new(&env, host_config)
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
        .discover_dhcp_host_primary_iface(|_, _| Ok(()))
        .boxed()
        .await
        .expect("bootstrap host lease");
    let dpu_id = explored.dpu_machine_ids[&0];
    let mut read = env.db_txn().await;
    let host_id = db::machine::find_host_by_dpu_machine_id(&mut read, &dpu_id)
        .await
        .expect("paired host lookup")
        .expect("paired host")
        .id;
    read.rollback().await.expect("close pairing read");
    let host = TestPredictedManagedHost {
        id: host_id.try_into().expect("predicted host ID"),
        dpu_ids: vec![dpu_id],
        api: env.api.clone(),
    };

    // Stop before DPF consumes its Ready observation because agent discovery must complete first.
    env.run_machine_state_controller_iteration_until_state_condition_with_requeue(
        &host_id,
        20,
        false,
        |machine| {
            matches!(machine.current_state(), ManagedHostState::DPUInit { dpu_states }
            if matches!(dpu_states.states.get(&dpu_id), Some(DpuInitState::DpfStates {
                state: DpfState::WaitingForReady { phase_detail: None },
            })))
        },
    )
    .await;
    create_machine_inventory(&env, dpu_id).await;

    // Register a factual PF MAC that differs from the factory MAC retained by Explorer's bootstrap link.
    let corrected_mac = "02:00:00:fa:00:01".parse().expect("corrected PF MAC");
    assert_ne!(corrected_mac, dpu.host_mac_address);
    let mut hardware_info = HardwareInfo::from(&dpu);
    hardware_info
        .dpu_info
        .as_mut()
        .expect("BF4 DPU hardware")
        .host_pfs = vec![DpuHostPf {
        pci_controller_id: env.config.dpu_host_pf_pci_controller_id,
        pf_id: 0,
        mac_address: corrected_mac, // Forces host-interface replacement instead of a same-MAC refresh.
    }];
    let reporting_interface = dpu_discover_dhcp(&env, &dpu.oob_mac_address.to_string()).await;
    let registered = env
        .api
        .discover_machine(Request::new(rpc::MachineDiscoveryInfo {
            machine_interface_id: Some(reporting_interface),
            create_machine: true,
            discovery_data: Some(DiscoveryData::Info(
                DiscoveryInfo::try_from(hardware_info).expect("wire BF4 hardware"),
            )),
            discovery_reporter: rpc::MachineDiscoveryReporter::DpuAgent as i32,
            ..Default::default()
        }))
        .await
        .expect("DPF agent registration")
        .into_inner();
    assert_eq!(registered.machine_id, Some(dpu_id.into()));

    // Complete the agent's discovery request before DPF advances beyond its Ready observation.
    let discovery = host.dpu().forge_agent_control().await;
    assert!(matches!(
        discovery.action,
        Some(rpc::forge_agent_control_response::Action::Discovery(_))
    ));
    discovery_completed(&env, &dpu_id).await;

    // Disable requeue to observe DPF's network wait before PF finalization can replace the bootstrap interface.
    env.run_machine_state_controller_iteration_until_state_condition_with_requeue(
        &host_id,
        35,
        false,
        |machine| matches!(machine.current_state(), ManagedHostState::DPUInit { dpu_states }
            if matches!(dpu_states.states.get(&dpu_id), Some(DpuInitState::WaitingForNetworkConfig))),
    )
    .await;

    // Confirm DPF marked ingestion while the host still owns the factory-MAC bootstrap interface.
    let mut read = env.db_txn().await;
    let before = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close provisional read");
    assert!(before.config.dpf.used_for_ingestion);
    let bootstrap_link = before
        .status
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("bootstrap primary");
    assert_eq!(bootstrap_link.mac_address, dpu.host_mac_address);

    // Render the provisional DPF interface and its leased address before recording the old ACK.
    let provisional = env
        .api
        .get_managed_host_network_config(Request::new(rpc::ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_id),
        }))
        .await
        .expect("render provisional DPF networking")
        .into_inner();
    assert!(provisional.use_admin_network);
    assert_eq!(
        provisional.host_interface_id,
        Some(bootstrap_link.id.to_string())
    );
    assert_eq!(
        provisional.managed_host_config_version,
        before.network_config.version.to_string()
    );
    let provisional_admin = provisional
        .admin_interface
        .as_ref()
        .expect("provisional admin interface");
    let leased_address = &explored
        .host_dhcp_response
        .as_ref()
        .expect("bootstrap host lease")
        .address;
    assert_eq!(provisional_admin.ip.as_ref(), Some(leased_address));
    assert!(!provisional_admin.addresses.is_empty());

    // Acknowledge the provisional response before the controller publishes the corrected interface and version.
    network_configured_with_health(&env, &dpu_id, None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    // Reload the repair's immediate Wait and changed version before fetching corrected networking.
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close repaired read");
    assert!(
        matches!(&repaired.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. })
        if reason.contains("finalized") && reason.contains("reload"))
    );
    assert_ne!(
        repaired.network_config.version,
        before.network_config.version
    );

    // Find the corrected host afresh and render its DPF response to verify interface identity and address continuity.
    let found = host.host().rpc_machine().await;
    let status = found.status.expect("persisted host status");
    let corrected_link = status
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("corrected primary");
    assert_eq!(corrected_link.mac_address, corrected_mac.to_string());
    assert_eq!(corrected_link.attached_dpu_machine_id, Some(dpu_id));
    let corrected = env
        .api
        .get_managed_host_network_config(Request::new(rpc::ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_id),
        }))
        .await
        .expect("render corrected DPF networking")
        .into_inner();
    assert!(corrected.use_admin_network);
    assert_eq!(
        corrected.host_interface_id,
        Some(
            corrected_link
                .id
                .expect("corrected interface ID")
                .to_string()
        )
    );
    assert_ne!(corrected.host_interface_id, provisional.host_interface_id);
    assert_eq!(
        corrected
            .admin_interface
            .as_ref()
            .expect("corrected admin interface")
            .addresses,
        provisional
            .admin_interface
            .as_ref()
            .expect("provisional admin interface")
            .addresses
    );
    assert_eq!(
        corrected.managed_host_config_version,
        repaired.network_config.version.to_string()
    );

    // The retained provisional ACK must leave the persisted DPF state waiting for the corrected network version.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let waiting = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close old ACK read");
    assert!(
        matches!(&waiting.state.value, ManagedHostState::DPUInit { dpu_states }
        if matches!(dpu_states.states.get(&dpu_id), Some(DpuInitState::WaitingForNetworkConfig)))
    );
    assert!(
        matches!(&waiting.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. })
        if reason.contains("current network config version"))
    );
    assert_eq!(
        waiting.network_config.version,
        repaired.network_config.version
    );

    // Acknowledge the newly rendered version, then reload the controller's committed HostInit transition.
    network_configured_with_health(&env, &dpu_id, None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let admitted = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close admission read");
    assert!(matches!(
        admitted.state.value,
        ManagedHostState::HostInit { .. }
    ));
    assert!(admitted.config.dpf.used_for_ingestion);
    assert_eq!(
        admitted.network_config.version,
        repaired.network_config.version
    );
}

/// Provision a DPU through DPF and verify that NICo performs only the
/// credential portion of post-ready platform handling.
async fn assert_dpf_skips_platform_configuration(
    pool: sqlx::PgPool,
    enable_secure_boot: bool,
    deployment_type: DpuDeploymentType,
) {
    let mut mock = default_mock(deployment_type);
    expect_dpf_service_inventory(&mut mock);
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);

    let mut config = get_config();
    config.dpf = dpf_config();
    // Configure the BF4 deployment to match the Bf4Generic provisioning reported by the mock.
    config.dpf.deployments.bf4_generic = Some(bf4_deployment_config("bf4-generic"));
    config.dpu_config.dpu_enable_secure_boot = enable_secure_boot;

    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;
    let redfish_timepoint = env.redfish_sim.timepoint();

    let mh = timeout(TEST_TIMEOUT, async {
        if deployment_type == DpuDeploymentType::Bf4Generic {
            create_managed_host_with_dpf_bf4(&env).await
        } else {
            create_managed_host_with_dpf(&env).await
        }
    })
    .await
    .expect("timed out during initial provisioning");

    let mut txn = env.db_txn().await;
    let host = mh.host().db_machine(&mut txn).await;
    let dpu = mh.dpu().db_machine(&mut txn).await;

    assert!(host.config.dpf.used_for_ingestion);
    assert!(matches!(dpu.current_state(), ManagedHostState::Ready));

    let dpu_bmc_ip = dpu
        .status
        .bmc_info
        .ip
        .expect("DPU BMC IP must be present")
        .to_string();

    // DPF-ingested DPUs must not receive the opaque vendor machine_setup call.
    let dpu_redfish_actions = env
        .redfish_sim
        .actions_since(&redfish_timepoint)
        .for_host(&dpu_bmc_ip);
    assert!(
        dpu_redfish_actions
            .iter()
            .all(|action| !matches!(action, RedfishSimAction::MachineSetup { .. })),
        "DPF-ingested DPU received machine_setup: {dpu_redfish_actions:?}"
    );

    let platform_actions = env.redfish_sim.platform_actions();

    // Credential replacement remains NICo-owned until DPF supports it.
    assert_eq!(
        platform_actions
            .iter()
            .filter(|action| matches!(action, RedfishSimPlatformAction::UefiSetup { dpu: true }))
            .count(),
        1,
        "DPU UEFI credential was not replaced exactly once: {platform_actions:?}"
    );

    // No platform mutation or BIOS verification may run after DPF reports Ready.
    assert!(
        platform_actions.iter().all(|action| !matches!(
            action,
            RedfishSimPlatformAction::SetHostRshim { host }
                | RedfishSimPlatformAction::SetHostPrivilegeLevel { host }
                | RedfishSimPlatformAction::IsBiosSetup { host }
                if host == &dpu_bmc_ip
        )),
        "DPF-ingested DPU received post-ready platform configuration: {platform_actions:?}"
    );
}

/// BF4 skips `machine_setup` when the legacy non-secure-boot branch is selected.
#[crate::sqlx_test]
async fn test_bf4_dpf_skips_machine_setup(pool: sqlx::PgPool) {
    assert_dpf_skips_platform_configuration(pool, false, DpuDeploymentType::Bf4Generic).await;
}

/// BF4 skips the RShim and host-privilege mutations when secure boot is enabled.
#[crate::sqlx_test]
async fn test_bf4_dpf_skips_secure_boot_platform_setup(pool: sqlx::PgPool) {
    assert_dpf_skips_platform_configuration(pool, true, DpuDeploymentType::Bf4Generic).await;
}

#[crate::sqlx_test]
async fn test_bf3_dpf_skips_machine_setup(pool: sqlx::PgPool) {
    assert_dpf_skips_platform_configuration(pool, false, DpuDeploymentType::Bf3).await;
}

#[crate::sqlx_test]
async fn test_bf3_dpf_skips_secure_boot_platform_setup(pool: sqlx::PgPool) {
    assert_dpf_skips_platform_configuration(pool, true, DpuDeploymentType::Bf3).await;
}

/// Verifies DPF inventory uses the host ingestion flag and composite DPU CR name,
/// and preserves the last complete operator inventory when a later lookup fails.
#[crate::sqlx_test]
async fn test_dpf_inventory_uses_host_context_and_preserves_last_good_value(pool: sqlx::PgPool) {
    let queried_dpu_names = Arc::new(Mutex::new(Vec::new()));
    let fail_inventory_lookup = Arc::new(AtomicBool::new(false));
    let mut mock = default_mock(DpuDeploymentType::Bf3);
    let queried_dpu_names_for_mock = queried_dpu_names.clone();
    let fail_inventory_lookup_for_mock = fail_inventory_lookup.clone();
    mock.expect_get_service_versions_for_dpu()
        .returning(move |dpu_name| {
            queried_dpu_names_for_mock
                .lock()
                .expect("queried DPU names lock must not be poisoned")
                .push(dpu_name.to_string());
            if fail_inventory_lookup_for_mock.load(Ordering::SeqCst) {
                return Err(DpfError::InvalidState(
                    "referenced service template is unavailable".to_string(),
                ));
            }
            Ok(vec![DpuServiceVersion {
                name: "doca-hbn".to_string(),
                version: "operator-version".to_string(),
                url: "nvcr.io/nvidia/doca".to_string(),
            }])
        });

    // Ingest through DPF so only the host receives used_for_ingestion.
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);
    let mut config = get_config();
    config.dpf = dpf_config();
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;
    let managed_host = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    // Read both records through the public API and derive the expected CR name
    // independently from their reported BMC MAC addresses.
    let host = env.find_machine(&managed_host.id).await.remove(0);
    let dpu = env.find_machine(&managed_host.dpu_ids[0]).await.remove(0);
    assert!(
        host.config
            .as_ref()
            .and_then(|config| config.dpf.as_ref())
            .is_some_and(|dpf| dpf.used_for_ingestion)
    );
    assert!(
        !dpu.config
            .as_ref()
            .and_then(|config| config.dpf.as_ref())
            .is_some_and(|dpf| dpf.used_for_ingestion)
    );
    let host_node_id = host
        .bmc
        .as_ref()
        .and_then(|bmc| bmc.mac.as_deref())
        .expect("host BMC MAC must exist")
        .to_ascii_lowercase()
        .replace(':', "-");
    let dpu_device_id = dpu
        .bmc
        .as_ref()
        .and_then(|bmc| bmc.mac.as_deref())
        .expect("DPU BMC MAC must exist")
        .to_ascii_lowercase()
        .replace(':', "-");
    let expected_dpu_name = format!("node-{host_node_id}-device-{dpu_device_id}");
    queried_dpu_names
        .lock()
        .expect("queried DPU names lock must not be poisoned")
        .clear();

    // Report an incomplete agent inventory and confirm the operator value wins.
    let report = || {
        Request::new(rpc::DpuAgentInventoryReport {
            machine_id: Some(managed_host.dpu_ids[0]),
            inventory: Some(rpc::MachineInventory {
                components: vec![rpc::MachineInventorySoftwareComponent {
                    name: "agent-only".to_string(),
                    version: "incomplete".to_string(),
                    url: String::new(),
                }],
            }),
        })
    };
    env.api
        .update_agent_reported_inventory(report())
        .await
        .expect("DPF inventory update must succeed");
    assert_eq!(
        *queried_dpu_names
            .lock()
            .expect("queried DPU names lock must not be poisoned"),
        vec![expected_dpu_name]
    );
    let stored_inventory = env
        .find_machine(&managed_host.dpu_ids[0])
        .await
        .remove(0)
        .inventory
        .expect("operator inventory must be persisted");
    assert_eq!(
        stored_inventory.components,
        vec![rpc::MachineInventorySoftwareComponent {
            name: "doca-hbn".to_string(),
            version: "operator-version".to_string(),
            url: "nvcr.io/nvidia/doca".to_string(),
        }]
    );

    // A later incomplete operator view must fail before replacing the complete value.
    fail_inventory_lookup.store(true, Ordering::SeqCst);
    env.api
        .update_agent_reported_inventory(report())
        .await
        .expect_err("incomplete DPF inventory must be rejected");
    let inventory_after_error = env
        .find_machine(&managed_host.dpu_ids[0])
        .await
        .remove(0)
        .inventory
        .expect("last complete inventory must remain persisted");
    assert_eq!(inventory_after_error, stored_inventory);
}
