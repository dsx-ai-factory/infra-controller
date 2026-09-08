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
use std::fs;
use std::time::Duration;

use carbide_firmware::test_support::script_setup;
use carbide_machine_controller::config::{FirmwareGlobal, TimePeriod};
use carbide_machine_controller::handler::MAX_NEW_FIRMWARE_REPORTED_RESET_RETRIES;
use carbide_uuid::machine::HostMachineId;
use common::api_fixtures::instance::TestInstance;
use common::api_fixtures::{
    self, TestEnv, TestManagedHost, create_test_env_with_overrides, get_config,
};
use model::firmware::{Firmware, FirmwareComponent, FirmwareComponentType, FirmwareEntry};
use model::instance::status::tenant::TenantState;
use model::machine::{
    HostReprovisionState, InstanceState, MAX_FIRMWARE_UPGRADE_RETRIES, ManagedHostState,
};
use model::machine_update_module::HOST_FW_UPDATE_HEALTH_REPORT_SOURCE;
use model::test_support::HardwareInfoTemplate;
use regex::Regex;
use rpc::forge::forge_server::Forge;
use rpc::model::instance::snapshot::instance_snapshot_derive_status;
use temp_dir::TempDir;
use tokio::time::sleep;
use tonic::Request;

use crate::CarbideResult;
use crate::machine_update_manager::MachineUpdateManager;
use crate::tests::common;
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_managed_host_with_hardware_info_template, create_test_env,
};

#[crate::sqlx_test]
async fn test_postingestion_bmc_upgrade(pool: sqlx::PgPool) -> CarbideResult<()> {
    // Keep the large nested futures on the heap so the test stays within libtest's thread stack.
    // Create an environment with one managed host in the ready state.
    let env = Box::pin(create_test_env(pool.clone())).await;

    let mh = Box::pin(common::api_fixtures::create_managed_host(&env)).await;

    // Create and start an update manager
    let update_manager = MachineUpdateManager::new(
        env.pool.clone(),
        env.config.clone(),
        env.test_meter.meter(),
        env.api.work_lock_manager_handle.clone(),
        None,
    );

    Box::pin(test_postingestion_bmc_upgrade_uefi(
        &env,
        &mh,
        &update_manager,
    ))
    .await?;
    Box::pin(test_postingestion_bmc_upgrade_bmc(&env, &mh)).await?;
    Box::pin(test_postingestion_bmc_upgrade_complete(
        &env,
        &mh,
        &update_manager,
    ))
    .await
}

async fn test_postingestion_bmc_upgrade_uefi(
    env: &TestEnv,
    mh: &TestManagedHost,
    update_manager: &MachineUpdateManager,
) -> CarbideResult<()> {
    // Update manager should notice that the host is underversioned, setting the request to update it
    update_manager.run_single_iteration().await.unwrap();

    // Check that we're properly marking it as upgrade needed
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    assert!(host.host_reprovision_requested.is_some());
    txn.commit().await.unwrap();

    // Now we want a tick of the state machine
    env.run_machine_state_controller_iteration().await;

    // Wait a bit for upload to complete
    sleep(Duration::from_millis(6000)).await;

    // Now we want a tick of the state machine
    env.run_machine_state_controller_iteration().await;

    // It should have "started" a UEFI upgrade
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.host_reprovision_requested.is_some());
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::WaitingForFirmwareUpgrade { firmware_type, .. } = reprovision_state
    else {
        panic!("Not in WaitingForFirmwareUpgrade");
    };
    assert_eq!(firmware_type, &FirmwareComponentType::Uefi);
    txn.commit().await.unwrap();

    // The faked Redfish task will immediately show as completed, but we won't proceed further because "site explorer" (ie us) has not re-reported the info.
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::ResetForNewFirmware { .. } = reprovision_state else {
        panic!("Not in reset {reprovision_state:?}");
    };
    txn.commit().await.unwrap();

    // Another state machine pass
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::NewFirmwareReportedWait { .. } = reprovision_state else {
        panic!("Not in waiting {reprovision_state:?}");
    };

    // "Site explorer" pass
    let endpoints = db::explored_endpoints::find_by_ips(
        txn.as_mut(),
        vec![host.status.bmc_info.ip_addr().unwrap()],
    )
    .await
    .unwrap();
    let mut endpoint = endpoints.into_iter().next().unwrap();
    endpoint.report.service[0].inventories[1].version = Some("1.13.2".to_string());
    endpoint
        .report
        .versions
        .insert(FirmwareComponentType::Uefi, "1.13.2".to_string());
    db::explored_endpoints::try_update(
        host.status.bmc_info.ip_addr().unwrap(),
        endpoint.report_version,
        &endpoint.report,
        false,
        &mut txn,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // Another state machine pass
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::CheckingFirmwareRepeatV2 { .. } = reprovision_state else {
        panic!("Not in reset {reprovision_state:?}");
    };
    txn.commit().await.unwrap();

    Ok(())
}

async fn test_postingestion_bmc_upgrade_bmc(
    env: &TestEnv,
    mh: &TestManagedHost,
) -> CarbideResult<()> {
    // Now we want a tick of the state machine, going to upload
    env.run_machine_state_controller_iteration().await;

    // Wait a bit for upload to complete
    sleep(Duration::from_millis(6000)).await;

    // Another state machine pass
    env.run_machine_state_controller_iteration().await;

    // It should have "started" a BMC upgrade now
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.host_reprovision_requested.is_some());
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::WaitingForFirmwareUpgrade {
        firmware_type,
        firmware_number,
        ..
    } = reprovision_state
    else {
        panic!("Not in WaitingForFirmwareUpgrade");
    };
    assert_eq!(firmware_type, &FirmwareComponentType::Bmc);
    assert_eq!(firmware_number, &Some(0));
    txn.commit().await.unwrap();

    // Another state machine pass
    // WaitingForFirmwareUpgrade -> CheckingFirmware (firmware_number: 1)
    env.run_machine_state_controller_iteration().await;

    // Another state machine pass
    // CheckingFirmware -> WaitingForUpload (firmware_number: 1)
    env.run_machine_state_controller_iteration().await;

    // Wait a bit for upload to complete
    sleep(Duration::from_millis(6000)).await;

    // WaitingForUpload -> WaitingForFirmwareUpgrade
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.host_reprovision_requested.is_some());
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::WaitingForFirmwareUpgrade {
        firmware_type,
        firmware_number,
        ..
    } = reprovision_state
    else {
        panic!("Not in WaitingForFirmwareUpgrade {reprovision_state:?}");
    };
    assert_eq!(firmware_type, &FirmwareComponentType::Bmc);
    assert_eq!(*firmware_number, Some(1));
    txn.commit().await.unwrap();

    // Another state machine pass
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::ResetForNewFirmware { .. } = reprovision_state else {
        panic!("Not in reset {reprovision_state:?}");
    };

    // "Site explorer" pass to indicate that we're at the desired version
    let endpoints = db::explored_endpoints::find_by_ips(
        txn.as_mut(),
        vec![host.status.bmc_info.ip_addr().unwrap()],
    )
    .await?;
    let mut endpoint = endpoints.into_iter().next().unwrap();
    endpoint.report.service[0].inventories[0].version = Some("6.00.30.00".to_string());
    endpoint
        .report
        .versions
        .insert(FirmwareComponentType::Bmc, "6.00.30.00".to_string());
    db::explored_endpoints::try_update(
        host.status.bmc_info.ip_addr().unwrap(),
        endpoint.report_version,
        &endpoint.report,
        false,
        &mut txn,
    )
    .await?;
    db::machine_topology::update_firmware_version_by_machine_id(
        &mut txn,
        &host.id,
        "6.00.30.00",
        "1.2.3",
    )
    .await?;
    txn.commit().await.unwrap();
    // Another state machine pass
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::NewFirmwareReportedWait { .. } = reprovision_state else {
        panic!("Not in waiting {reprovision_state:?}");
    };
    txn.commit().await.unwrap();

    // Another state machine pass
    env.run_machine_state_controller_iteration().await;

    Ok(())
}

