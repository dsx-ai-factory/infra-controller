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

//! Integration coverage for DPU NIC-mode handling/skipping in
//! `carbide_preingestion_manager` and a similar guard in
//! `bmc_endpoint_explorer::copy_bfb_to_dpu_rshim`.
//!
//! The BFB preingestion path assumes the DPU ARM OS will boot and emit the
//! "markers" that `in_bfb_installation_wait` polls for. When the DPU is
//! configured in NIC mode, the ARM OS never boots, so those states would
//! otherwise time out and go into `Failed` (per SLA). These tests verify
//! that our workflows don't let that happen.

use std::net::{IpAddr, Ipv4Addr};

use carbide_preingestion_manager::PreingestionManager;
use model::site_explorer::{
    BlueFieldOperatingMode, Chassis, ComputerSystem, ComputerSystemAttributes,
    EndpointExplorationReport, EndpointType, Inventory, PowerState, PreingestionState, Service,
};
use model::test_support::DpuConfig;
use rpc::forge::forge_server::Forge;
use tonic::Code;

use crate::test_support::fixture_config::FixtureDefault as _;
use crate::tests::common;

fn dpu_report(nic_mode: BlueFieldOperatingMode) -> EndpointExplorationReport {
    DpuConfig {
        nic_mode: Some(nic_mode),
        ..DpuConfig::default()
    }
    .into()
}

/// Minimal BMC report for the *host* endpoint side. The cleanup only touches
/// `pause_remediation` on this row, so the rest of the report is irrelevant;
/// we just need a row that `set_pause_remediation` can update.
fn host_bmc_report() -> EndpointExplorationReport {
    EndpointExplorationReport {
        endpoint_type: EndpointType::Bmc,
        vendor: Some(bmc_vendor::BMCVendor::Dell),
        last_exploration_error: None,
        last_exploration_latency: None,
        managers: vec![],
        systems: vec![ComputerSystem {
            id: String::new(),
            ethernet_interfaces: vec![],
            manufacturer: Some("Dell".to_string()),
            model: Some("PowerEdge R760".to_string()),
            serial_number: None,
            attributes: ComputerSystemAttributes {
                nic_mode: None,
                is_infinite_boot_enabled: Some(true),
            },
            pcie_devices: vec![],
            base_mac: None,
            power_state: PowerState::On,
            sku: None,
            boot_order: None,
            bios_version: None,
            serial_console_ssh_port: None,
        }],
        chassis: vec![Chassis {
            id: String::new(),
            manufacturer: Some("Dell".to_string()),
            model: Some("PowerEdge R760".to_string()),
            part_number: None,
            serial_number: None,
            network_adapters: vec![],
            compute_tray_index: None,
            physical_slot_number: None,
            revision_id: None,
            topology_id: None,
        }],
        service: vec![Service {
            id: String::new(),
            inventories: vec![Inventory {
                id: "iDRAC".to_string(),
                description: None,
                version: Some("6.10.00.00".to_string()),
                release_date: None,
            }],
        }],
        machine_id: None,
        versions: Default::default(),
        model: None,
        machine_setup_status: None,
        secure_boot_status: None,
        lockdown_status: None,
        power_shelf_id: None,
        switch_id: None,
        compute_tray_index: None,
        physical_slot_number: None,
        revision_id: None,
        topology_id: None,
        remediation_error: None,
    }
}

const DPU_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 100));
const HOST_BMC_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 200));

fn build_preingestion_manager(env: &common::api_fixtures::TestEnv) -> PreingestionManager {
    PreingestionManager::new(
        env.pool.clone(),
        env.config.preingestion_manager(),
        env.redfish_sim.clone(),
        env.test_meter.meter(),
        None,
        None,
        None,
        env.api.work_lock_manager_handle.clone(),
        env.config.ntp_servers.clone(),
    )
}

