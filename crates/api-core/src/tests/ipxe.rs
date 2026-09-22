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

use carbide_host_support::bootstrap_ca::BootstrapCaSource;
use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use chrono::Utc;
use common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_test_env, create_test_env_with_overrides, get_config,
};
use config_version::ConfigVersion;
use db::{self};
use futures_util::FutureExt;
use mac_address::MacAddress;
use model::machine::{
    CleanupContext, DpuInitState, HostReprovisionState, MachineState, ManagedHostState,
    ReadyBootConfigState, SetBootOrderInfo, SetBootOrderState,
};
use model::machine_boot_interface::MachineBootInterfaceTarget;
use model::test_support::ManagedHostConfig;
use rpc::forge::CloudInitInstructionsRequest;
use rpc::forge::forge_server::Forge;

use crate::test_support::fixture_config::FixtureDefault as _;
use crate::test_support::mac_address_pool::DPU_OOB_MAC_ADDRESS_POOL;
use crate::tests::common;
use crate::tests::common::api_fixtures::site_explorer::MockExploredHost;
use crate::tests::common::rpc_builder::DhcpDiscovery;

async fn move_machine_to_needed_state(
    machine_id: MachineId,
    state: &ManagedHostState,
    pool: &sqlx::PgPool,
) {
    let mut txn = pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");
    let machine = db::machine::find_one(
        txn.as_mut(),
        &machine_id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .unwrap();

    db::machine::advance(&machine, &mut txn, state, None)
        .await
        .unwrap();
    txn.commit().await.unwrap();
}

async fn get_pxe_instructions(
    env: &TestEnv,
    interface_id: MachineInterfaceId,
    arch: rpc::forge::MachineArchitecture,
    product: Option<String>,
) -> rpc::forge::PxeInstructions {
    let mut txn = env.pool.begin().await.unwrap();
    let iface = db::machine_interface::find_one(txn.as_mut(), interface_id)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let client_ip = iface
        .addresses
        .first()
        .expect("interface must have at least one address to PXE boot")
        .to_string();

    env.api
        .get_pxe_instructions(tonic::Request::new(rpc::forge::PxeInstructionRequest {
            arch: arch as i32,
            product,
            client_ip: Some(client_ip),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
}

#[crate::sqlx_test]
async fn test_pxe_dpu_ready(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let (_host_id, dpu_id) = common::api_fixtures::create_managed_host(&env).await.into();
    move_machine_to_needed_state(dpu_id.into(), &ManagedHostState::Ready, &env.pool).await;

    let mut txn = env
        .pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");
    let dpu_interface_id = db::machine_interface::find_by_machine_ids(&mut txn, &[dpu_id])
        .await
        .unwrap()[&dpu_id][0]
        .id;
    txn.commit().await.unwrap();

    let instructions = get_pxe_instructions(
        &env,
        dpu_interface_id,
        rpc::forge::MachineArchitecture::Arm,
        Some("Fake Bluefield".to_string()),
    )
    .await;
    assert!(
        instructions.pxe_script.contains("Current state: Ready"),
        "Actual script: {}",
        instructions.pxe_script
    );
    assert!(instructions.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));
}

#[crate::sqlx_test]
async fn test_pxe_dpu_waiting_for_network_install(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let mh = common::api_fixtures::dpu::create_dpu_machine_in_waiting_for_network_install(
        &env,
        &host_config,
    )
    .await;

    let mut txn = env.pool.begin().await.unwrap();

    let machine = mh.dpu().db_machine(&mut txn).await;

    assert_eq!(
        machine.current_state(),
        &ManagedHostState::DPUInit {
            dpu_states: model::machine::DpuInitStates {
                states: HashMap::from([(mh.dpu_ids[0], DpuInitState::WaitingForNetworkConfig)]),
            },
        }
    );

    let instructions = get_pxe_instructions(
        &env,
        machine.status.interfaces.first().unwrap().id,
        rpc::forge::MachineArchitecture::Arm,
        Some("Fake Bluefield".to_string()),
    )
    .await;

    assert!(
        instructions
            .pxe_script
            .contains("Current state: DPUInitializing/WaitingForNetworkConfig"),
        "Actual script: {}",
        instructions.pxe_script
    );
    assert!(instructions.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    assert!(!instructions.pxe_script.contains("aarch64/carbide.root"));
}

#[crate::sqlx_test]
async fn test_dpu_pxe_gets_correct_os_when_machine_is_not_created(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    // This test ensures that when a DPU PXE boots after site-explorer ingestion, but before the
    // managed host is fully configured, we don't confuse it for an ARM host, and we give it
    // carbide.efi (the DPU OS) and *not* scout.efi.
    let env = create_test_env(pool).await;
    let mock_explored_host = MockExploredHost::new(&env, ManagedHostConfig::default());
    let dpu_oob_mac = mock_explored_host.managed_host.dpus[0].oob_mac_address;

    // Ingest the DPU BMC into site explorer
    mock_explored_host
        .discover_dhcp_dpu_bmc(0, |_, _| Ok(()))
        .boxed()
        .await?
        .insert_site_exploration_results()?
        .run_site_explorer_iteration()
        .boxed()
        .await;

    // Discover DHCP from the DPU's OOB
    let dpu_interface_id =
        common::api_fixtures::dpu::dpu_discover_dhcp(&env, &dpu_oob_mac.to_string()).await;

    let instructions = get_pxe_instructions(
        &env,
        dpu_interface_id,
        rpc::forge::MachineArchitecture::Arm,
        Some("Fake Bluefield".to_string()),
    )
    .await;

    assert!(
        !instructions.pxe_script.contains("exit"),
        "should PXE boot, got an exit instruction"
    );
    assert!(
        instructions.pxe_script.contains("aarch64/carbide.efi"),
        "should PXE boot to carbide.efi for DPU agent OS"
    );
    assert!(
        instructions
            .pxe_script
            .contains("bfks=${dpu-cloudinit-url}/user-data"),
        "DPU should fetch its kickstart from the dpu cloud-init prefix"
    );
    assert!(
        !instructions.pxe_script.contains("scout-cloudinit-url"),
        "the Scout snippet datasource is for hosts, not DPUs"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_pxe_when_machine_is_not_ingested(pool: sqlx::PgPool) -> eyre::Result<()> {
    let env = create_test_env(pool).await;
    let dpu_interface_id = common::api_fixtures::dpu::dpu_discover_dhcp(
        &env,
        &DPU_OOB_MAC_ADDRESS_POOL.allocate().to_string(),
    )
    .await;

    let instructions = get_pxe_instructions(
        &env,
        dpu_interface_id,
        rpc::forge::MachineArchitecture::Arm,
        Some("Fake Host".to_string()),
    )
    .await;

    assert!(
        instructions.pxe_script.contains("exit"),
        "should exit, since we don't know about this machine"
    );
    assert!(
        !instructions.pxe_script.contains("aarch64"),
        "should not PXE boot, since we don't know about this machine"
    );

    let instructions = get_pxe_instructions(
        &env,
        dpu_interface_id,
        rpc::forge::MachineArchitecture::X86,
        None,
    )
    .await;
    assert!(
        instructions.pxe_script.contains("exit"),
        "should exit, since we don't know about this machine"
    );
    assert!(
        !instructions.pxe_script.contains("x86_64/scout.efi"),
        "should not PXE boot, since we don't know about this machine"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_pxe_when_dpu_is_not_ingested(pool: sqlx::PgPool) -> eyre::Result<()> {
    let env = create_test_env(pool).await;
    let dpu_interface_id = common::api_fixtures::dpu::dpu_discover_dhcp(
        &env,
        &DPU_OOB_MAC_ADDRESS_POOL.allocate().to_string(),
    )
    .await;

    let instructions = get_pxe_instructions(
        &env,
        dpu_interface_id,
        rpc::forge::MachineArchitecture::Arm,
        Some("Fake Bluefield".to_string()),
    )
    .await;

    assert!(
        !instructions.pxe_script.contains("exit"),
        "should not exit, since DPUs are allowed to boot"
    );
    assert!(
        instructions.pxe_script.contains("aarch64"),
        "should PXE boot, since DPUs are allowed to boot"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_pxe_host(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let (host_id, _dpu_id) = common::api_fixtures::create_managed_host(&env).await.into();
    let mut txn = env
        .pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");
    let host_interface_id = db::machine_interface::find_by_machine_ids(&mut txn, &[host_id])
        .await
        .unwrap()[&host_id][0]
        .id;
    txn.commit().await.unwrap();
    move_machine_to_needed_state(
        host_id.into(),
        &ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForDiscovery,
        },
        &env.pool,
    )
    .await;

    let instructions = get_pxe_instructions(
        &env,
        host_interface_id,
        rpc::forge::MachineArchitecture::X86,
        None,
    )
    .await;
    assert!(instructions.pxe_script.contains("x86_64/scout.efi"));
    assert!(
        instructions
            .pxe_script
            .contains("ds=nocloud;s=${scout-cloudinit-url}"),
        "a host booting Scout should be given the discovery cloud-init datasource"
    );

    move_machine_to_needed_state(
        host_id.into(),
        &ManagedHostState::HostInit {
            machine_state: MachineState::Discovered {
                skip_reboot_wait: false,
            },
        },
        &env.pool,
    )
    .await;

    let instructions = get_pxe_instructions(
        &env,
        host_interface_id,
        rpc::forge::MachineArchitecture::X86,
        None,
    )
    .await;
    assert!(instructions.pxe_script.contains("x86_64/scout.efi"));

    move_machine_to_needed_state(
        host_id.into(),
        &ManagedHostState::BootConfiguring {
            desired_version: ConfigVersion::new(7),
            desired_boot_interface: MachineBootInterfaceTarget::MacOnly(
                "02:00:00:00:00:01".parse().unwrap(),
            ),
            post_lock_verification_retry_count: 0,
            boot_config_state: ReadyBootConfigState::SetBootOrder {
                set_boot_order_info: SetBootOrderInfo {
                    set_boot_order_jid: None,
                    set_boot_order_state: SetBootOrderState::SetBootOrder,
                    retry_count: 0,
                },
            },
        },
        &env.pool,
    )
    .await;

    let instructions = get_pxe_instructions(
        &env,
        host_interface_id,
        rpc::forge::MachineArchitecture::X86,
        None,
    )
    .await;
    assert!(instructions.pxe_script.contains("x86_64/scout.efi"));

    move_machine_to_needed_state(
        host_id.into(),
        &ManagedHostState::HostReprovision {
            reprovision_state: HostReprovisionState::WaitingForManualUpgrade {
                manual_upgrade_started: Utc::now(),
            },
            retry_count: 0,
        },
        &env.pool,
    )
    .await;

    let instructions = get_pxe_instructions(
        &env,
        host_interface_id,
        rpc::forge::MachineArchitecture::X86,
        None,
    )
    .await;
    assert!(instructions.pxe_script.contains("x86_64/scout.efi"));

    move_machine_to_needed_state(
        host_id.into(),
        &ManagedHostState::WaitingForCleanup {
            cleanup_state: model::machine::CleanupState::Init,
            cleanup_context: CleanupContext::Deprovision,
        },
        &env.pool,
    )
    .await;

    let instructions = get_pxe_instructions(
        &env,
        host_interface_id,
        rpc::forge::MachineArchitecture::X86,
        Some("Fake X86 Host".to_string()),
    )
    .await;
    assert!(instructions.pxe_script.contains("x86_64/scout.efi"));
}

#[crate::sqlx_test]
async fn test_pxe_instance(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let mut txn = env
        .pool
        .clone()
        .begin()
        .await
        .expect("Unable to create transaction on database pool");
    let host_interface = mh.host().first_interface(&mut txn).await;
    txn.commit().await.unwrap();

    mh.instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    let instructions = host_interface
        .get_pxe_instructions(rpc::forge::MachineArchitecture::X86)
        .await;

    assert_eq!(
        instructions.pxe_script,
        "set nico-retry-provisioning 1\nSomeRandomiPxe"
    );
}

#[crate::sqlx_test]
async fn test_cloud_init_when_machine_is_not_created(pool: sqlx::PgPool) {
    let env = common::api_fixtures::create_test_env(pool).await;

    let mac_address = "FF:FF:FF:FF:FF:FF".to_string();
    let _ = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(&mac_address, "192.0.2.1").tonic_request())
        .await
        .unwrap()
        .into_inner();

    // Interface is created. Let's fetch interface id.
    let mut txn = env.pool.begin().await.unwrap();
    let interfaces = db::machine_interface::find_by_mac_address(
        txn.as_mut(),
        mac_address.parse::<MacAddress>().unwrap(),
    )
    .await
    .unwrap();

    assert_eq!(interfaces.len(), 1);

    let cloud_init_cfg = env
        .api
        .get_cloud_init_instructions(tonic::Request::new(CloudInitInstructionsRequest {
            ip: interfaces[0].addresses[0].to_string(),
        }))
        .await
        .expect("get_cloud_init_instructions returned an error")
        .into_inner();

    assert!(cloud_init_cfg.discovery_instructions.is_some());
}

/// Verifies cloud-init discovery instructions carry configured DPU provisioning values.
#[crate::sqlx_test]
async fn test_cloud_init_uses_configured_dpu_provisioning_values(pool: sqlx::PgPool) {
    let mut config = get_config();
    config.dpu_config.num_of_vfs = 64;
    config.dpu_config.bootstrap_ca_source = BootstrapCaSource::Embedded;
    config
        .vmaas_config
        .as_mut()
        .expect("test config should include VMaaS settings")
        .hbn_reps = Some("pf0hpf,pf0vf0,pf0vf2".to_string());
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;

    // Discover an unassigned interface so the API returns discovery instructions.
    let mac_address = "FF:FF:FF:FF:FF:FE".to_string();
    env.api
        .discover_dhcp(DhcpDiscovery::builder(&mac_address, "192.0.2.1").tonic_request())
        .await
        .unwrap();

    // Look up the allocated IP address to request cloud-init instructions.
    let mut txn = env.pool.begin().await.unwrap();
    let interfaces = db::machine_interface::find_by_mac_address(
        txn.as_mut(),
        mac_address.parse::<MacAddress>().unwrap(),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let cloud_init_cfg = env
        .api
        .get_cloud_init_instructions(tonic::Request::new(CloudInitInstructionsRequest {
            ip: interfaces[0].addresses[0].to_string(),
        }))
        .await
        .expect("get_cloud_init_instructions returned an error")
        .into_inner();

    // The configured value should pass through to carbide-pxe unchanged.
    let discovery_instructions = cloud_init_cfg
        .discovery_instructions
        .expect("expected discovery instructions");
    assert_eq!(
        discovery_instructions.hbn_reps.as_deref(),
        Some("pf0hpf,pf0vf0,pf0vf2")
    );
    assert_eq!(discovery_instructions.num_of_vfs, Some(64));
    assert_eq!(
        discovery_instructions.bootstrap_ca_source,
        rpc::forge::BootstrapCaSource::Embedded as i32
    );
}

#[crate::sqlx_test]
async fn test_cloud_init_after_dpu_update(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let (_host_id, dpu_id) = common::api_fixtures::create_managed_host(&env).await.into();
    move_machine_to_needed_state(
        dpu_id.into(),
        &ManagedHostState::DPUInit {
            dpu_states: model::machine::DpuInitStates {
                states: HashMap::from([(dpu_id, DpuInitState::Init)]),
            },
        },
        &env.pool,
    )
    .await;

    // Interface is created. Let's fetch interface id.
    let machine = env.find_machine(&dpu_id).await.remove(0);
    assert_eq!(machine.status.as_ref().unwrap().interfaces.len(), 1);

    let cloud_init_cfg = env
        .api
        .get_cloud_init_instructions(tonic::Request::new(CloudInitInstructionsRequest {
            ip: machine.status.as_ref().unwrap().interfaces[0].address[0].clone(),
        }))
        .await
        .expect("get_cloud_init_instructions returned an error")
        .into_inner();

    assert!(cloud_init_cfg.discovery_instructions.is_some());
}

/// Helper to create a test env with external URL overrides configured.
async fn create_env_with_external_urls(pool: sqlx::PgPool) -> TestEnv {
    let mut config = get_config();
    config.external_api_url = Some("https://10.99.0.1:1079".to_string());
    config.external_pxe_url = Some("http://10.99.0.1:8080".to_string());
    config.external_static_pxe_url = Some("http://10.99.0.1:8081".to_string());
    create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await
}

/// Preallocate a static interface on the static-assignments segment and
/// return its interface ID plus the IP address used.
async fn preallocate_external_interface(
    env: &TestEnv,
    mac: &str,
    ip: &str,
) -> (MachineInterfaceId, String) {
    let mac_address: MacAddress = mac.parse().unwrap();
    let ip_addr: std::net::IpAddr = ip.parse().unwrap();

    let mut txn = env.pool.begin().await.unwrap();
    db::machine_interface::preallocate_machine_interface(&mut txn, mac_address, ip_addr, None)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Look up the interface ID that was created.
    let mut txn = env.pool.begin().await.unwrap();
    let interfaces = db::machine_interface::find_by_mac_address(txn.as_mut(), mac_address)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    assert_eq!(
        interfaces.len(),
        1,
        "expected exactly one interface for MAC {mac}"
    );
    (interfaces[0].id, ip.to_string())
}

#[crate::sqlx_test]
async fn test_pxe_url_overrides_for_external_host(pool: sqlx::PgPool) {
    let env = create_env_with_external_urls(pool).await;

    // Use an IP outside any managed prefix so it lands on static-assignments.
    let (interface_id, _ip) =
        preallocate_external_interface(&env, "AA:BB:CC:DD:EE:01", "10.99.0.50").await;

    let instructions = get_pxe_instructions(
        &env,
        interface_id,
        rpc::forge::MachineArchitecture::X86,
        None,
    )
    .await;

    // The interface is on the static-assignments segment, so the API should
    // return the configured external URLs as overrides.
    assert_eq!(
        instructions.api_url_override.as_deref(),
        Some("https://10.99.0.1:1079"),
        "expected api_url_override for external host"
    );
    assert_eq!(
        instructions.pxe_url_override.as_deref(),
        Some("http://10.99.0.1:8080"),
        "expected pxe_url_override for external host"
    );
    assert_eq!(
        instructions.static_pxe_url_override.as_deref(),
        Some("http://10.99.0.1:8081"),
        "expected static_pxe_url_override for external host"
    );
}

#[crate::sqlx_test]
async fn test_cloud_init_url_overrides_for_external_host(pool: sqlx::PgPool) {
    let env = create_env_with_external_urls(pool).await;

    let (_interface_id, ip) =
        preallocate_external_interface(&env, "AA:BB:CC:DD:EE:02", "10.99.0.51").await;

    let cloud_init = env
        .api
        .get_cloud_init_instructions(tonic::Request::new(CloudInitInstructionsRequest { ip }))
        .await
        .expect("get_cloud_init_instructions returned an error")
        .into_inner();

    assert!(
        cloud_init.discovery_instructions.is_some(),
        "expected discovery instructions for external host"
    );

    assert_eq!(
        cloud_init.api_url_override.as_deref(),
        Some("https://10.99.0.1:1079"),
        "expected api_url_override for external host cloud-init"
    );
    assert_eq!(
        cloud_init.pxe_url_override.as_deref(),
        Some("http://10.99.0.1:8080"),
        "expected pxe_url_override for external host cloud-init"
    );
}

#[crate::sqlx_test]
async fn test_pxe_url_overrides_none_for_internal_host(pool: sqlx::PgPool) {
    let env = create_env_with_external_urls(pool).await;

    // Create a normal managed host -- its interface is on the admin segment, not
    // static-assignments.
    let (host_id, _dpu_id) = common::api_fixtures::create_managed_host(&env).await.into();
    let mut txn = env.pool.begin().await.unwrap();
    let host_interface_id = db::machine_interface::find_by_machine_ids(&mut txn, &[host_id])
        .await
        .unwrap()[&host_id][0]
        .id;
    txn.commit().await.unwrap();

    let instructions = get_pxe_instructions(
        &env,
        host_interface_id,
        rpc::forge::MachineArchitecture::X86,
        None,
    )
    .await;

    // Internal hosts should NOT get URL overrides.
    assert_eq!(
        instructions.api_url_override, None,
        "internal host should not get api_url_override"
    );
    assert_eq!(
        instructions.pxe_url_override, None,
        "internal host should not get pxe_url_override"
    );
    assert_eq!(
        instructions.static_pxe_url_override, None,
        "internal host should not get static_pxe_url_override"
    );
}

#[crate::sqlx_test]
async fn test_cloud_init_url_overrides_none_for_internal_host(pool: sqlx::PgPool) {
    let env = create_env_with_external_urls(pool).await;

    // Discover a regular DHCP interface on a managed segment.
    let mac_address = "AA:BB:CC:DD:EE:03".to_string();
    // Use the admin segment gateway IP as the relay so the interface lands
    // on the admin segment.
    env.api
        .discover_dhcp(DhcpDiscovery::builder(&mac_address, "192.0.2.1").tonic_request())
        .await
        .unwrap();

    // Look up the actual allocated IP for this interface.
    let mut txn = env.pool.begin().await.unwrap();
    let interfaces = db::machine_interface::find_by_mac_address(
        txn.as_mut(),
        mac_address.parse::<MacAddress>().unwrap(),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    assert_eq!(interfaces.len(), 1);
    let allocated_ip = interfaces[0].addresses[0].to_string();

    let cloud_init = env
        .api
        .get_cloud_init_instructions(tonic::Request::new(CloudInitInstructionsRequest {
            ip: allocated_ip,
        }))
        .await
        .expect("get_cloud_init_instructions returned an error")
        .into_inner();

    assert!(cloud_init.discovery_instructions.is_some());

    // Internal hosts should NOT get URL overrides.
    assert_eq!(
        cloud_init.api_url_override, None,
        "internal host should not get api_url_override"
    );
    assert_eq!(
        cloud_init.pxe_url_override, None,
        "internal host should not get pxe_url_override"
    );
}

// carbide-pxe identifies the booting machine by the client IP it observed
// and forwards it to carbide-api. An IP that doesn't map to any interface
// in machine_interface_addresses should NotFound cleanly. (The happy path
// -- known IP resolves to the right interface -- is exercised by every
// other test in this module that calls get_pxe_instructions, since the
// helper now goes through the client_ip lookup.)
#[crate::sqlx_test]
async fn test_pxe_instructions_unknown_client_ip_returns_not_found(pool: sqlx::PgPool) {
    let env = create_env_with_external_urls(pool).await;

    let result = env
        .api
        .get_pxe_instructions(tonic::Request::new(rpc::forge::PxeInstructionRequest {
            arch: rpc::forge::MachineArchitecture::X86 as i32,
            product: None,
            client_ip: Some("203.0.113.99".to_string()),
            ..Default::default()
        }))
        .await;

    let status = result.expect_err("expected NotFound for unknown client_ip");
    assert_eq!(status.code(), tonic::Code::NotFound, "got: {status:?}");
}