async fn test_postingestion_bmc_upgrade_complete(
    env: &TestEnv,
    mh: &TestManagedHost,
    update_manager: &MachineUpdateManager,
) -> CarbideResult<()> {
    // It should be checking
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::CheckingFirmwareRepeatV2 { .. } = reprovision_state else {
        panic!("Not in checking");
    };
    txn.commit().await.unwrap();

    // Another state machine pass
    env.run_machine_state_controller_iteration().await;

    // Now we should be back waiting for lockdown to resolve
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostInit { .. } = host.current_state() else {
        panic!("Not in HostInit");
    };
    txn.commit().await.unwrap();

    // Step until we reach ready
    env.run_machine_state_controller_iteration().await;

    // Now let update manager run again, it should not put us back to reprovisioning.
    update_manager.run_single_iteration().await?;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    assert!(host.host_reprovision_requested.is_none()); // Should be cleared or we'd right back in
    assert!(host.status.update_complete);
    let reqs = db::host_machine_update::find_upgrade_needed(&mut txn, true, false).await?;
    assert!(reqs.is_empty());
    txn.commit().await.unwrap();

    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_pending_host_firmware_update_count")
            .unwrap(),
        "0"
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_active_host_firmware_update_count")
            .unwrap(),
        "0"
    );

    // Validate update_firmware_version_by_machine_id behavior
    assert_eq!(
        host.status.bmc_info.firmware_version,
        Some("6.00.30.00".to_string())
    );
    assert_eq!(
        host.status
            .hardware_info
            .as_ref()
            .unwrap()
            .dmi_data
            .clone()
            .unwrap()
            .bios_version,
        "1.2.3".to_string()
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_host_fw_upgrade_enabledisable_global_enabled(
    pool: sqlx::PgPool,
) -> CarbideResult<()> {
    let (env, mh) = test_host_fw_upgrade_enabledisable_generic(pool, true).await?;
    let host_machine_id = mh.host().id;

    // Check that if it's globally enabled but specifically disabled, we don't request updates.
    let mut txn = env.pool.begin().await.unwrap();
    db::machine::set_firmware_autoupdate(&mut txn, &host_machine_id, Some(false)).await?;
    txn.commit().await.unwrap();

    // Create and start an update manager
    let update_manager = MachineUpdateManager::new(
        env.pool.clone(),
        env.config.clone(),
        env.test_meter.meter(),
        env.api.work_lock_manager_handle.clone(),
        None,
    );
    update_manager.run_single_iteration().await?;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    assert!(host.host_reprovision_requested.is_none());

    // Now switch it to unspecified and it should get a request
    db::machine::set_firmware_autoupdate(&mut txn, &host_machine_id, None).await?;
    txn.commit().await.unwrap();

    update_manager.run_single_iteration().await?;
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    assert!(host.host_reprovision_requested.is_some());
    txn.commit().await.unwrap();

    Ok(())
}

#[crate::sqlx_test]
async fn test_host_fw_upgrade_enabledisable_global_disabled(
    pool: sqlx::PgPool,
) -> CarbideResult<()> {
    let (env, mh) = test_host_fw_upgrade_enabledisable_generic(pool, false).await?;
    let host_machine_id = mh.host().id;
    // Create and start an update manager
    let update_manager = MachineUpdateManager::new(
        env.pool.clone(),
        env.config.clone(),
        env.test_meter.meter(),
        env.api.work_lock_manager_handle.clone(),
        None,
    );
    update_manager.run_single_iteration().await?;

    // Globally disabled, so it should not have requested an update
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    assert!(host.host_reprovision_requested.is_none());

    tracing::info!("setting update");
    // Now specifically enable it, and an update should be requested.
    db::machine::set_firmware_autoupdate(&mut txn, &host_machine_id, Some(true)).await?;
    txn.commit().await.unwrap();

    tracing::info!("run iteration");
    update_manager.run_single_iteration().await?;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    assert!(host.host_reprovision_requested.is_some());
    txn.commit().await.unwrap();

    Ok(())
}

async fn test_host_fw_upgrade_enabledisable_generic(
    pool: sqlx::PgPool,
    global_enabled: bool,
) -> CarbideResult<(TestEnv, TestManagedHost)> {
    // Create an environment with one managed host in the ready state.  Tweak the default config to enable or disable firmware global autoupdate.
    let mut config = get_config();
    config.firmware_global.autoupdate = global_enabled;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;

    let mh = common::api_fixtures::create_managed_host(&env).await;

    Ok((env, mh))
}

#[test]
fn test_merge_firmware_configs() -> Result<(), eyre::Report> {
    let tmpdir = TempDir::with_prefix("test_merge_firmware_configs")?;

    // B_1 comes later alphabetically but because it's written first, should be parsed first
    test_merge_firmware_configs_write(
        &tmpdir,
        "dir_B_1",
        r#"
vendor = "Dell"
model = "PowerEdge R750"
[components.uefi]
current_version_reported_as = "^Installed-.*__BIOS.Setup."
preingest_upgrade_when_below = "1.0"
known_firmware = [
    # Set version to match the version that the firmware will give, and for filename change filename.bin to the filename you specified in Dockerfile.  Leave everything else as it is.
    { version = "1.0", filename = "/opt/fw/dell-r750-bmc-1.0/filename.bin", default = true },
]
    "#,
    )?;
    // Even though the file modification time has a precision of nanoseconds, the two files can have matching times, so we have to wait a bit.
    std::thread::sleep(Duration::from_millis(100));
    test_merge_firmware_configs_write(
        &tmpdir,
        "dir_A_2",
        r#"
vendor = "Dell"
model = "PowerEdge R750"
[components.uefi]
current_version_reported_as = "^Installed-.*__BIOS.Setup."
preingest_upgrade_when_below = "1.1"
known_firmware = [
    # Set version to match the version that the firmware will give, and for filename change filename.bin to the filename you specified in Dockerfile.  Leave everything else as it is.
    { version = "2.0", filename = "/opt/fw/dell-r750-bmc-2.0/filename.bin", default = true },
]
    "#,
    )?;
    // And a directory that has no metadata, just to make sure we don't panic
    let mut dir = tmpdir.path().to_path_buf();
    dir.push("bad");
    fs::create_dir_all(dir.clone())?;

    let mut cfg = api_fixtures::get_config();
    cfg.firmware_global.firmware_directory = tmpdir.path().to_path_buf();
    let cfg = cfg.get_firmware_config();

    let model = cfg
        .create_snapshot()
        .find(bmc_vendor::BMCVendor::Dell, "PowerEdge R750")
        .unwrap();

    drop(tmpdir);

    assert_eq!(
        model
            .components
            .get(&FirmwareComponentType::Bmc)
            .unwrap()
            .known_firmware
            .len(),
        3
    );
    let uefi = model.components.get(&FirmwareComponentType::Uefi).unwrap();
    assert_eq!(uefi.preingest_upgrade_when_below, Some("1.1".to_string()));

    assert_eq!(uefi.known_firmware.len(), 3);
    for x in &uefi.known_firmware {
        match x.version.as_str() {
            "1.0" => {
                assert!(!x.default);
            }
            "2.0" => {
                assert!(x.default);
            }
            "1.13.2" => {
                assert!(!x.default);
            }
            _ => {
                panic!("Wrong version {x:?}");
            }
        }
    }

    Ok(())
}

fn test_merge_firmware_configs_write(
    tmpdir: &TempDir,
    name: &str,
    contents: &str,
) -> Result<(), eyre::Report> {
    let mut dir = tmpdir.path().to_path_buf();
    dir.push(name);
    fs::create_dir_all(dir.clone())?;
    let mut file = dir.clone();
    file.push("metadata.toml");
    fs::write(file, contents)?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_instance_upgrading_false(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_instance_upgrading_actual(&pool, false).await.unwrap();
    Ok(())
}
#[crate::sqlx_test]
async fn test_instance_upgrading_true(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_instance_upgrading_actual(&pool, true).await.unwrap();
    Ok(())
}

async fn test_instance_upgrading_actual(
    pool: &sqlx::PgPool,
    with_ignore_request: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = common::api_fixtures::get_config();
    if with_ignore_request {
        config.machine_updater.instance_autoreboot_period = Some(TimePeriod {
            start: chrono::Utc::now()
                .checked_add_signed(chrono::TimeDelta::new(-300, 0).unwrap())
                .unwrap(),
            end: chrono::Utc::now()
                .checked_add_signed(chrono::TimeDelta::new(300, 0).unwrap())
                .unwrap(),
        });
    }
    let env = common::api_fixtures::create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config),
    )
    .await;

    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    // Create and start an update manager
    let update_manager = MachineUpdateManager::new(
        env.pool.clone(),
        env.config.clone(),
        env.test_meter.meter(),
        env.api.work_lock_manager_handle.clone(),
        None,
    );

    // Single iteration now starts it
    update_manager.run_single_iteration().await.unwrap();

    if with_ignore_request {
        // Shouldn't need a "manual" OK
    } else {
        // A tick of the state machine, but we don't start anything yet and it's still in assigned/ready
        env.run_machine_state_controller_iteration().await;
        let mut txn = env.pool.begin().await.unwrap();
        let host = mh.host().db_machine(&mut txn).await;
        let ManagedHostState::Assigned { instance_state } = host.state.clone().value else {
            panic!("Unexpected state {:?}", host.state);
        };
        let InstanceState::Ready = instance_state else {
            panic!("Unexpecte instance state {:?}", host.state);
        };
        txn.commit().await.unwrap();

        // Simulate a tenant OKing the request
        let request = rpc::forge::InstancePowerRequest {
            instance_id: tinstance.id.into(),
            operation: rpc::forge::instance_power_request::Operation::PowerReset.into(),
            boot_with_custom_ipxe: false,
            apply_updates_on_reboot: true,
        };
        let request = Request::new(request);
        env.api.invoke_instance_power(request).await.unwrap();
    }

    // Box::pin to keep the nested future on the heap and limit the outer state machine size.
    Box::pin(test_instance_upgrading_actual_part_2(
        &env,
        &mh,
        &tinstance,
        &update_manager,
    ))
    .await
}

