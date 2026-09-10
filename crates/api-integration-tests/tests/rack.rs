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

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, TcpListener};
use std::time::Duration;

use api_test_helper::api_server::{TEST_BMC_DHCP_RELAY_ADDRESS, TEST_BMC_NETWORK_PREFIX};
use api_test_helper::utils::TestApiServerArgs;
use api_test_helper::{IntegrationTestEnvironment, utils};
use bmc_mock::ListenerOrAddress;
use bmc_mock::test_support::TEST_MAC_POOL;
use carbide_utils::HostPortPair;
use carbide_uuid::rack::{RackId, RackProfileId};
use eyre::ContextCompat;
use futures::future::join_all;
use machine_a_tron::lifecycle_timings::{LifecycleTimingOverrides, PartialLifecycleTimings};
use machine_a_tron::{
    BmcMockRegistry, DhcpType, LenovoGb300RackConfig, LogFormat, MachineATronConfig, RackConfig,
    RackModelConfig, WiwynnGb200RackConfig,
};
use tokio_util::sync::CancellationToken;

const UNDERLAY_DHCP_RELAY_ADDRESS: Ipv4Addr = Ipv4Addr::new(172, 20, 1, 1);

#[ctor::ctor(unsafe)]
fn setup() {
    api_test_helper::setup_logging()
}

#[tokio::test(flavor = "multi_thread")]
async fn test_machine_a_tron_racks_integration() -> eyre::Result<()> {
    let Some(mut test_env) = IntegrationTestEnvironment::try_from_environment(
        1,
        "api_server_test_machine_a_tron_rack_integration",
    )
    .await?
    else {
        return Ok(());
    };

    let bmc_address_registry = BmcMockRegistry::default();
    let certs_dir = test_env.root_dir.join("crates/bmc-mock");
    let server_config = bmc_mock::tls::server_config(Some(certs_dir)).unwrap();
    let mut bmc_mock_handle = bmc_mock::CombinedServer::run(
        "bmc-mock",
        bmc_address_registry.clone(),
        Some(ListenerOrAddress::Listener(TcpListener::bind(
            "127.0.0.1:0",
        )?)),
        server_config,
    );
    let empty_firmware_dir = temp_dir::TempDir::with_prefix("firmware")?;
    let cancel_token = CancellationToken::new();
    let server_handle = utils::start_api_server(
        &mut test_env,
        TestApiServerArgs {
            bmc_proxy: Some(HostPortPair::HostAndPort(
                "127.0.0.1".to_string(),
                bmc_mock_handle.address.port(),
            )),
            firmware_directory: empty_firmware_dir.path().to_owned(),
            addr_index: 0,
            put_dev_bin_in_path: true,
            insecure_discovery: true,
        },
        cancel_token.clone(),
    )
    .await?;

    run_machine_a_tron_racks_test(
        &test_env,
        &bmc_address_registry,
        // DPU OOB boot and switch NVOS DHCP use the shared Underlay network.
        UNDERLAY_DHCP_RELAY_ADDRESS,
    )
    .await?;

    cancel_token.cancel();
    server_handle.wait().await?;
    test_env.db_pool.close().await;
    bmc_mock_handle.stop().await?;
    Ok(())
}