/// Seed a DPU reporting NIC operating mode plus a host BMC with
/// `pause_remediation = true`. The host
/// row's flag is the primary marker that the skip's cleanup ran,
/// since `set_pause_remediation(host_bmc_ip, false)` is the only thing the
/// skip does to that row.
async fn seed_nic_mode_pair(env: &common::api_fixtures::TestEnv) {
    let mut txn = env.pool.begin().await.unwrap();
    db::explored_endpoints::insert(
        DPU_IP,
        &dpu_report(BlueFieldOperatingMode::Nic),
        false,
        &mut txn,
    )
    .await
    .unwrap();
    db::explored_endpoints::insert(HOST_BMC_IP, &host_bmc_report(), false, &mut txn)
        .await
        .unwrap();
    db::explored_endpoints::set_pause_remediation(HOST_BMC_IP, true, &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();
}

async fn fetch(
    env: &common::api_fixtures::TestEnv,
    ip: IpAddr,
) -> model::site_explorer::ExploredEndpoint {
    let mut txn = env.pool.begin().await.unwrap();
    db::explored_endpoints::find_all_by_ip(ip, &mut txn)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("endpoint not found")
}

#[crate::sqlx_test]
async fn test_preingestion_nic_mode_skips_bfb_recovery_needed(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let mgr = build_preingestion_manager(&env);

    seed_nic_mode_pair(&env).await;
    let mut txn = pool.begin().await?;
    db::explored_endpoints::set_preingestion_bfb_recovery_needed(
        DPU_IP,
        "test seed".to_string(),
        HOST_BMC_IP,
        false,
        &mut txn,
    )
    .await?;
    txn.commit().await?;

    mgr.run_single_iteration().await?;

    let dpu_after = fetch(&env, DPU_IP).await;
    let host_after = fetch(&env, HOST_BMC_IP).await;

    assert!(
        matches!(dpu_after.preingestion_state, PreingestionState::Complete),
        "DPU should be marked Complete; got {:?}",
        dpu_after.preingestion_state,
    );
    assert!(
        dpu_after.waiting_for_explorer_refresh,
        "DPU should be waiting for site-explorer to re-read its state",
    );
    assert!(
        !host_after.pause_remediation,
        "host BMC pause_remediation should be cleared so remediation can resume",
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_preingestion_nic_mode_skips_bfb_installation_wait(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let mgr = build_preingestion_manager(&env);

    seed_nic_mode_pair(&env).await;
    let mut txn = pool.begin().await?;
    db::explored_endpoints::set_preingestion_bfb_installation_wait(DPU_IP, HOST_BMC_IP, &mut txn)
        .await?;
    txn.commit().await?;

    mgr.run_single_iteration().await?;

    let dpu_after = fetch(&env, DPU_IP).await;
    let host_after = fetch(&env, HOST_BMC_IP).await;

    assert!(
        matches!(dpu_after.preingestion_state, PreingestionState::Complete),
        "DPU should be marked Complete; got {:?}",
        dpu_after.preingestion_state,
    );
    assert!(dpu_after.waiting_for_explorer_refresh);
    assert!(!host_after.pause_remediation);

    Ok(())
}

/// Small test to make sure BfbCopyInProgress *doesn't* get skipped
/// by our NIC mode checks. We don't want to skip if we're copying a BFB
/// or waiting for power cycling.
#[crate::sqlx_test]
async fn test_preingestion_nic_mode_does_not_skip_bfb_copy_in_progress(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let mgr = build_preingestion_manager(&env);

    seed_nic_mode_pair(&env).await;
    let mut txn = pool.begin().await?;
    db::explored_endpoints::set_preingestion_bfb_copy_in_progress(DPU_IP, HOST_BMC_IP, &mut txn)
        .await?;
    txn.commit().await?;

    mgr.run_single_iteration().await?;

    let dpu_after = fetch(&env, DPU_IP).await;
    let host_after = fetch(&env, HOST_BMC_IP).await;

    assert!(
        !matches!(dpu_after.preingestion_state, PreingestionState::Complete),
        "BfbCopyInProgress must not be skipped to Complete by the NIC-mode guard; \
         the normal handler should run instead. got {:?}",
        dpu_after.preingestion_state,
    );
    assert!(
        host_after.pause_remediation,
        "host BMC pause_remediation should remain set; only the skip cleanup clears it",
    );

    Ok(())
}

/// The handler-side guard. preingestion-manager would collapse a freshly
/// inserted `BfbRecoveryNeeded` for a NIC-mode DPU into `Complete` on its
/// next tick, so accepting the request would be a silent no-op for the
/// operator. The handler must reject up front with an actionable message
/// pointing at the admin-CLI policy flag.
#[crate::sqlx_test]
async fn test_copy_bfb_to_dpu_rshim_rejects_nic_mode_dpu(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;

    let mut txn = pool.begin().await?;
    db::explored_endpoints::insert(
        DPU_IP,
        &dpu_report(BlueFieldOperatingMode::Nic),
        false,
        &mut txn,
    )
    .await?;
    txn.commit().await?;

    let status = env
        .api
        .copy_bfb_to_dpu_rshim(tonic::Request::new(rpc::forge::CopyBfbToDpuRshimRequest {
            ssh_request: Some(rpc::forge::SshRequest {
                endpoint_request: Some(rpc::forge::BmcEndpointRequest {
                    ip_address: DPU_IP.to_string(),
                    mac_address: None,
                }),
            }),
            host_bmc_ip: HOST_BMC_IP.to_string(),
            pre_copy_powercycle: false,
        }))
        .await
        .expect_err("NIC-mode DPU should be rejected before host endpoint lookup");

    assert_eq!(status.code(), Code::InvalidArgument);
    let msg = status.message();
    assert!(
        msg.contains("NIC mode"),
        "error should name NIC mode; got: {msg}",
    );
    assert!(
        msg.contains("--dpu-policy manage"),
        "error should point at the operator-facing `--dpu-policy` flag; got: {msg}",
    );

    Ok(())
}