async fn test_instance_upgrading_actual_part_2(
    env: &TestEnv,
    mh: &TestManagedHost,
    tinstance: &TestInstance<'_, '_>,
    update_manager: &MachineUpdateManager,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await.unwrap();
    // Check that the TenantState is what we expect based on the instance/machine state.
    let host = mh.host().db_machine(&mut txn).await;
    let instance = tinstance.db_instance(&mut txn).await;
    txn.commit().await.unwrap();

    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &instance,
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        TenantState::Configuring
    );

    // A tick of the state machine, now we begin.
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    mh.network_configured(env).await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    mh.network_configured(env).await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::Assigned { instance_state } = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };
    let InstanceState::BootingWithDiscoveryImage { .. } = instance_state else {
        panic!("Unexpected instance state {:?}", host.state);
    };

    let instance = tinstance.db_instance(&mut txn).await;
    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &instance,
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        TenantState::Updating
    );

    assert!(host.host_reprovision_requested.is_some());
    println!("{:?}", host.health_reports);
    assert!(
        host.health_reports
            .merges
            .contains_key(HOST_FW_UPDATE_HEALTH_REPORT_SOURCE)
    );
    txn.commit().await.unwrap();

    // Simulate agent saying it's booted so we can continue
    mh.host().forge_agent_control().await;
    sleep(std::time::Duration::from_secs(2)).await;

    env.run_machine_state_controller_iteration().await;

    // Should check firmware next
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.host_reprovision_requested.is_some());
    let ManagedHostState::Assigned { instance_state } = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };
    let InstanceState::HostReprovision { reprovision_state } = instance_state else {
        panic!("Unexpected state {:?}", host.state)
    };
    let HostReprovisionState::CheckingFirmwareV2 { .. } = reprovision_state else {
        panic!("Unexpected state {:?}", host.state)
    };
    assert!(host.host_reprovision_requested.is_some());

    let instance = tinstance.db_instance(&mut txn).await;
    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &instance,
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        TenantState::Updating
    );
    txn.commit().await.unwrap();

    let request = Request::new(mh.id.into());
    env.api.reset_host_reprovisioning(request).await?;

    // Next one should start a UEFI upgrade
    env.run_machine_state_controller_iteration().await;

    // Wait a second for the thread to run, and the next should show it complete
    sleep(Duration::from_millis(6000)).await;
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.host_reprovision_requested.is_some());
    let ManagedHostState::Assigned { instance_state } = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };
    let InstanceState::HostReprovision { reprovision_state } = instance_state else {
        panic!("Unexpected state {:?}", host.state)
    };
    let HostReprovisionState::WaitingForFirmwareUpgrade { firmware_type, .. } = reprovision_state
    else {
        panic!("Not in WaitingForFirmwareUpgrade: {:?}", host.state);
    };
    assert_eq!(firmware_type, FirmwareComponentType::Uefi);

    // Verify expected TenantState
    let instance = tinstance.db_instance(&mut txn).await;
    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &instance,
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        TenantState::Updating
    );

    txn.commit().await.unwrap();

    // The faked Redfish task will immediately show as completed, but we won't proceed further because "site explorer" (ie us) has not re-reported the info.
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::Assigned { instance_state } = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };
    let InstanceState::HostReprovision { reprovision_state } = instance_state else {
        panic!("Unexpected state {:?}", host.state)
    };
    let HostReprovisionState::ResetForNewFirmware { .. } = reprovision_state else {
        panic!("Not in reset {reprovision_state:?}");
    };

    // Check that the TenantState is what we expect based on the instance/machine state.
    let instance = tinstance.db_instance(&mut txn).await;
    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &instance,
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        TenantState::Updating
    );

    txn.commit().await.unwrap();

    // Another state machine pass
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::Assigned { instance_state } = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };
    let InstanceState::HostReprovision { reprovision_state } = instance_state else {
        panic!("Unexpected state {:?}", host.state)
    };
    let HostReprovisionState::NewFirmwareReportedWait { .. } = reprovision_state else {
        panic!("Not in waiting {reprovision_state:?}");
    };

    // "Site explorer" pass
    let endpoints = db::explored_endpoints::find_by_ips(
        txn.as_mut(),
        vec![host.status.bmc_info.ip_addr().unwrap()],
    )
    .await
    .unwrap();
    let mut endpoint = endpoints.into_iter().next().unwrap();
    endpoint.report.service[0].inventories[1].version = Some("1.13.2".to_string());
    endpoint
        .report
        .versions
        .insert(FirmwareComponentType::Uefi, "1.13.2".to_string());
    db::explored_endpoints::try_update(
        host.status.bmc_info.ip_addr().unwrap(),
        endpoint.report_version,
        &endpoint.report,
        false,
        &mut txn,
    )
    .await
    .unwrap();

    // Check that the TenantState is what we expect based on the instance/machine state.
    let host = mh.host().db_machine(&mut txn).await;

    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &instance,
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        TenantState::Updating
    );

    txn.commit().await.unwrap();

    // Another state machine pass
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::Assigned { instance_state } = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };
    let InstanceState::HostReprovision { reprovision_state } = instance_state else {
        panic!("Unexpected state {:?}", host.state)
    };
    let HostReprovisionState::CheckingFirmwareRepeatV2 { .. } = reprovision_state else {
        panic!("Not in reset {reprovision_state:?}");
    };

    // Check that the TenantState is what we expect based on the instance/machine state.
    let instance = tinstance.db_instance(&mut txn).await;
    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &instance,
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        TenantState::Updating
    );
    txn.commit().await.unwrap();

    // Box::pin to keep the nested future on the heap and limit the outer state machine size.
    Box::pin(test_instance_upgrading_actual_part_3(
        env,
        mh,
        tinstance,
        update_manager,
    ))
    .await
}