async fn run_machine_a_tron_racks_test(
    test_env: &IntegrationTestEnvironment,
    bmc_mock_registry: &BmcMockRegistry,
    underlay_dhcp_relay_address: Ipv4Addr,
) -> eyre::Result<()> {
    let gb200_rack_id = RackId::new("machine-a-tron-gb200-nvl72");
    let gb300_rack_id = RackId::new("machine-a-tron-gb300-nvl72");
    let api_addr = test_env
        .carbide_api_addrs
        .first()
        .copied()
        .context("no carbide API addresses configured")?;
    let additional_api_urls = test_env.carbide_api_addrs[1..]
        .iter()
        .map(|address| format!("https://{}:{}", address.ip(), address.port()))
        .collect();
    let mat_config = MachineATronConfig {
        racks: BTreeMap::from([
            (
                "gb200".to_string(),
                RackConfig {
                    rack_profile_id: RackProfileId::new("NVL72"),
                    ids: vec![gb200_rack_id.clone()],
                    model: RackModelConfig::WiwynnGb200Nvl72 {
                        simulation: WiwynnGb200RackConfig {
                            dpu_reboot_delay: 1,
                            host_reboot_delay: 1,
                            timing_overrides: Some(LifecycleTimingOverrides {
                                host: PartialLifecycleTimings {
                                    reboot: Some(Duration::from_secs(1)),
                                    // ZERO disables the BMC self-reset offline window entirely,
                                    // keeping ingestion at its pre-feature pace
                                    bmc_reset: Some(Duration::ZERO),
                                    ..Default::default()
                                },
                                dpu: PartialLifecycleTimings {
                                    reboot: Some(Duration::from_secs(1)),
                                    // ZERO disables the BMC self-reset offline window entirely,
                                    // keeping ingestion at its pre-feature pace
                                    bmc_reset: Some(Duration::ZERO),
                                    ..Default::default()
                                },
                            }),
                            acceleration_factor: 1.0,
                            scout_run_interval: Duration::from_secs(1),
                            discovery_retry_interval: Duration::from_millis(100),
                            bmc_dhcp_relay_address: TEST_BMC_DHCP_RELAY_ADDRESS,
                            underlay_dhcp_relay_address,
                            host_inband_dhcp_relay_address: Some(Ipv4Addr::new(10, 10, 11, 2)),
                            run_interval_working: Duration::from_millis(100),
                            run_interval_idle: Duration::from_secs(1),
                            network_status_run_interval: Duration::from_secs(1),
                            dpus_in_nic_mode: false,
                            dpu_firmware_versions: None,
                            dpu_agent_version: None,
                        },
                    },
                },
            ),
            (
                "gb300".to_string(),
                RackConfig {
                    rack_profile_id: RackProfileId::new("NVL72_GB300"),
                    ids: vec![gb300_rack_id.clone()],
                    model: RackModelConfig::LenovoGb300Nvl72 {
                        simulation: LenovoGb300RackConfig {
                            dpu_reboot_delay: 1,
                            host_reboot_delay: 1,
                            timing_overrides: Some(LifecycleTimingOverrides {
                                host: PartialLifecycleTimings {
                                    reboot: Some(Duration::from_secs(1)),
                                    // ZERO disables the BMC self-reset offline window entirely,
                                    // keeping ingestion at its pre-feature pace
                                    bmc_reset: Some(Duration::ZERO),
                                    ..Default::default()
                                },
                                dpu: PartialLifecycleTimings {
                                    reboot: Some(Duration::from_secs(1)),
                                    // ZERO disables the BMC self-reset offline window entirely,
                                    // keeping ingestion at its pre-feature pace
                                    bmc_reset: Some(Duration::ZERO),
                                    ..Default::default()
                                },
                            }),
                            acceleration_factor: 1.0,
                            scout_run_interval: Duration::from_secs(1),
                            discovery_retry_interval: Duration::from_millis(100),
                            bmc_dhcp_relay_address: TEST_BMC_DHCP_RELAY_ADDRESS,
                            underlay_dhcp_relay_address,
                            host_inband_dhcp_relay_address: Some(Ipv4Addr::new(10, 10, 11, 2)),
                            run_interval_working: Duration::from_millis(100),
                            run_interval_idle: Duration::from_secs(1),
                            network_status_run_interval: Duration::from_secs(1),
                            dpus_in_nic_mode: false,
                            dpu_firmware_versions: None,
                            dpu_agent_version: None,
                        },
                    },
                },
            ),
        ]),
        machines: BTreeMap::new(),
        carbide_api_url: format!("https://{}:{}", api_addr.ip(), api_addr.port()),
        dhcp: DhcpType::Api {},
        log_file: None,
        log_format: LogFormat::Compact,
        bmc_mock_port: 0,
        bmc_mock_certs_dir: None,
        configure_carbide_bmc_proxy_host: None,
        persist_dir: None,
        cleanup_on_quit: false,
        register_expected_machines: true,
        host_bmc_password: None,
        dpu_bmc_password: None,
        api_refresh_interval: Duration::from_millis(500),
        mock_bmc_ssh_server: false,
        enable_ipmi_simulation: false,
        hw_mac_address_ranges: None,
        mac_address_pool: None,
        ufm_mock: Default::default(),
        rms_sim: Default::default(),
    };

    let (provisionable_handles, mat_handle) = api_test_helper::machine_a_tron::run_local(
        mat_config,
        additional_api_urls,
        &test_env.root_dir,
        bmc_mock_registry.clone(),
        TEST_MAC_POOL.clone(),
    )
    .await?;

    let assertion_result = async {
        assert_eq!(provisionable_handles.len(), 36);
        let machine_ids = join_all(
            provisionable_handles
                .iter()
                .map(|machine_handle| async move {
                    machine_handle
                        .wait_until_machine_up_with_api_state("Ready", Duration::from_secs(240))
                        .await?;
                    Ok::<_, eyre::Report>(
                        machine_handle
                            .observed_machine_id()
                            .expect("Machine ID should be set if host is ready")
                            .to_string(),
                    )
                }),
        )
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(machine_ids.len(), 36);

        for (rack_id, expected_profile_id, expected_power_shelf_count) in [
            (&gb200_rack_id, "NVL72", 8),
            (&gb300_rack_id, "NVL72_GB300", 6),
        ] {
            let managed_machine_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM machines WHERE rack_id = $1")
                    .bind(rack_id.as_str())
                    .fetch_one(&test_env.db_pool)
                    .await?;
            assert_eq!(managed_machine_count, 18, "rack {rack_id}");

            let expected_machine_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM expected_machines WHERE rack_id = $1")
                    .bind(rack_id.as_str())
                    .fetch_one(&test_env.db_pool)
                    .await?;
            assert_eq!(expected_machine_count, 18, "rack {rack_id}");

            let expected_switch_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM expected_switches WHERE rack_id = $1")
                    .bind(rack_id.as_str())
                    .fetch_one(&test_env.db_pool)
                    .await?;
            assert_eq!(expected_switch_count, 9, "rack {rack_id}");

            let switch_bmc_ips: Vec<String> = sqlx::query_scalar(
                "SELECT host(mia.address) \
                 FROM expected_switches es \
                 JOIN machine_interfaces mi ON mi.mac_address = es.bmc_mac_address \
                 JOIN machine_interface_addresses mia ON mia.interface_id = mi.id \
                 WHERE es.rack_id = $1",
            )
            .bind(rack_id.as_str())
            .fetch_all(&test_env.db_pool)
            .await?;
            let switch_bmc_ips: Vec<Ipv4Addr> = switch_bmc_ips
                .into_iter()
                .map(|address| address.parse())
                .collect::<Result<_, _>>()?;
            assert_eq!(switch_bmc_ips.len(), 9, "rack {rack_id}");
            assert!(
                switch_bmc_ips
                    .iter()
                    .all(|address| TEST_BMC_NETWORK_PREFIX.contains(address)),
                "rack {rack_id} switch BMC DHCP used the Underlay relay: {switch_bmc_ips:?}"
            );

            let switch_nvos_ips: Vec<String> = sqlx::query_scalar(
                "SELECT host(mia.address) \
                 FROM expected_switches es \
                 JOIN machine_interfaces mi ON mi.mac_address = ANY(es.nvos_mac_addresses) \
                 JOIN machine_interface_addresses mia ON mia.interface_id = mi.id \
                 WHERE es.rack_id = $1",
            )
            .bind(rack_id.as_str())
            .fetch_all(&test_env.db_pool)
            .await?;
            let switch_nvos_ips: Vec<Ipv4Addr> = switch_nvos_ips
                .into_iter()
                .map(|address| address.parse())
                .collect::<Result<_, _>>()?;
            assert_eq!(switch_nvos_ips.len(), 9, "rack {rack_id}");
            assert!(
                switch_nvos_ips.iter().all(|address| {
                    address.octets()[..3] == UNDERLAY_DHCP_RELAY_ADDRESS.octets()[..3]
                }),
                "rack {rack_id} switch NVOS DHCP used the BMC relay: {switch_nvos_ips:?}"
            );

            let actual_power_shelf_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM expected_power_shelves WHERE rack_id = $1",
            )
            .bind(rack_id.as_str())
            .fetch_one(&test_env.db_pool)
            .await?;
            assert_eq!(
                actual_power_shelf_count, expected_power_shelf_count,
                "rack {rack_id}"
            );

            let rack_profile_id: String =
                sqlx::query_scalar("SELECT rack_profile_id FROM expected_racks WHERE rack_id = $1")
                    .bind(rack_id.as_str())
                    .fetch_one(&test_env.db_pool)
                    .await?;
            assert_eq!(rack_profile_id, expected_profile_id, "rack {rack_id}");
        }

        Ok::<(), eyre::Report>(())
    }
    .await;
    let shutdown_result = mat_handle.shutdown().await;

    assertion_result?;
    shutdown_result
}