async fn test_instance_upgrading_actual_part_3(
    env: &TestEnv,
    mh: &TestManagedHost,
    tinstance: &TestInstance<'_, '_>,
    update_manager: &MachineUpdateManager,
) -> Result<(), Box<dyn std::error::Error>> {
    // Another state machine pass, we're do a 2 chained uploads
    env.run_machine_state_controller_iteration().await;
    // Wait a second for the thread to run, and the next should show it complete
    sleep(Duration::from_millis(6000)).await;
    env.run_machine_state_controller_iteration().await;

    // It should have "started" a BMC upgrade now (first file out of 2)
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.host_reprovision_requested.is_some());
    let ManagedHostState::Assigned { instance_state } = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };
    let InstanceState::HostReprovision { reprovision_state } = instance_state else {
        panic!("Unexpected state {:?}", host.state)
    };
    let HostReprovisionState::WaitingForFirmwareUpgrade {
        firmware_type,
        firmware_number,
        ..
    } = reprovision_state
    else {
        panic!("Not in WaitingForFirmwareUpgrade");
    };
    assert_eq!(firmware_type, FirmwareComponentType::Bmc);
    assert_eq!(firmware_number, Some(0));
    // Check that the TenantState is what we expect based on the instance/machine state.
    let instance = tinstance.db_instance(&mut txn).await;

    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &instance,
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        TenantState::Updating
    );

    txn.commit().await.unwrap();

    // Another state machine pass
    // WaitingForFirmwareUpgrade -> CheckingFirmware (firmware_number: 1)
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::Assigned { instance_state } = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };
    let InstanceState::HostReprovision { reprovision_state } = instance_state else {
        panic!("Unexpected state {:?}", host.state)
    };
    let HostReprovisionState::CheckingFirmwareV2 {
        firmware_number, ..
    } = reprovision_state
    else {
        panic!("Not in CheckingFirmware: {reprovision_state:?}");
    };
    assert_eq!(firmware_number, Some(1));

    // Another state machine pass
    // CheckingFirmware -> WaitingForUpload (firmware_number: 1)
    env.run_machine_state_controller_iteration().await;
    sleep(Duration::from_millis(6000)).await;
    // Another state machine pass
    // WaitingForUpload -> WaitingForFirmwareUpgrade
    env.run_machine_state_controller_iteration().await;
    // Another state machine pass
    // WaitingForFirmwareUpgrade -> ResetForNewFirmware
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::Assigned { instance_state } = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };
    let InstanceState::HostReprovision { reprovision_state } = instance_state else {
        panic!("Unexpected state {:?}", host.state)
    };
    let HostReprovisionState::ResetForNewFirmware {
        firmware_number, ..
    } = reprovision_state
    else {
        panic!("Not in reset {reprovision_state:?}");
    };

    assert_eq!(firmware_number, Some(1));

    // Check that the TenantState is what we expect based on the instance/machine state.
    let instance = tinstance.db_instance(&mut txn).await;

    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &instance,
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        TenantState::Updating
    );

    // "Site explorer" pass to indicate that we're at the desired version
    let endpoints = db::explored_endpoints::find_by_ips(
        txn.as_mut(),
        vec![host.status.bmc_info.ip_addr().unwrap()],
    )
    .await
    .unwrap();
    let mut endpoint = endpoints.into_iter().next().unwrap();
    endpoint.report.service[0].inventories[0].version = Some("6.00.30.00".to_string());
    endpoint
        .report
        .versions
        .insert(FirmwareComponentType::Bmc, "6.00.30.00".to_string());
    db::explored_endpoints::try_update(
        host.status.bmc_info.ip_addr().unwrap(),
        endpoint.report_version,
        &endpoint.report,
        false,
        &mut txn,
    )
    .await
    .unwrap();
    db::machine_topology::update_firmware_version_by_machine_id(
        &mut txn,
        &host.id,
        "6.00.30.00",
        "1.2.3",
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    // Another state machine pass
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::Assigned { instance_state } = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };
    let InstanceState::HostReprovision { reprovision_state } = instance_state else {
        panic!("Unexpected state {:?}", host.state)
    };
    let HostReprovisionState::NewFirmwareReportedWait { .. } = reprovision_state else {
        panic!("Not in waiting {reprovision_state:?}");
    };

    // Check that the TenantState is what we expect based on the instance/machine state.
    let instance = tinstance.db_instance(&mut txn).await;

    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &instance,
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        TenantState::Updating
    );

    // Another state machine pass
    env.run_machine_state_controller_iteration().await;

    // It should be checking
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::Assigned { instance_state } = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };
    let InstanceState::HostReprovision { reprovision_state } = instance_state else {
        panic!("Unexpected state {:?}", host.state)
    };
    let HostReprovisionState::CheckingFirmwareRepeatV2 { .. } = reprovision_state else {
        panic!("Not in checking");
    };

    // Check that the TenantState is what we expect based on the instance/machine state.
    let instance = tinstance.db_instance(&mut txn).await;

    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &instance,
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        TenantState::Updating
    );

    txn.commit().await.unwrap();

    // Another state machine pass, and we should be complete
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::Assigned { instance_state } = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };
    let InstanceState::Ready = instance_state else {
        panic!("Unexpected state {:?}", host.state)
    };

    // Check that the TenantState is what we expect based on the instance/machine state.
    let instance = tinstance.db_instance(&mut txn).await;

    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &instance,
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        TenantState::Configuring
    );

    update_manager.run_single_iteration().await.unwrap();

    assert!(host.host_reprovision_requested.is_none()); // Should be cleared
    let reqs = db::host_machine_update::find_upgrade_needed(&mut txn, true, false)
        .await
        .unwrap();
    assert!(reqs.is_empty());
    let host = mh.host().db_machine(&mut txn).await;

    // Make sure TenantState agrees
    let instance = tinstance.db_instance(&mut txn).await;

    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &instance,
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        TenantState::Configuring
    );

    txn.commit().await.unwrap();

    // Validate update_firmware_version_by_machine_id behavior
    assert_eq!(
        host.status.bmc_info.firmware_version,
        Some("6.00.30.00".to_string())
    );
    assert_eq!(
        host.status
            .hardware_info
            .as_ref()
            .unwrap()
            .dmi_data
            .clone()
            .unwrap()
            .bios_version,
        "1.2.3".to_string()
    );
    assert!(
        !host
            .health_reports
            .merges
            .contains_key(HOST_FW_UPDATE_HEALTH_REPORT_SOURCE)
    );
    Ok(())
}

#[crate::sqlx_test]
async fn test_script_upgrade(pool: sqlx::PgPool) -> CarbideResult<()> {
    let (_tmpdir, host_models) = script_setup();
    let mut config = get_config();
    config.host_models = host_models;

    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;

    let mh = common::api_fixtures::create_managed_host(&env).await;

    // Create and start an update manager
    let update_manager = MachineUpdateManager::new(
        env.pool.clone(),
        env.config.clone(),
        env.test_meter.meter(),
        env.api.work_lock_manager_handle.clone(),
        None,
    );
    // Update manager should notice that the host is underversioned, setting the request to update it
    update_manager.run_single_iteration().await.unwrap();

    // Check that we're properly marking it as upgrade needed
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    assert!(host.host_reprovision_requested.is_some());
    txn.commit().await.unwrap();

    // Now we want a tick of the state machine
    env.run_machine_state_controller_iteration().await;

    // It should have started the script
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.host_reprovision_requested.is_some());
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::WaitingForScript { .. } = reprovision_state else {
        panic!("Not in WaitingForScript");
    };
    txn.commit().await.unwrap();

    // The script shouldn't have completed yet, so the state machine running shouldn't change anything
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.host_reprovision_requested.is_some());
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::WaitingForScript { .. } = reprovision_state else {
        panic!("Not in WaitingForScript");
    };
    txn.commit().await.unwrap();

    // Wait a few seconds for the sleep, now the script should complete and we go to CheckingFirmwareRetry
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.host_reprovision_requested.is_some());
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::CheckingFirmwareRepeatV2 { .. } = reprovision_state else {
        panic!("Not in CheckingFirmwareRepeat");
    };
    txn.commit().await.unwrap();

    Ok(())
}

#[crate::sqlx_test]
async fn test_script_upgrade_failure(pool: sqlx::PgPool) -> CarbideResult<()> {
    let mut config = get_config();
    config.host_models = HashMap::from([(
        "1".to_string(),
        Firmware {
            vendor: bmc_vendor::BMCVendor::Dell,
            model: "PowerEdge R750".to_string(),
            explicit_start_needed: false,
            components: HashMap::from([(
                FirmwareComponentType::Bmc,
                FirmwareComponent {
                    current_version_reported_as: Some(Regex::new("^Installed-.*__iDRAC.").unwrap()),
                    preingest_upgrade_when_below: None,
                    known_firmware: vec![FirmwareEntry::standard_script("1234", "/bin/false")],
                },
            )]),
            ordering: vec![FirmwareComponentType::Uefi, FirmwareComponentType::Bmc],
        },
    )]);
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;

    let mh = common::api_fixtures::create_managed_host(&env).await;

    // Create and start an update manager
    let update_manager = MachineUpdateManager::new(
        env.pool.clone(),
        env.config.clone(),
        env.test_meter.meter(),
        env.api.work_lock_manager_handle.clone(),
        None,
    );
    // Update manager should notice that the host is underversioned, setting the request to update it
    update_manager.run_single_iteration().await.unwrap();

    // Check that we're properly marking it as upgrade needed
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    assert!(host.host_reprovision_requested.is_some());
    txn.commit().await.unwrap();

    // Now we want a tick of the state machine
    env.run_machine_state_controller_iteration().await;

    // It should have started the script
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.host_reprovision_requested.is_some());
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::WaitingForScript { .. } = reprovision_state else {
        panic!("Not in WaitingForScript");
    };
    txn.commit().await.unwrap();

    // Give it a bit to run, it will have exited with error code 0
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.host_reprovision_requested.is_some());
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::FailedFirmwareUpgrade { .. } = reprovision_state else {
        panic!("Not in FailedFirmwareUpgrade");
    };
    txn.commit().await.unwrap();

    for retry_i in 1..=MAX_FIRMWARE_UPGRADE_RETRIES {
        // Wait and try again, it will increment the retry_count and move to CheckingFirmware again
        tokio::time::sleep(std::time::Duration::from_secs(
            FirmwareGlobal::get_retry_interval().as_seconds_f64() as u64,
        ))
        .await;
        env.run_machine_state_controller_iteration().await;
        let mut txn = env.pool.begin().await.unwrap();
        let host = mh.host().db_machine(&mut txn).await;

        assert!(host.host_reprovision_requested.is_some());
        let ManagedHostState::HostReprovision {
            reprovision_state,
            retry_count,
        } = host.current_state()
        else {
            panic!("Not in HostReprovision");
        };
        assert_eq!(*retry_count, retry_i);
        let HostReprovisionState::CheckingFirmwareV2 { .. } = reprovision_state else {
            panic!("Not in CheckingFirmware");
        };
        txn.commit().await.unwrap();

        // Now we want another tick of the state machine
        env.run_machine_state_controller_iteration().await;

        // It should have started the script
        let mut txn = env.pool.begin().await.unwrap();
        let host = mh.host().db_machine(&mut txn).await;

        assert!(host.host_reprovision_requested.is_some());
        let ManagedHostState::HostReprovision {
            reprovision_state, ..
        } = host.current_state()
        else {
            panic!("Not in HostReprovision");
        };
        let HostReprovisionState::WaitingForScript { .. } = reprovision_state else {
            panic!("Not in WaitingForScript");
        };
        txn.commit().await.unwrap();

        // Give it a bit to run, it will have exited with error code 0
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        env.run_machine_state_controller_iteration().await;
        let mut txn = env.pool.begin().await.unwrap();
        let host = mh.host().db_machine(&mut txn).await;

        assert!(host.host_reprovision_requested.is_some());
        let ManagedHostState::HostReprovision {
            reprovision_state,
            retry_count,
        } = host.current_state()
        else {
            panic!("Not in HostReprovision");
        };
        assert_eq!(*retry_count, retry_i);
        let HostReprovisionState::FailedFirmwareUpgrade { .. } = reprovision_state else {
            panic!("Not in FailedFirmwareUpgrade");
        };
        txn.commit().await.unwrap();
    }

    // Wait and try again, it should not retry any more and stay in FailedFirmwareUpgrade
    tokio::time::sleep(std::time::Duration::from_secs(
        FirmwareGlobal::get_retry_interval().as_seconds_f64() as u64,
    ))
    .await;
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.host_reprovision_requested.is_some());
    let ManagedHostState::HostReprovision {
        reprovision_state,
        retry_count,
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    assert_eq!(*retry_count, MAX_FIRMWARE_UPGRADE_RETRIES);
    let HostReprovisionState::FailedFirmwareUpgrade { .. } = reprovision_state else {
        panic!("Not in FailedFirmwareUpgrade");
    };
    txn.commit().await.unwrap();

    // The machine with the exhausted retry budget surfaces on the update
    // manager's gauge rather than in per-pass logs.
    update_manager.run_single_iteration().await.unwrap();
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_exhausted_reprovision_retry_count")
            .unwrap(),
        "1"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_explicit_update(pool: sqlx::PgPool) -> CarbideResult<()> {
    let mut config = common::api_fixtures::get_config();
    config
        .host_models
        .get_mut("1")
        .unwrap()
        .explicit_start_needed = true;

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config),
    )
    .await;

    let _segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = common::api_fixtures::create_managed_host(&env).await;

    // Create and start an update manager
    let update_manager = MachineUpdateManager::new(
        env.pool.clone(),
        env.config.clone(),
        env.test_meter.meter(),
        env.api.work_lock_manager_handle.clone(),
        None,
    );

    // A tick of the state machine, but we don't start anything yet and it's still in ready
    update_manager.run_single_iteration().await.unwrap();
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::Ready = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };

    // Start time in the future
    db::machine::update_firmware_update_time_window_start_end(
        &[mh.id.into()],
        chrono::Utc::now()
            .checked_add_signed(chrono::TimeDelta::seconds(100))
            .unwrap(),
        chrono::Utc::now()
            .checked_add_signed(chrono::TimeDelta::seconds(101))
            .unwrap(),
        &mut txn,
    )
    .await?;
    txn.commit().await.unwrap();

    // Still doesn't start
    update_manager.run_single_iteration().await.unwrap();
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::Ready = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };

    // End time in the past
    db::machine::update_firmware_update_time_window_start_end(
        &[mh.id.into()],
        chrono::Utc::now()
            .checked_add_signed(chrono::TimeDelta::seconds(-100))
            .unwrap(),
        chrono::Utc::now()
            .checked_add_signed(chrono::TimeDelta::seconds(-99))
            .unwrap(),
        &mut txn,
    )
    .await?;
    txn.commit().await.unwrap();

    // Still doesn't start
    update_manager.run_single_iteration().await.unwrap();
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::Ready = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };

    // Now a start and end around us
    db::machine::update_firmware_update_time_window_start_end(
        &[mh.id.into()],
        chrono::Utc::now()
            .checked_add_signed(chrono::TimeDelta::seconds(-100))
            .unwrap(),
        chrono::Utc::now()
            .checked_add_signed(chrono::TimeDelta::seconds(100))
            .unwrap(),
        &mut txn,
    )
    .await?;
    txn.commit().await.unwrap();

    // Now it should start
    update_manager.run_single_iteration().await.unwrap();
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision { .. } = host.state.clone().value else {
        panic!("Unexpected state {:?}", host.state);
    };

    // That's sufficient to check the differences in this path
    Ok(())
}

#[crate::sqlx_test]
async fn test_manual_firmware_upgrade_workflow(pool: sqlx::PgPool) -> CarbideResult<()> {
    // create an env with requires_manual_upgrade = true
    let mut config = common::api_fixtures::get_config();
    config.firmware_global.requires_manual_upgrade = true;
    config.bom_validation.enabled = false;
    config.machine_validation_config.enabled = false;
    let env =
        create_test_env_with_overrides(pool.clone(), TestEnvOverrides::with_config(config)).await;

    // create a gb200
    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON,
        ),
    )
    .await;

    // Create and start an update manager
    let update_manager = MachineUpdateManager::new(
        env.pool.clone(),
        env.config.clone(),
        env.test_meter.meter(),
        env.api.work_lock_manager_handle.clone(),
        None,
    );
    update_manager.run_single_iteration().await?;

    // verify reprovision was requested
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    assert!(host.host_reprovision_requested.is_some());
    txn.commit().await.unwrap();

    // state machine iteration
    // Ready -> WaitingForManualUpgrade
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    assert!(
        matches!(
            host.current_state(),
            ManagedHostState::HostReprovision {
                reprovision_state: HostReprovisionState::WaitingForManualUpgrade { .. },
                ..
            }
        ),
        "Machine should still be in HostReprovision::WaitingForManualUpgrade"
    );

    // multiple state machine iterations
    // should stay in WaitingForManualUpgrade
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    assert!(
        matches!(
            host.current_state(),
            ManagedHostState::HostReprovision {
                reprovision_state: HostReprovisionState::WaitingForManualUpgrade { .. },
                ..
            }
        ),
        "Machine should still be in HostReprovision::WaitingForManualUpgrade"
    );

    // Mark manual upgrade as complete
    db::host_machine_update::set_manual_firmware_upgrade_completed(&mut txn, &mh.host().id).await?;
    txn.commit().await.unwrap();

    // state machine iteration
    // WaitingForManualUpgrade -> CheckingFirmwareRepeat
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.manual_firmware_upgrade_completed.is_some());

    assert!(
        matches!(
            host.current_state(),
            ManagedHostState::HostReprovision {
                reprovision_state: HostReprovisionState::CheckingFirmwareRepeatV2 { .. },
                ..
            }
        ),
        "Machine should be in HostReprovision::CheckingFirmwareRepeat"
    );

    // CheckingFirmwareRepeat -> WaitingForUpload
    env.run_machine_state_controller_iteration().await;

    // Wait a bit for upload to complete
    sleep(Duration::from_millis(6000)).await;

    // WaitingForUpload -> WaitingForFirmwareUpgrade
    env.run_machine_state_controller_iteration().await;

    // WaitingForFirmwareUpgrade -> ResetForNewFirmware
    env.run_machine_state_controller_iteration().await;

    // ResetForNewFirmware -> NewFirmwareReportedWait
    env.run_machine_state_controller_iteration().await;

    // "Site explorer" pass
    let endpoints = db::explored_endpoints::find_by_ips(
        txn.as_mut(),
        vec![host.status.bmc_info.ip_addr().unwrap()],
    )
    .await
    .unwrap();
    let mut endpoint = endpoints.into_iter().next().unwrap();
    endpoint.report.service[0].inventories[0].version = Some("6.00.30.00".to_string());
    endpoint.report.service[0].inventories[1].version = Some("1.13.2".to_string());
    endpoint
        .report
        .versions
        .insert(FirmwareComponentType::Uefi, "1.13.2".to_string());
    endpoint
        .report
        .versions
        .insert(FirmwareComponentType::Bmc, "6.00.30.00".to_string());
    db::explored_endpoints::try_update(
        host.status.bmc_info.ip_addr().unwrap(),
        endpoint.report_version,
        &endpoint.report,
        false,
        &mut txn,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // NewFirmwareReportedWait -> CheckingFirmwareRepeat
    env.run_machine_state_controller_iteration().await;

    // CheckingFirmwareRepeat -> WaitingForLockdown
    env.run_machine_state_controller_iteration().await;

    // WaitingForLockdown -> BomValidating
    env.run_machine_state_controller_iteration().await;

    // BomValidating -> Validation (RebootHost)
    env.run_machine_state_controller_iteration().await;

    // Validation (RebootHost) -> Validation (MachineValidating)
    env.run_machine_state_controller_iteration().await;

    // reboot makes it move forward from MachineValidating
    common::api_fixtures::reboot_completed(&env, mh.host().id.into()).await;

    // Validation (MachineValidating) -> HostInit
    env.run_machine_state_controller_iteration().await;

    // HostInit -> Ready
    env.run_machine_state_controller_iteration().await;

    // assert manual_firmware_upgrade_completed is cleared
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;

    assert!(host.manual_firmware_upgrade_completed.is_none());

    Ok(())
}

/// Helper: set `host` to WaitingForScoutUpgrade with the given deadline and result.
async fn put_in_waiting_for_scout_upgrade(
    env: &common::api_fixtures::TestEnv,
    host: &common::api_fixtures::test_machine::TestMachine<HostMachineId>,
    deadline: chrono::DateTime<chrono::Utc>,
    power_drains_needed: Option<u32>,
    result: Option<model::machine::ScoutUpgradeResult>,
) {
    let mut txn = env.pool.begin().await.unwrap();
    let machine = host.db_machine(&mut txn).await;
    let state = ManagedHostState::HostReprovision {
        reprovision_state: HostReprovisionState::WaitingForScoutUpgrade {
            upgrade_task_id: "scout-upgrade-task-id".to_string(),
            firmware_type: FirmwareComponentType::Bmc,
            final_version: "1.2.3".to_string(),
            power_drains_needed,
            started_at: chrono::Utc::now(),
            deadline,
            task_json: String::new(),
            result,
        },
        retry_count: 0,
    };
    db::machine::advance(&machine, &mut txn, &state, None)
        .await
        .unwrap();
    txn.commit().await.unwrap();
}

async fn put_in_new_firmware_reported_wait(
    env: &TestEnv,
    mh: &TestManagedHost,
    previous_reset_time: i64,
    reset_retry_count: u32,
) {
    let mut txn = env.pool.begin().await.unwrap();
    let machine = mh.host().db_machine(&mut txn).await;
    let state = ManagedHostState::HostReprovision {
        reprovision_state: HostReprovisionState::NewFirmwareReportedWait {
            firmware_type: FirmwareComponentType::Uefi,
            firmware_number: Some(0),
            final_version: "1.13.2".to_string(),
            previous_reset_time: Some(previous_reset_time),
            reset_retry_count,
        },
        retry_count: 0,
    };
    db::machine::advance(&machine, &mut txn, &state, None)
        .await
        .unwrap();
    txn.commit().await.unwrap();
}

#[crate::sqlx_test]
async fn test_new_firmware_reported_wait_retries_reset_after_timeout(
    pool: sqlx::PgPool,
) -> CarbideResult<()> {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    put_in_new_firmware_reported_wait(&env, &mh, chrono::Utc::now().timestamp() - 31 * 60, 0).await;

    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::NewFirmwareReportedWait {
        reset_retry_count, ..
    } = reprovision_state
    else {
        panic!("expected NewFirmwareReportedWait, got {reprovision_state:?}");
    };
    assert_eq!(*reset_retry_count, 1);

    Ok(())
}

#[crate::sqlx_test]
async fn test_new_firmware_reported_wait_fails_after_reset_retry_limit(
    pool: sqlx::PgPool,
) -> CarbideResult<()> {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    put_in_new_firmware_reported_wait(
        &env,
        &mh,
        chrono::Utc::now().timestamp() - 31 * 60,
        MAX_NEW_FIRMWARE_REPORTED_RESET_RETRIES,
    )
    .await;

    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::FailedFirmwareUpgrade { reason, .. } = reprovision_state else {
        panic!("expected FailedFirmwareUpgrade, got {reprovision_state:?}");
    };
    let reason = reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("firmware version did not converge after completed update"),
        "unexpected reason: {reason}",
    );
    assert!(
        reason.contains("expected 1.13.2"),
        "unexpected reason: {reason}",
    );
    assert!(reason.contains("1.12.0"), "unexpected reason: {reason}",);

    Ok(())
}

#[crate::sqlx_test]
async fn test_waiting_for_scout_upgrade_success_transitions_to_reset_for_new_firmware(
    pool: sqlx::PgPool,
) -> CarbideResult<()> {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;

    put_in_waiting_for_scout_upgrade(
        &env,
        &mh.host(),
        chrono::Utc::now() + chrono::TimeDelta::minutes(60),
        Some(1),
        Some(model::machine::ScoutUpgradeResult {
            success: true,
            exit_code: 0,
            stdout: "ok".to_string(),
            stderr: String::new(),
            error: String::new(),
        }),
    )
    .await;

    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::ResetForNewFirmware {
        power_drains_needed,
        ..
    } = reprovision_state
    else {
        panic!("expected ResetForNewFirmware, got {reprovision_state:?}");
    };
    assert_eq!(*power_drains_needed, Some(1));

    Ok(())
}

#[crate::sqlx_test]
async fn test_waiting_for_scout_upgrade_failure_uses_error_as_reason(
    pool: sqlx::PgPool,
) -> CarbideResult<()> {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;

    put_in_waiting_for_scout_upgrade(
        &env,
        &mh.host(),
        chrono::Utc::now() + chrono::TimeDelta::minutes(60),
        None,
        Some(model::machine::ScoutUpgradeResult {
            success: false,
            exit_code: 1,
            stdout: String::new(),
            stderr: String::new(),
            error: "boom".to_string(),
        }),
    )
    .await;

    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::FailedFirmwareUpgrade { reason, .. } = reprovision_state else {
        panic!("expected FailedFirmwareUpgrade, got {reprovision_state:?}");
    };
    assert_eq!(reason.as_deref(), Some("boom"));

    Ok(())
}

#[crate::sqlx_test]
async fn test_waiting_for_scout_upgrade_failure_without_error_uses_exit_code(
    pool: sqlx::PgPool,
) -> CarbideResult<()> {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;

    put_in_waiting_for_scout_upgrade(
        &env,
        &mh.host(),
        chrono::Utc::now() + chrono::TimeDelta::minutes(60),
        None,
        Some(model::machine::ScoutUpgradeResult {
            success: false,
            exit_code: 7,
            stdout: String::new(),
            stderr: String::new(),
            error: String::new(),
        }),
    )
    .await;

    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::FailedFirmwareUpgrade { reason, .. } = reprovision_state else {
        panic!("expected FailedFirmwareUpgrade, got {reprovision_state:?}");
    };
    assert_eq!(
        reason.as_deref(),
        Some("scout upgrade failed with exit code 7"),
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_waiting_for_scout_upgrade_past_deadline_times_out(
    pool: sqlx::PgPool,
) -> CarbideResult<()> {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;

    put_in_waiting_for_scout_upgrade(
        &env,
        &mh.host(),
        chrono::Utc::now() - chrono::TimeDelta::minutes(1),
        None,
        None,
    )
    .await;

    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::FailedFirmwareUpgrade { reason, .. } = reprovision_state else {
        panic!("expected FailedFirmwareUpgrade, got {reprovision_state:?}");
    };
    assert!(
        reason
            .as_deref()
            .is_some_and(|r| r.starts_with("scout firmware upgrade timed out")),
        "unexpected reason: {reason:?}",
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_waiting_for_scout_upgrade_before_deadline_waits(
    pool: sqlx::PgPool,
) -> CarbideResult<()> {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;

    put_in_waiting_for_scout_upgrade(
        &env,
        &mh.host(),
        chrono::Utc::now() + chrono::TimeDelta::minutes(60),
        None,
        None,
    )
    .await;

    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = host.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    assert!(
        matches!(
            reprovision_state,
            HostReprovisionState::WaitingForScoutUpgrade { result: None, .. }
        ),
        "expected to remain in WaitingForScoutUpgrade, got {reprovision_state:?}",
    );

    Ok(())
}
