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

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use carbide_rack_controller::config::ScaleUpFabricManagerApiVersion;
use carbide_rack_controller::context::RackStateHandlerContextObjects;
use carbide_rack_controller::firmware_object::FirmwareObjectFetcher;
use carbide_rack_controller::handler::RackStateHandler;
use carbide_rack_controller::metrics::RackMetrics;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials,
};
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use carbide_uuid::rack::{RackId, RackProfileId};
use carbide_uuid::switch::SwitchId;
use db::db_read::DbReader;
use db::{
    ObjectColumnFilter, expected_rack as db_expected_rack, rack as db_rack, switch as db_switch,
};
use librms::protos::{rack_manager as rms, rack_manager_v2 as rms_v2};
use model::expected_machine::ExpectedMachineData;
use model::expected_rack::ExpectedRack;
use model::rack::{
    ConfigureNmxClusterState, FirmwareUpgradeDeviceStatus, FirmwareUpgradeJob,
    FirmwareUpgradeState, MaintenanceActivity, MaintenanceScope, NvosUpdateJob, NvosUpdateState,
    NvosUpdateSwitchStatus, Rack, RackConfig, RackFirmwareUpgradeState, RackFirmwareUpgradeStatus,
    RackMaintenanceState, RackPowerState, RackState, RackValidationState, SwitchNvosUpdateState,
    SwitchNvosUpdateStatus,
};
use model::rack_type::{
    RackCapabilitiesSet, RackCapabilityCompute, RackCapabilityPowerShelf, RackCapabilitySwitch,
    RackFirmwareObjectConfig, RackHardwareClass, RackHardwareTopology, RackHardwareType,
    RackProductFamily, RackProfile, RackProfileConfig,
};
use model::switch::{
    CONTROL_PLANE_STATE_CONFIGURED, FabricManagerState, FabricManagerStatus, NewSwitch,
    SwitchConfig,
};
use model::test_support::ManagedHostConfig;
use state_controller::db_write_batch::DbWriteBatch;
use state_controller::state_handler::{StateHandler, StateHandlerContext, StateHandlerOutcome};
use tonic::Request;

use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
use crate::tests::common::api_fixtures::site_explorer::{create_expected_switches, new_host};
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_test_env_with_overrides, get_config,
};

#[derive(Debug)]
struct StaticFirmwareObjectFetcher {
    response: Mutex<Result<String, String>>,
    requested_urls: Mutex<Vec<String>>,
    requested_timeouts: Mutex<Vec<std::time::Duration>>,
}

#[async_trait]
impl FirmwareObjectFetcher for StaticFirmwareObjectFetcher {
    async fn fetch(&self, url: &str, timeout: std::time::Duration) -> Result<String, String> {
        self.requested_urls.lock().unwrap().push(url.to_string());
        self.requested_timeouts.lock().unwrap().push(timeout);
        self.response.lock().unwrap().clone()
    }
}

fn test_capabilities() -> RackCapabilitiesSet {
    RackCapabilitiesSet {
        compute: RackCapabilityCompute {
            name: None,
            count: 2,
            vendor: Some("NVIDIA".to_string()),
            slot_ids: None,
            attributes: Default::default(),
        },
        switch: RackCapabilitySwitch {
            name: None,
            count: 1,
            vendor: Some("NVIDIA".to_string()),
            slot_ids: None,
            attributes: Default::default(),
        },
        power_shelf: RackCapabilityPowerShelf {
            name: None,
            count: 1,
            vendor: Some("LiteOn".to_string()),
            slot_ids: None,
            attributes: Default::default(),
        },
    }
}

fn simple_capabilities() -> RackCapabilitiesSet {
    RackCapabilitiesSet {
        compute: RackCapabilityCompute {
            name: None,
            count: 2,
            vendor: Some("NVIDIA".to_string()),
            slot_ids: None,
            attributes: Default::default(),
        },
        switch: RackCapabilitySwitch {
            name: None,
            count: 0,
            vendor: Some("NVIDIA".to_string()),
            slot_ids: None,
            attributes: Default::default(),
        },
        power_shelf: RackCapabilityPowerShelf {
            name: None,
            count: 0,
            vendor: Some("LiteOn".to_string()),
            slot_ids: None,
            attributes: Default::default(),
        },
    }
}

fn single_capabilities() -> RackCapabilitiesSet {
    RackCapabilitiesSet {
        compute: RackCapabilityCompute {
            name: None,
            count: 1,
            vendor: Some("NVIDIA".to_string()),
            slot_ids: None,
            attributes: Default::default(),
        },
        switch: RackCapabilitySwitch {
            name: None,
            count: 0,
            vendor: Some("NVIDIA".to_string()),
            slot_ids: None,
            attributes: Default::default(),
        },
        power_shelf: RackCapabilityPowerShelf {
            name: None,
            count: 0,
            vendor: Some("LiteOn".to_string()),
            slot_ids: None,
            attributes: Default::default(),
        },
    }
}

pub(crate) fn config_with_rack_profiles() -> crate::cfg::file::CarbideConfig {
    let mut config = get_config();
    config.rack_profiles = RackProfileConfig {
        rack_profiles: [
            (
                "NVL72".to_string(),
                RackProfile {
                    product_family: Some(RackProductFamily::Gb200),
                    rack_hardware_topology: Some(RackHardwareTopology::Gb200Nvl72r1C2g4Topology),
                    rack_capabilities: test_capabilities(),
                    ..Default::default()
                },
            ),
            (
                "Simple".to_string(),
                RackProfile {
                    product_family: Some(RackProductFamily::Gb200),
                    firmware_object: None,
                    rack_hardware_topology: Some(RackHardwareTopology::Gb200Nvl72r1C2g4Topology),
                    rack_hardware_type: Some(RackHardwareType::any()),
                    rack_hardware_class: Some(RackHardwareClass::Prod),
                    attributes: Default::default(),
                    rack_capabilities: simple_capabilities(),
                },
            ),
            (
                "Single".to_string(),
                RackProfile {
                    product_family: Some(RackProductFamily::Gb200),
                    firmware_object: None,
                    rack_hardware_topology: Some(RackHardwareTopology::Gb200Nvl72r1C2g4Topology),
                    rack_hardware_type: Some(RackHardwareType::any()),
                    rack_hardware_class: Some(RackHardwareClass::Prod),
                    attributes: Default::default(),
                    rack_capabilities: single_capabilities(),
                },
            ),
            ("Empty".to_string(), RackProfile::default()),
        ]
        .into_iter()
        .collect(),
    };
    config
}

fn config_with_nmx_cluster_profile() -> crate::cfg::file::CarbideConfig {
    let mut config = config_with_rack_profiles();
    config.rack_profiles.rack_profiles.insert(
        "NmxCluster".to_string(),
        RackProfile {
            product_family: Some(RackProductFamily::Gb200),
            rack_hardware_topology: Some(RackHardwareTopology::Gb200Nvl72r1C2g4Topology),
            rack_capabilities: test_capabilities(),
            ..Default::default()
        },
    );
    config
}

async fn create_single_compute_rack(
    env: &TestEnv,
    pool: &sqlx::PgPool,
) -> Result<(RackId, model::machine::ManagedHostStateSnapshot), Box<dyn std::error::Error>> {
    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Single")),
        &RackConfig::default(),
        None,
    )
    .await?;
    drop(txn);

    let host = new_host(
        env,
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        }),
    )
    .await?;

    Ok((rack_id, host))
}

async fn set_machine_host_reprovision_state(
    pool: &sqlx::PgPool,
    machine_id: &MachineId,
    reprovision_state: model::machine::HostReprovisionState,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let machine = db::machine::find_one(
        txn.as_mut(),
        machine_id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .expect("machine should exist");
    db::machine::advance(
        &machine,
        txn.as_mut(),
        &model::machine::ManagedHostState::HostReprovision {
            reprovision_state,
            retry_count: 0,
        },
        None,
    )
    .await?;
    txn.commit().await?;
    Ok(())
}

async fn set_machine_power_states(
    pool: &sqlx::PgPool,
    machine_id: &MachineId,
    desired_power_state: model::power_manager::PowerState,
    actual_power_state: model::power_manager::PowerState,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let power_options = db::power_options::get_by_ids(&[*machine_id], txn.as_mut())
        .await?
        .pop()
        .expect("machine should have power options");
    let mut power_options = db::power_options::update_desired_state(
        machine_id,
        desired_power_state,
        &power_options.desired_power_state_version,
        txn.as_mut(),
    )
    .await?;
    power_options.last_fetched_power_state = actual_power_state;
    db::power_options::persist(&power_options, txn.as_mut()).await?;
    txn.commit().await?;
    Ok(())
}

fn waiting_for_rack_firmware_upgrade_state() -> model::machine::HostReprovisionState {
    model::machine::HostReprovisionState::WaitingForRackFirmwareUpgrade
}

fn failed_rack_firmware_upgrade_state() -> model::machine::HostReprovisionState {
    model::machine::HostReprovisionState::FailedFirmwareUpgrade {
        firmware_type: model::firmware::FirmwareComponentType::Bmc,
        report_time: Some(chrono::Utc::now()),
        reason: Some("upgrade failed".to_string()),
    }
}

fn completed_rack_firmware_upgrade_state() -> model::machine::HostReprovisionState {
    model::machine::HostReprovisionState::CheckingFirmwareRepeatV2 {
        firmware_type: None,
        firmware_number: None,
    }
}

async fn create_two_compute_rack(
    env: &TestEnv,
    pool: &sqlx::PgPool,
) -> Result<
    (
        RackId,
        model::machine::ManagedHostStateSnapshot,
        model::machine::ManagedHostStateSnapshot,
    ),
    Box<dyn std::error::Error>,
> {
    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Simple")),
        &RackConfig::default(),
        None,
    )
    .await?;
    drop(txn);

    let host_a = new_host(
        env,
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        }),
    )
    .await?;
    let host_b = new_host(
        env,
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        }),
    )
    .await?;

    Ok((rack_id, host_a, host_b))
}

async fn attach_switches_with_nvos_credentials(
    env: &TestEnv,
    rack_id: &RackId,
    count: usize,
) -> Result<Vec<SwitchId>, Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await?;
    let expected_switches = create_expected_switches(txn.as_mut()).await;
    let selected_switches = expected_switches
        .into_iter()
        .take(count)
        .collect::<Vec<_>>();
    if selected_switches.len() != count {
        return Err(eyre::eyre!("expected at least {} switch fixtures", count).into());
    }

    let mut switch_ids = Vec::with_capacity(selected_switches.len());
    for (index, expected_switch) in selected_switches.iter().enumerate() {
        let switch_id = model::switch::switch_id::from_hardware_info(
            &expected_switch.serial_number,
            "NVIDIA",
            "Switch",
            carbide_uuid::switch::SwitchIdSource::ProductBoardChassisSerial,
            carbide_uuid::switch::SwitchType::NvLink,
        )?;

        let new_switch = NewSwitch {
            id: switch_id,
            config: SwitchConfig {
                name: expected_switch.metadata.name.clone(),
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            bmc_mac_address: Some(expected_switch.bmc_mac_address),
            metadata: None,
            rack_id: Some(rack_id.clone()),
            slot_number: Some(index as i32),
            tray_index: Some(0),
        };
        db_switch::create(txn.as_mut(), &new_switch).await?;
        switch_ids.push(switch_id);
    }
    txn.commit().await?;

    for expected_switch in selected_switches {
        env.api
            .credential_manager
            .set_credentials(
                &CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::BmcRoot {
                        bmc_mac_address: expected_switch.bmc_mac_address,
                    },
                },
                &Credentials::UsernamePassword {
                    username: "root".to_string(),
                    password: "notforprod".to_string(),
                },
            )
            .await
            .map_err(|error| eyre::eyre!("failed to set switch BMC credentials: {}", error))?;
        env.api
            .credential_manager
            .set_credentials(
                &CredentialKey::SwitchNvosAdmin {
                    bmc_mac_address: expected_switch.bmc_mac_address,
                },
                &Credentials::UsernamePassword {
                    username: "nvos-admin".to_string(),
                    password: "nvos-pass".to_string(),
                },
            )
            .await
            .map_err(|error| eyre::eyre!("failed to set switch NVOS credentials: {}", error))?;
    }

    Ok(switch_ids)
}

async fn attach_switch_with_nvos_credentials(
    env: &TestEnv,
    rack_id: &RackId,
) -> Result<SwitchId, Box<dyn std::error::Error>> {
    let mut switch_ids = attach_switches_with_nvos_credentials(env, rack_id, 1).await?;
    switch_ids
        .pop()
        .ok_or_else(|| eyre::eyre!("expected one switch fixture").into())
}

pub(crate) fn new_rack_id() -> RackId {
    RackId::new(uuid::Uuid::new_v4().to_string())
}

async fn create_ready_rack_with_switch(
    env: &TestEnv,
    pool: &sqlx::PgPool,
) -> Result<(RackId, SwitchId), Box<dyn std::error::Error>> {
    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("NVL72")),
        &RackConfig::default(),
        None,
    )
    .await?;
    drop(txn);

    let switch_id = attach_switch_with_nvos_credentials(env, &rack_id).await?;

    let mut txn = pool.begin().await?;
    let rack = get_db_rack(txn.as_mut(), &rack_id).await;
    db_rack::try_update_controller_state(
        txn.as_mut(),
        &rack_id,
        rack.controller_state.version,
        rack.controller_state.version.increment(),
        &RackState::Ready,
    )
    .await?;
    txn.commit().await?;

    Ok((rack_id, switch_id))
}

async fn create_expected_rack(pool: &sqlx::PgPool, rack_id: &RackId, rack_profile_id: &str) {
    let mut txn = pool.acquire().await.unwrap();
    let er = ExpectedRack {
        rack_id: rack_id.clone(),
        rack_profile_id: RackProfileId::new(rack_profile_id),
        ..Default::default()
    };
    db_expected_rack::create(&mut txn, &er).await.unwrap();
}

pub(crate) fn new_machine_id(seed: u8) -> MachineId {
    let mut hash = [0u8; 32];
    hash[0] = seed;
    MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        hash,
        MachineType::Host,
    )
}

#[crate::sqlx_test]
async fn test_on_demand_rack_maintenance_schedules_nvos_only_scope(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;

    crate::handlers::rack::on_demand_rack_maintenance(
        env.api.as_ref(),
        Request::new(rpc::forge::RackMaintenanceOnDemandRequest {
            rack_id: Some(rack_id.clone()),
            scope: Some(rpc::forge::RackMaintenanceScope {
                machine_ids: vec![],
                switch_ids: vec![switch_id.to_string()],
                power_shelf_ids: vec![],
                activities: vec![rpc::forge::MaintenanceActivityConfig {
                    activity: Some(
                        rpc::forge::maintenance_activity_config::Activity::NvosUpdate(
                            rpc::forge::NvosUpdateActivity {
                                config_json: r#"{"Id":"fw-nvos"}"#.to_string(),
                                access_token: Some("token".to_string()),
                            },
                        ),
                    ),
                }],
            }),
        }),
    )
    .await?;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let scope = rack
        .config
        .maintenance_requested
        .expect("maintenance should be scheduled");
    assert_eq!(scope.switch_ids, vec![switch_id]);
    assert_eq!(scope.activities.len(), 1);
    assert!(matches!(
        &scope.activities[0],
        MaintenanceActivity::NvosUpdate {
            config_json,
        } if config_json == r#"{"Id":"fw-nvos"}"#
    ));

    Ok(())
}

#[crate::sqlx_test]
async fn test_on_demand_rack_maintenance_schedules_configure_nmx_cluster_only_scope(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;

    crate::handlers::rack::on_demand_rack_maintenance(
        env.api.as_ref(),
        Request::new(rpc::forge::RackMaintenanceOnDemandRequest {
            rack_id: Some(rack_id.clone()),
            scope: Some(rpc::forge::RackMaintenanceScope {
                machine_ids: vec![],
                switch_ids: vec![switch_id.to_string()],
                power_shelf_ids: vec![],
                activities: vec![rpc::forge::MaintenanceActivityConfig {
                    activity: Some(
                        rpc::forge::maintenance_activity_config::Activity::ConfigureNmxCluster(
                            rpc::forge::ConfigureNmxClusterActivity {},
                        ),
                    ),
                }],
            }),
        }),
    )
    .await?;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let scope = rack
        .config
        .maintenance_requested
        .expect("maintenance should be scheduled");
    assert_eq!(scope.switch_ids, vec![switch_id]);
    assert_eq!(scope.activities.len(), 1);
    assert!(matches!(
        &scope.activities[0],
        MaintenanceActivity::ConfigureNmxCluster
    ));

    Ok(())
}

#[crate::sqlx_test]
async fn test_on_demand_rack_maintenance_schedules_firmware_and_nvos_scope(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;

    crate::handlers::rack::on_demand_rack_maintenance(
        env.api.as_ref(),
        Request::new(rpc::forge::RackMaintenanceOnDemandRequest {
            rack_id: Some(rack_id.clone()),
            scope: Some(rpc::forge::RackMaintenanceScope {
                machine_ids: vec![],
                switch_ids: vec![switch_id.to_string()],
                power_shelf_ids: vec![],
                activities: vec![
                    rpc::forge::MaintenanceActivityConfig {
                        activity: Some(
                            rpc::forge::maintenance_activity_config::Activity::FirmwareUpgrade(
                                rpc::forge::FirmwareUpgradeActivity {
                                    firmware_version: r#"{"Id":"fw-mixed"}"#.to_string(),
                                    components: vec!["BMC".to_string()],
                                    access_token: Some("token".to_string()),
                                    force_update: false,
                                },
                            ),
                        ),
                    },
                    rpc::forge::MaintenanceActivityConfig {
                        activity: Some(
                            rpc::forge::maintenance_activity_config::Activity::NvosUpdate(
                                rpc::forge::NvosUpdateActivity {
                                    config_json: r#"{"Id":"fw-mixed"}"#.to_string(),
                                    access_token: Some("token".to_string()),
                                },
                            ),
                        ),
                    },
                ],
            }),
        }),
    )
    .await?;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let scope = rack
        .config
        .maintenance_requested
        .expect("maintenance should be scheduled");
    assert_eq!(scope.switch_ids, vec![switch_id]);
    assert_eq!(scope.activities.len(), 2);
    assert!(matches!(
        &scope.activities[0],
        MaintenanceActivity::FirmwareUpgrade {
            firmware_version: Some(id),
            components,
            ..
        } if id == r#"{"Id":"fw-mixed"}"# && components == &vec!["BMC".to_string()]
    ));
    assert!(matches!(
        &scope.activities[1],
        MaintenanceActivity::NvosUpdate {
            config_json,
        } if config_json == r#"{"Id":"fw-mixed"}"#
    ));
    let token_credentials = env
        .test_credential_manager
        .get_credentials(&CredentialKey::RackMaintenanceAccessToken {
            rack_id: rack_id.clone(),
        })
        .await
        .expect("credential lookup should succeed")
        .expect("access token should be stored as a credential");
    assert_eq!(
        token_credentials,
        Credentials::UsernamePassword {
            username: "access_token".to_string(),
            password: "token".to_string(),
        }
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_on_demand_rack_maintenance_defaults_missing_access_token_to_noauth(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;

    crate::handlers::rack::on_demand_rack_maintenance(
        env.api.as_ref(),
        Request::new(rpc::forge::RackMaintenanceOnDemandRequest {
            rack_id: Some(rack_id.clone()),
            scope: Some(rpc::forge::RackMaintenanceScope {
                machine_ids: vec![],
                switch_ids: vec![switch_id.to_string()],
                power_shelf_ids: vec![],
                activities: vec![rpc::forge::MaintenanceActivityConfig {
                    activity: Some(
                        rpc::forge::maintenance_activity_config::Activity::FirmwareUpgrade(
                            rpc::forge::FirmwareUpgradeActivity {
                                firmware_version: r#"{"Id":"fw-mixed"}"#.to_string(),
                                components: vec!["BMC".to_string()],
                                access_token: None,
                                force_update: false,
                            },
                        ),
                    ),
                }],
            }),
        }),
    )
    .await?;

    let token_credentials = env
        .test_credential_manager
        .get_credentials(&CredentialKey::RackMaintenanceAccessToken {
            rack_id: rack_id.clone(),
        })
        .await
        .expect("credential lookup should succeed")
        .expect("access token should be stored as a credential");
    assert_eq!(
        token_credentials,
        Credentials::UsernamePassword {
            username: "access_token".to_string(),
            password: carbide_rack::firmware_object::RMS_NOAUTH_ACCESS_TOKEN.to_string(),
        }
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_terminate_rack_maintenance_latches_request_and_cleans_access_token(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;
    let (rack_id, _) = create_ready_rack_with_switch(&env, &pool).await?;
    let request = || {
        Request::new(rpc::forge::RackMaintenanceTerminateRequest {
            rack_id: Some(rack_id.clone()),
        })
    };

    let error = crate::handlers::rack::terminate_rack_maintenance(env.api.as_ref(), request())
        .await
        .expect_err("Ready rack should reject maintenance termination");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);

    let mut txn = pool.begin().await?;
    let rack = get_db_rack(txn.as_mut(), &rack_id).await;
    assert!(
        db_rack::try_update_controller_state(
            txn.as_mut(),
            &rack_id,
            rack.controller_state.version,
            rack.controller_state.version.increment(),
            &RackState::Maintenance {
                maintenance_state: RackMaintenanceState::FirmwareUpgrade {
                    rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
                },
            },
        )
        .await?
    );
    txn.commit().await?;

    env.api
        .credential_manager
        .set_credentials(
            &CredentialKey::RackMaintenanceAccessToken {
                rack_id: rack_id.clone(),
            },
            &Credentials::UsernamePassword {
                username: "access_token".to_string(),
                password: "token".to_string(),
            },
        )
        .await
        .map_err(|error| eyre::eyre!("failed to set maintenance access token: {}", error))?;

    crate::handlers::rack::terminate_rack_maintenance(env.api.as_ref(), request()).await?;
    // An exact retry while the controller has not consumed the latch is
    // idempotent.
    crate::handlers::rack::terminate_rack_maintenance(env.api.as_ref(), request()).await?;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(matches!(
        rack.controller_state.value,
        RackState::Maintenance { .. }
    ));
    assert!(rack.config.maintenance_termination_requested);
    assert!(
        env.test_credential_manager
            .get_credentials(&CredentialKey::RackMaintenanceAccessToken {
                rack_id: rack_id.clone(),
            })
            .await
            .map_err(|error| eyre::eyre!("failed to get maintenance access token: {}", error))?
            .is_none()
    );

    Ok(())
}

/// test_expected_no_definition_stays_parked pins the first thing `handle_created` does:
/// with no rack profile to resolve, it parks in `Created` instead of advancing.
///
/// The rack is created with `None` for the profile id deliberately. Hand it a known
/// profile and `resolve_capabilities` succeeds, so the handler falls through to the
/// device-count check and waits for an entirely different reason -- which left the
/// unresolved-profile branch untested until this test was pointed at it.
#[crate::sqlx_test]
async fn test_expected_no_definition_stays_parked(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config_with_rack_profiles();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;

    db_rack::create(&mut txn, &rack_id, None, &RackConfig::default(), None).await?;

    let mut rack = get_db_rack(txn.as_mut(), &rack_id).await;

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &RackState::Created, &mut ctx)
        .await?;

    // Match the reason, not just the variant. The device-count check further down
    // `handle_created` also returns `Wait`, so a bare `Wait { .. }` would keep passing if
    // an unresolvable profile ever started resolving -- which is the whole thing this test
    // is here to catch. (`StateHandlerOutcome` has no `Debug`, since it holds a
    // `PgTransaction`, so pull the reason out rather than formatting the outcome.)
    let reason = match &outcome {
        StateHandlerOutcome::Wait { reason, .. } => reason.as_str(),
        _ => panic!("expected the handler to wait, not advance"),
    };
    assert_eq!(
        reason, "no or unknown rack_profile_id",
        "Rack with an unresolvable rack_profile_id should wait on the profile, not on device counts"
    );

    Ok(())
}

/// test_expected_incomplete_device_counts_stays verifies that a rack with a
/// topology expecting more devices than currently exist stays in Created.
#[crate::sqlx_test]
async fn test_expected_incomplete_device_counts_stays(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config_with_rack_profiles();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;

    // Create a rack with a definition expecting 2 compute, 1 switch, 1 PS,
    // but only register 1 compute tray.
    let mut rack = db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("NVL72")),
        &RackConfig::default(),
        None,
    )
    .await?;

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &RackState::Created, &mut ctx)
        .await?;

    assert!(
        matches!(outcome, StateHandlerOutcome::Wait { .. }),
        "Rack with incomplete device counts should wait in Created"
    );

    Ok(())
}

/// test_expected_zero_topology_transitions_to_discovering verifies that a rack
/// with zero expected devices in topology immediately transitions to Discovering.
#[crate::sqlx_test]
async fn test_expected_zero_topology_transitions_to_discovering(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config_with_rack_profiles();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;

    // Create the rack with the "Empty" profile, which expects zero devices.
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Empty")),
        &RackConfig::default(),
        None,
    )
    .await?;

    drop(txn);

    create_expected_rack(&pool, &rack_id, "Empty").await;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &RackState::Created, &mut ctx)
        .await?;

    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => {
            assert!(
                matches!(next_state, RackState::Discovering),
                "Zero-device topology should transition to Discovering, got {:?}",
                next_state
            );
        }
        other => panic!(
            "Expected Transition to Discovering, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    Ok(())
}

/// test_expected_more_discovered_than_expected_transitions verifies that a
/// rack with more compute hosts present than the profile expects still
/// transitions out of Created: the Created handler only waits while counts
/// are below the expected minimum, so an over-count satisfies the threshold
/// and advances to Discovering.
#[crate::sqlx_test]
async fn test_expected_more_discovered_than_expected_transitions(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config_with_rack_profiles();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;

    // Rack profile "Single" expects 1 compute, 0 switches, 0 PS. Seed two
    // host machines tied to the rack so the actual compute count (2) exceeds
    // the expected count (1), exercising the over-discovery path.
    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Single")),
        &RackConfig::default(),
        None,
    )
    .await?;
    drop(txn);

    new_host(
        &env,
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        }),
    )
    .await?;
    new_host(
        &env,
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        }),
    )
    .await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &RackState::Created, &mut ctx)
        .await?;

    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => {
            assert!(
                matches!(next_state, RackState::Discovering),
                "Should transition to Discovering, got {:?}",
                next_state
            );
        }
        other => panic!(
            "Expected Transition to Discovering, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    Ok(())
}

/// test_discovering_waits_when_compute_not_ready verifies that the Discovering
/// handler waits (rather than erroring) when the rack does not yet have enough
/// Ready/Assigned compute hosts to satisfy the expected count.
#[crate::sqlx_test]
async fn test_discovering_waits_when_compute_not_ready(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config_with_rack_profiles();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;

    // Create a rack whose profile expects compute hosts but register none of
    // them, so no host has reached a Ready/Assigned state.
    let mut rack = db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("NVL72")),
        &RackConfig::default(),
        None,
    )
    .await?;

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    // The Discovering handler waits (does not fault) while not enough compute
    // hosts are Ready/Assigned.
    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &RackState::Discovering, &mut ctx)
        .await?;
    assert!(
        matches!(outcome, StateHandlerOutcome::Wait { .. }),
        "Discovering should wait when compute hosts are not yet ready, got {:?}",
        std::mem::discriminant(&outcome)
    );

    Ok(())
}

/// test_discovering_empty_rack_transitions_to_maintenance verifies that a
/// rack in Discovering state with no devices transitions to Maintenance.
#[crate::sqlx_test]
async fn test_discovering_empty_rack_transitions_to_maintenance(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config_with_rack_profiles();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;

    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Empty")),
        &RackConfig::default(),
        None,
    )
    .await?;

    let cfg = RackConfig::default();
    db_rack::update(&mut txn, &rack_id, &cfg).await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &RackState::Discovering, &mut ctx)
        .await?;

    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => {
            assert!(
                matches!(next_state, RackState::Maintenance { .. }),
                "Empty rack in Discovering should transition to Maintenance, got {:?}",
                next_state
            );
        }
        other => panic!(
            "Expected Transition to Maintenance, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    Ok(())
}

/// test_error_state_does_nothing verifies that the Error state logs and does nothing.
#[crate::sqlx_test]
async fn test_error_state_does_nothing(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Empty")),
        &RackConfig::default(),
        None,
    )
    .await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let error_state = RackState::Error {
        cause: "test error".to_string(),
    };
    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &error_state, &mut ctx)
        .await?;

    assert!(
        matches!(outcome, StateHandlerOutcome::Wait { .. }),
        "Error state should wait"
    );

    Ok(())
}

/// test_maintenance_completed_transitions_to_validation verifies that
/// Maintenance::Completed transitions to Validation(Pending).
#[crate::sqlx_test]
async fn test_maintenance_completed_transitions_to_validation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Empty")),
        &RackConfig::default(),
        None,
    )
    .await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let maintenance_state = RackState::Maintenance {
        maintenance_state: model::rack::RackMaintenanceState::Completed,
    };
    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &maintenance_state, &mut ctx)
        .await?;

    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => {
            assert!(
                matches!(
                    next_state,
                    RackState::Validating {
                        validating_state: RackValidationState::Pending,
                    }
                ),
                "Maintenance::Completed should transition to Validating(Pending), got {:?}",
                next_state
            );
        }
        other => panic!(
            "Expected Transition, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    Ok(())
}

/// test_ready_with_no_labels_stays_ready verifies that Ready with no
/// validation metadata labels on machines stays in Ready (do_nothing).
#[crate::sqlx_test]
async fn test_ready_with_no_labels_stays_ready(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Empty")),
        &RackConfig::default(),
        None,
    )
    .await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let ready_state = RackState::Ready;
    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &ready_state, &mut ctx)
        .await?;

    assert!(
        matches!(
            outcome,
            StateHandlerOutcome::Wait { .. } | StateHandlerOutcome::DoNothing { .. }
        ),
        "Ready with no labels should wait or do nothing, got {:?}",
        std::mem::discriminant(&outcome)
    );

    Ok(())
}

/// test_firmware_upgrade_start_skips_without_json verifies that
/// Maintenance::FirmwareUpgrade(Start) skips firmware flashing when no
/// firmware object JSON source is configured for rack maintenance.
#[crate::sqlx_test]
async fn test_firmware_upgrade_start_skips_without_json(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config_with_rack_profiles()),
            ..Default::default()
        },
    )
    .await;
    let (rack_id, host) = create_single_compute_rack(&env, &pool).await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler_instance = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let fw_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::Start,
        },
    };
    let mut outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &fw_state, &mut ctx)
        .await?;
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => {
            assert!(
                matches!(
                    next_state,
                    RackState::Maintenance {
                        maintenance_state: RackMaintenanceState::ConfigureNmxCluster {
                            configure_nmx_cluster: _,
                        },
                    }
                ),
                "FirmwareUpgrade(Start) should skip firmware without JSON and advance to the next requested activity, got {:?}",
                next_state
            );
        }
        other => panic!(
            "Expected Transition, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    let requests = env.rms_sim.submitted_apply_firmware_object_requests().await;
    assert!(
        requests.is_empty(),
        "firmware apply should not be submitted without a firmware object JSON source"
    );

    let machine = db::machine::find_one(
        &pool,
        &host.host_snapshot.id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .expect("machine should exist");
    assert!(machine.host_reprovision_requested.is_none());

    Ok(())
}

#[crate::sqlx_test]
async fn test_ingestion_transitions_to_firmware_upgrade_and_submits_rack_profile_firmware_object(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const FIRMWARE_OBJECT_URL: &str =
        "https://firmware.example.invalid/sot/single-rack-ingestion.json";

    const CONFIG_JSON: &str = r#"{"Id":"single-rack-ingestion"}"#;

    let firmware_object_fetcher = Arc::new(StaticFirmwareObjectFetcher {
        response: Mutex::new(Err("temporary SOT host failure".to_string())),
        requested_urls: Mutex::new(Vec::new()),
        requested_timeouts: Mutex::new(Vec::new()),
    });

    let mut config = config_with_rack_profiles();

    config
        .rack_profiles
        .rack_profiles
        .get_mut("Single")
        .unwrap()
        .rack_capabilities
        .compute
        .name = Some("GB200".to_string());

    let switch = &mut config
        .rack_profiles
        .rack_profiles
        .get_mut("Single")
        .unwrap()
        .rack_capabilities
        .switch;

    switch.count = 1;
    switch.name = Some("NVLinkSwitch".to_string());

    config
        .rack_profiles
        .rack_profiles
        .get_mut("Single")
        .unwrap()
        .firmware_object = Some(RackFirmwareObjectConfig {
        url: url::Url::parse(FIRMWARE_OBJECT_URL).unwrap(),
        fetch_timeout: std::time::Duration::from_secs(17),
    });

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config),
            firmware_object_fetcher: Some(firmware_object_fetcher.clone()),
            ..Default::default()
        },
    )
    .await;

    let (rack_id, host) = create_single_compute_rack(&env, &pool).await?;
    let machine_id = host.host_snapshot.id.to_string();
    let switch_id = attach_switch_with_nvos_credentials(&env, &rack_id).await?;
    set_switch_state(
        pool.acquire().await?.as_mut(),
        &switch_id,
        model::switch::SwitchControllerState::Ready,
    )
    .await;
    let switch_id = switch_id.to_string();

    env.rms_sim
        .queue_apply_firmware_object_response(rms::ApplyFirmwareObjectResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                message: "accepted".to_string(),
                job_id: "parent-job".to_string(),
                stats: Some(rms::NodeOperationStats {
                    total_nodes: 2,
                    successful_nodes: 2,
                    failed_nodes: 0,
                }),
                ..Default::default()
            }),
            object_id: "single-rack-ingestion".to_string(),
            jobs: vec![
                rms::NodeFirmwareJobInfo {
                    node_id: machine_id.clone(),
                    job_id: "compute-child-job".to_string(),
                },
                rms::NodeFirmwareJobInfo {
                    node_id: switch_id.clone(),
                    job_id: "switch-child-job".to_string(),
                },
            ],
        })
        .await;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let handler = RackStateHandler::default();

    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();

    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let ingestion_outcome = handler
        .handle_object_state(&rack_id, &mut rack, &RackState::Discovering, &mut ctx)
        .await?;
    let StateHandlerOutcome::Transition {
        next_state: firmware_state,
        ..
    } = ingestion_outcome
    else {
        panic!("ready ingestion inventory should transition to firmware maintenance");
    };
    assert!(matches!(
        firmware_state,
        RackState::Maintenance {
            maintenance_state: RackMaintenanceState::FirmwareUpgrade {
                rack_firmware_upgrade: FirmwareUpgradeState::Start,
            },
        }
    ));

    let error = match handler
        .handle_object_state(&rack_id, &mut rack, &firmware_state, &mut ctx)
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("SOT fetch failure should keep the state retryable"),
    };
    assert!(error.to_string().contains("temporary SOT host failure"));
    assert!(
        env.rms_sim
            .submitted_apply_firmware_object_requests()
            .await
            .is_empty()
    );

    *firmware_object_fetcher.response.lock().unwrap() = Ok("[]".to_string());

    let error = match handler
        .handle_object_state(&rack_id, &mut rack, &firmware_state, &mut ctx)
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("non-object SOT JSON should be rejected"),
    };
    assert!(
        error
            .to_string()
            .contains("configured SOT firmware object is not a JSON object")
    );
    assert!(
        env.rms_sim
            .submitted_apply_firmware_object_requests()
            .await
            .is_empty()
    );

    *firmware_object_fetcher.response.lock().unwrap() = Ok(CONFIG_JSON.to_string());

    let mut outcome = handler
        .handle_object_state(&rack_id, &mut rack, &firmware_state, &mut ctx)
        .await?;

    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    assert!(matches!(
        outcome,
        StateHandlerOutcome::Transition {
            next_state: RackState::Maintenance {
                maintenance_state: RackMaintenanceState::FirmwareUpgrade {
                    rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
                },
            },
            ..
        }
    ));

    assert_eq!(
        *firmware_object_fetcher.requested_urls.lock().unwrap(),
        vec![
            FIRMWARE_OBJECT_URL.to_string(),
            FIRMWARE_OBJECT_URL.to_string(),
            FIRMWARE_OBJECT_URL.to_string(),
        ]
    );

    assert_eq!(
        *firmware_object_fetcher.requested_timeouts.lock().unwrap(),
        vec![
            std::time::Duration::from_secs(17),
            std::time::Duration::from_secs(17),
            std::time::Duration::from_secs(17),
        ]
    );

    let requests = env.rms_sim.submitted_apply_firmware_object_requests().await;

    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].config_json, CONFIG_JSON);

    let requested_node_ids = requests[0]
        .nodes
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(
        requested_node_ids,
        std::collections::HashSet::from([machine_id.as_str(), switch_id.as_str()])
    );

    assert_eq!(
        requests[0].access_token.as_deref(),
        Some(carbide_rack::firmware_object::RMS_NOAUTH_ACCESS_TOKEN)
    );

    assert!(requests[0].component_filters.is_empty());
    assert!(!requests[0].force_update);

    let machine = db::machine::find_one(
        &pool,
        &host.host_snapshot.id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .expect("machine should exist");

    assert!(machine.host_reprovision_requested.is_some());

    Ok(())
}

#[crate::sqlx_test]
async fn test_firmware_upgrade_start_rejects_desired_off_machine_before_rms_submission(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config_with_rack_profiles()),
            ..Default::default()
        },
    )
    .await;
    let (rack_id, host) = create_single_compute_rack(&env, &pool).await?;
    set_machine_power_states(
        &pool,
        &host.host_snapshot.id,
        model::power_manager::PowerState::Off,
        model::power_manager::PowerState::On,
    )
    .await?;

    let config = RackConfig {
        maintenance_requested: Some(MaintenanceScope {
            machine_ids: vec![host.host_snapshot.id],
            activities: vec![MaintenanceActivity::FirmwareUpgrade {
                firmware_version: Some(r#"{"Id":"fw-json"}"#.to_string()),
                components: vec!["BMC".to_string()],
                force_update: false,
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut txn = pool.acquire().await?;
    db_rack::update(txn.as_mut(), &rack_id, &config).await?;
    drop(txn);
    env.api
        .credential_manager
        .set_credentials(
            &CredentialKey::RackMaintenanceAccessToken {
                rack_id: rack_id.clone(),
            },
            &Credentials::UsernamePassword {
                username: "access_token".to_string(),
                password: "token".to_string(),
            },
        )
        .await
        .map_err(|error| eyre::eyre!("failed to set maintenance access token: {}", error))?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };
    let fw_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::Start,
        },
    };

    let mut outcome = handler
        .handle_object_state(&rack_id, &mut rack, &fw_state, &mut ctx)
        .await?;
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    let StateHandlerOutcome::Transition {
        next_state: RackState::Error { cause },
        ..
    } = outcome
    else {
        panic!("desired-Off target should fail rack firmware start");
    };
    assert!(cause.contains(&host.host_snapshot.id.to_string()));
    assert!(cause.contains("desired power state is Off"));
    assert!(
        env.rms_sim
            .submitted_apply_firmware_object_requests()
            .await
            .is_empty()
    );

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(rack.config.maintenance_requested.is_none());
    let machine = db::machine::find_one(
        &pool,
        &host.host_snapshot.id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .expect("machine should exist");
    assert!(machine.host_reprovision_requested.is_none());
    let token = env
        .test_credential_manager
        .get_credentials(&CredentialKey::RackMaintenanceAccessToken {
            rack_id: rack_id.clone(),
        })
        .await
        .map_err(|error| eyre::eyre!("failed to get maintenance access token: {}", error))?;
    assert!(token.is_none());

    Ok(())
}

#[crate::sqlx_test]
async fn test_firmware_upgrade_start_submits_json_and_deletes_access_token(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let firmware_object_fetcher = Arc::new(StaticFirmwareObjectFetcher {
        response: Mutex::new(Err(
            "explicit firmware request must not fetch the profile URL".to_string(),
        )),
        requested_urls: Mutex::new(Vec::new()),
        requested_timeouts: Mutex::new(Vec::new()),
    });
    let mut config = config_with_rack_profiles();
    config
        .rack_profiles
        .rack_profiles
        .get_mut("NVL72")
        .unwrap()
        .firmware_object = Some(RackFirmwareObjectConfig {
        url: url::Url::parse("https://firmware.example.invalid/sot/nvl72.json").unwrap(),
        fetch_timeout: std::time::Duration::from_secs(11),
    });

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config),
            firmware_object_fetcher: Some(firmware_object_fetcher.clone()),
            ..Default::default()
        },
    )
    .await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;
    let switch_id_string = switch_id.to_string();

    crate::handlers::rack::on_demand_rack_maintenance(
        env.api.as_ref(),
        Request::new(rpc::forge::RackMaintenanceOnDemandRequest {
            rack_id: Some(rack_id.clone()),
            scope: Some(rpc::forge::RackMaintenanceScope {
                machine_ids: vec![],
                switch_ids: vec![switch_id_string.clone()],
                power_shelf_ids: vec![],
                activities: vec![rpc::forge::MaintenanceActivityConfig {
                    activity: Some(
                        rpc::forge::maintenance_activity_config::Activity::FirmwareUpgrade(
                            rpc::forge::FirmwareUpgradeActivity {
                                firmware_version: r#"{"Id":"fw-json"}"#.to_string(),
                                components: vec!["BMC".to_string()],
                                access_token: Some("token".to_string()),
                                force_update: true,
                            },
                        ),
                    ),
                }],
            }),
        }),
    )
    .await?;
    env.rms_sim
        .queue_apply_firmware_object_response(rms::ApplyFirmwareObjectResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                message: "accepted".to_string(),
                job_id: "parent-job".to_string(),
                stats: Some(rms::NodeOperationStats {
                    total_nodes: 1,
                    successful_nodes: 1,
                    failed_nodes: 0,
                }),
                ..Default::default()
            }),
            object_id: "fw-json".to_string(),
            jobs: vec![rms::NodeFirmwareJobInfo {
                node_id: switch_id_string.clone(),
                job_id: "child-job".to_string(),
            }],
        })
        .await;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let handler_instance = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let fw_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::Start,
        },
    };
    let mut outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &fw_state, &mut ctx)
        .await?;
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    assert!(
        matches!(
            outcome,
            StateHandlerOutcome::Transition {
                next_state: RackState::Maintenance {
                    maintenance_state: RackMaintenanceState::FirmwareUpgrade {
                        rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
                    },
                },
                ..
            }
        ),
        "expected FirmwareUpgrade(WaitForComplete), got {:?}",
        std::mem::discriminant(&outcome),
    );
    let requests = env.rms_sim.submitted_apply_firmware_object_requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].config_json, r#"{"Id":"fw-json"}"#);
    assert_eq!(requests[0].access_token.as_deref(), Some("token"));
    assert_eq!(requests[0].firmware_type, "prod");
    assert!(requests[0].force_update);
    assert_eq!(requests[0].nodes.as_ref().unwrap().nodes.len(), 1);
    assert!(
        firmware_object_fetcher
            .requested_urls
            .lock()
            .unwrap()
            .is_empty()
    );

    let token_after = env
        .test_credential_manager
        .get_credentials(&CredentialKey::RackMaintenanceAccessToken {
            rack_id: rack_id.clone(),
        })
        .await
        .expect("credential lookup should succeed");
    assert!(token_after.is_none());

    Ok(())
}

#[crate::sqlx_test]
async fn test_firmware_upgrade_start_missing_profile_deletes_access_token(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config_with_rack_profiles()),
            ..Default::default()
        },
    )
    .await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(&mut txn, &rack_id, None, &RackConfig::default(), None).await?;
    drop(txn);

    let switch_id = attach_switch_with_nvos_credentials(&env, &rack_id).await?;
    let config = RackConfig {
        maintenance_requested: Some(MaintenanceScope {
            switch_ids: vec![switch_id],
            activities: vec![MaintenanceActivity::FirmwareUpgrade {
                firmware_version: Some(r#"{"Id":"fw-json"}"#.to_string()),
                components: vec!["BMC".to_string()],
                force_update: false,
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut txn = pool.acquire().await?;
    db_rack::update(&mut txn, &rack_id, &config).await?;
    drop(txn);

    env.api
        .credential_manager
        .set_credentials(
            &CredentialKey::RackMaintenanceAccessToken {
                rack_id: rack_id.clone(),
            },
            &Credentials::UsernamePassword {
                username: "access_token".to_string(),
                password: "token".to_string(),
            },
        )
        .await
        .map_err(|error| eyre::eyre!("failed to set maintenance access token: {}", error))?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let handler_instance = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let fw_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::Start,
        },
    };
    let mut outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &fw_state, &mut ctx)
        .await?;
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    assert!(
        matches!(
            outcome,
            StateHandlerOutcome::Transition {
                next_state: RackState::Error { .. },
                ..
            }
        ),
        "expected missing rack profile to transition to Error"
    );
    let token_after = env
        .test_credential_manager
        .get_credentials(&CredentialKey::RackMaintenanceAccessToken {
            rack_id: rack_id.clone(),
        })
        .await
        .map_err(|error| eyre::eyre!("failed to get maintenance access token: {}", error))?;

    assert!(token_after.is_none());

    Ok(())
}

/// A machine that cannot consume its rack reprovision request because desired
/// power is Off fails the rack job without clearing requests already owned by
/// active device reprovisioning state machines.
#[crate::sqlx_test]
async fn test_firmware_upgrade_wait_for_complete_recovers_power_blocked_machine(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config_with_rack_profiles()),
            ..Default::default()
        },
    )
    .await;
    let (rack_id, blocked_host, active_host) = create_two_compute_rack(&env, &pool).await?;
    set_machine_power_states(
        &pool,
        &blocked_host.host_snapshot.id,
        model::power_manager::PowerState::Off,
        model::power_manager::PowerState::Off,
    )
    .await?;

    let scope = MaintenanceScope {
        machine_ids: vec![blocked_host.host_snapshot.id, active_host.host_snapshot.id],
        activities: vec![MaintenanceActivity::FirmwareUpgrade {
            firmware_version: Some(r#"{"Id":"fw-json"}"#.to_string()),
            components: vec!["BMC".to_string()],
            force_update: false,
        }],
        ..Default::default()
    };
    let job = FirmwareUpgradeJob {
        job_id: Some("parent-job".to_string()),
        status: Some("in_progress".to_string()),
        started_at: Some(chrono::Utc::now()),
        ..Default::default()
    };
    let initiator = format!("rack-{rack_id}");
    let mut txn = pool.begin().await?;
    let config = RackConfig {
        maintenance_requested: Some(scope),
        ..Default::default()
    };
    db_rack::update(txn.as_mut(), &rack_id, &config).await?;
    db_rack::update_firmware_upgrade_job(txn.as_mut(), &rack_id, Some(&job)).await?;
    db::host_machine_update::trigger_host_reprovisioning_request(
        txn.as_mut(),
        &initiator,
        &blocked_host.host_snapshot.id,
    )
    .await?;
    db::host_machine_update::trigger_host_reprovisioning_request(
        txn.as_mut(),
        &initiator,
        &active_host.host_snapshot.id,
    )
    .await?;
    txn.commit().await?;
    env.api
        .credential_manager
        .set_credentials(
            &CredentialKey::RackMaintenanceAccessToken {
                rack_id: rack_id.clone(),
            },
            &Credentials::UsernamePassword {
                username: "access_token".to_string(),
                password: "token".to_string(),
            },
        )
        .await
        .map_err(|error| eyre::eyre!("failed to set maintenance access token: {}", error))?;
    set_machine_host_reprovision_state(
        &pool,
        &active_host.host_snapshot.id,
        waiting_for_rack_firmware_upgrade_state(),
    )
    .await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };
    let fw_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
        },
    };

    let mut outcome = handler
        .handle_object_state(&rack_id, &mut rack, &fw_state, &mut ctx)
        .await?;

    let StateHandlerOutcome::Transition {
        next_state: RackState::Error { cause },
        ..
    } = &outcome
    else {
        panic!("power-blocked rack firmware job should transition to Error");
    };
    assert!(cause.contains(&blocked_host.host_snapshot.id.to_string()));
    assert!(!cause.contains(&active_host.host_snapshot.id.to_string()));

    let rack_before_transition = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(
        rack_before_transition
            .config
            .maintenance_requested
            .is_some()
    );
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(rack.config.maintenance_requested.is_none());
    let job = rack
        .firmware_upgrade_job
        .expect("failed firmware job should be retained");
    assert_eq!(job.status.as_deref(), Some("failed"));
    assert!(job.completed_at.is_some());

    let blocked_machine = db::machine::find_one(
        &pool,
        &blocked_host.host_snapshot.id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .expect("blocked machine should exist");
    assert!(blocked_machine.host_reprovision_requested.is_none());
    let active_machine = db::machine::find_one(
        &pool,
        &active_host.host_snapshot.id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .expect("active machine should exist");
    assert!(active_machine.host_reprovision_requested.is_some());
    let token = env
        .test_credential_manager
        .get_credentials(&CredentialKey::RackMaintenanceAccessToken {
            rack_id: rack_id.clone(),
        })
        .await
        .map_err(|error| eyre::eyre!("failed to get maintenance access token: {}", error))?;
    assert!(token.is_none());

    Ok(())
}

#[crate::sqlx_test]
async fn test_rack_maintenance_termination_unwinds_all_scoped_device_state(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config_with_rack_profiles()),
            ..Default::default()
        },
    )
    .await;
    let (rack_id, ready_host, active_host) = create_two_compute_rack(&env, &pool).await?;
    let switch_id = attach_switch_with_nvos_credentials(&env, &rack_id).await?;
    let initiator = format!("rack-{rack_id}");
    let now = chrono::Utc::now();
    let maintenance_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
        },
    };

    let mut txn = pool.begin().await?;
    let power_shelf_id = create_test_power_shelf(txn.as_mut(), &rack_id, 0xb1).await;
    set_switch_state(
        txn.as_mut(),
        &switch_id,
        model::switch::SwitchControllerState::Ready,
    )
    .await;
    set_power_shelf_state(
        txn.as_mut(),
        &power_shelf_id,
        model::power_shelf::PowerShelfControllerState::Ready,
    )
    .await;

    crate::tests::rack_state_controller::fixtures::rack::set_rack_controller_state(
        txn.as_mut(),
        &rack_id,
        maintenance_state.clone(),
    )
    .await?;
    let config = RackConfig {
        maintenance_requested: Some(MaintenanceScope::default()),
        maintenance_termination_requested: true,
        ..Default::default()
    };
    db_rack::update(txn.as_mut(), &rack_id, &config).await?;
    db_rack::update_firmware_upgrade_job(
        txn.as_mut(),
        &rack_id,
        Some(&FirmwareUpgradeJob {
            status: Some("in_progress".to_string()),
            started_at: Some(now),
            ..Default::default()
        }),
    )
    .await?;
    db_rack::update_nvos_update_job(
        txn.as_mut(),
        &rack_id,
        Some(&model::rack::NvosUpdateJob {
            status: Some("completed".to_string()),
            started_at: Some(now),
            completed_at: Some(now),
            ..Default::default()
        }),
    )
    .await?;

    for machine_id in [ready_host.host_snapshot.id, active_host.host_snapshot.id] {
        db::host_machine_update::trigger_host_reprovisioning_request(
            txn.as_mut(),
            &initiator,
            &machine_id,
        )
        .await?;
        db::machine::update_rack_fw_details(
            txn.as_mut(),
            &machine_id,
            Some(&RackFirmwareUpgradeStatus {
                task_id: "firmware-job".to_string(),
                status: RackFirmwareUpgradeState::InProgress,
                started_at: Some(now),
                ended_at: None,
            }),
        )
        .await?;
    }
    db_switch::set_switch_reprovisioning_requested(txn.as_mut(), switch_id, &initiator, vec![])
        .await?;
    db_switch::update_firmware_upgrade_status(
        txn.as_mut(),
        switch_id,
        Some(&RackFirmwareUpgradeStatus {
            task_id: "firmware-job".to_string(),
            status: RackFirmwareUpgradeState::InProgress,
            started_at: Some(now),
            ended_at: None,
        }),
    )
    .await?;
    db::power_shelf::set_power_shelf_reprovisioning_requested(
        txn.as_mut(),
        power_shelf_id,
        &initiator,
        vec![],
    )
    .await?;
    db::power_shelf::update_firmware_upgrade_status(
        txn.as_mut(),
        power_shelf_id,
        Some(&RackFirmwareUpgradeStatus {
            task_id: "firmware-job".to_string(),
            status: RackFirmwareUpgradeState::InProgress,
            started_at: Some(now),
            ended_at: None,
        }),
    )
    .await?;
    txn.commit().await?;

    set_machine_host_reprovision_state(
        &pool,
        &active_host.host_snapshot.id,
        waiting_for_rack_firmware_upgrade_state(),
    )
    .await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };
    let mut outcome = handler
        .handle_object_state(&rack_id, &mut rack, &maintenance_state, &mut ctx)
        .await?;
    let StateHandlerOutcome::Transition {
        next_state: RackState::Error { cause },
        ..
    } = &outcome
    else {
        panic!("termination should transition rack maintenance to Error");
    };
    assert!(cause.contains("terminated by operator"));
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(rack.config.maintenance_requested.is_none());
    assert!(!rack.config.maintenance_termination_requested);
    assert_eq!(
        rack.firmware_upgrade_job.unwrap().status.as_deref(),
        Some("failed")
    );
    assert_eq!(
        rack.nvos_update_job.unwrap().status.as_deref(),
        Some("completed")
    );

    let ready_machine = db::machine::find_one(
        &pool,
        &ready_host.host_snapshot.id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .unwrap();
    assert!(ready_machine.host_reprovision_requested.is_none());
    assert!(ready_machine.rack_fw_details.is_none());
    let active_machine = db::machine::find_one(
        &pool,
        &active_host.host_snapshot.id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .unwrap();
    assert!(active_machine.host_reprovision_requested.is_some());
    assert!(active_machine.rack_fw_details.is_none());

    let switch = db_switch::find_by_id(pool.acquire().await?.as_mut(), &switch_id)
        .await?
        .unwrap();
    assert!(switch.switch_reprovisioning_requested.is_none());
    assert!(switch.firmware_upgrade_status.is_none());
    assert!(switch.nvos_update_status.is_none());
    let power_shelf = db::power_shelf::find_by_id(pool.acquire().await?.as_mut(), &power_shelf_id)
        .await?
        .unwrap();
    assert!(power_shelf.power_shelf_reprovisioning_requested.is_none());
    assert!(power_shelf.firmware_upgrade_status.is_none());

    Ok(())
}

#[crate::sqlx_test]
async fn test_rack_maintenance_termination_fails_current_nvos_job(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;
    let now = chrono::Utc::now();
    let initiator = format!("rack-{rack_id}");
    let maintenance_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::NVOSUpdate {
            nvos_update: NvosUpdateState::WaitForComplete,
        },
    };
    let mut txn = pool.begin().await?;
    set_switch_state(
        txn.as_mut(),
        &switch_id,
        model::switch::SwitchControllerState::Ready,
    )
    .await;
    crate::tests::rack_state_controller::fixtures::rack::set_rack_controller_state(
        txn.as_mut(),
        &rack_id,
        maintenance_state.clone(),
    )
    .await?;
    db_rack::update(
        txn.as_mut(),
        &rack_id,
        &RackConfig {
            maintenance_requested: Some(MaintenanceScope {
                switch_ids: vec![switch_id],
                activities: vec![MaintenanceActivity::NvosUpdate {
                    config_json: r#"{"Id":"nvos"}"#.to_string(),
                }],
                ..Default::default()
            }),
            maintenance_termination_requested: true,
            ..Default::default()
        },
    )
    .await?;
    db_rack::update_nvos_update_job(
        txn.as_mut(),
        &rack_id,
        Some(&model::rack::NvosUpdateJob {
            status: Some("in_progress".to_string()),
            started_at: Some(now),
            ..Default::default()
        }),
    )
    .await?;
    db_switch::set_switch_reprovisioning_requested(
        txn.as_mut(),
        switch_id,
        &initiator,
        vec![MaintenanceActivity::NvosUpdate {
            config_json: r#"{"Id":"nvos"}"#.to_string(),
        }],
    )
    .await?;
    db_switch::update_nvos_update_status(
        txn.as_mut(),
        switch_id,
        Some(&SwitchNvosUpdateStatus {
            task_id: "nvos-job".to_string(),
            firmware_id: "nvos".to_string(),
            image_filename: "nvos.img".to_string(),
            status: SwitchNvosUpdateState::InProgress,
            started_at: Some(now),
            ended_at: None,
        }),
    )
    .await?;
    txn.commit().await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };
    let mut outcome = handler
        .handle_object_state(&rack_id, &mut rack, &maintenance_state, &mut ctx)
        .await?;
    assert!(matches!(
        outcome,
        StateHandlerOutcome::Transition {
            next_state: RackState::Error { .. },
            ..
        }
    ));
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert_eq!(
        rack.nvos_update_job.unwrap().status.as_deref(),
        Some("failed")
    );
    let switch = db_switch::find_by_id(pool.acquire().await?.as_mut(), &switch_id)
        .await?
        .unwrap();
    assert!(switch.switch_reprovisioning_requested.is_none());
    assert!(switch.nvos_update_status.is_none());

    Ok(())
}

/// test_firmware_upgrade_wait_for_complete_waits_while_jobs_running verifies
/// that WaitForComplete remains in a wait state while machines are still in
/// WaitingForRackFirmwareUpgrade and writes in-progress rack firmware status
/// back to the machine from RMS.
#[crate::sqlx_test]
async fn test_firmware_upgrade_wait_for_complete_waits_while_jobs_running(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config_with_rack_profiles()),
            ..Default::default()
        },
    )
    .await;
    let (rack_id, host) = create_single_compute_rack(&env, &pool).await?;
    set_machine_host_reprovision_state(
        &pool,
        &host.host_snapshot.id,
        waiting_for_rack_firmware_upgrade_state(),
    )
    .await?;
    env.rms_sim
        .set_firmware_job_status(librms::protos::rack_manager::GetFirmwareJobStatusResponse {
            status: librms::protos::rack_manager::ReturnCode::Success as i32,
            job_id: "child-job-1".to_string(),
            job_state: rms::FirmwareJobState::Running as i32,
            state_description: "running".to_string(),
            node_id: host.host_snapshot.id.to_string(),
            ..Default::default()
        })
        .await;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    rack.firmware_upgrade_job = Some(FirmwareUpgradeJob {
        job_id: Some("batch-job-1".to_string()),
        status: Some("in_progress".to_string()),
        started_at: Some(chrono::Utc::now()),
        batch_job_ids: vec!["batch-job-1".to_string()],
        machines: vec![FirmwareUpgradeDeviceStatus {
            node_id: host.host_snapshot.id.to_string(),
            mac: "00:11:22:33:44:55".to_string(),
            bmc_ip: "192.0.2.10".to_string(),
            status: "in_progress".to_string(),
            job_id: Some("child-job-1".to_string()),
            parent_job_id: Some("batch-job-1".to_string()),
            error_message: None,
        }],
        ..Default::default()
    });

    let handler_instance = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let fw_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
        },
    };
    let mut outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &fw_state, &mut ctx)
        .await?;
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    let StateHandlerOutcome::Wait { reason, .. } = outcome else {
        panic!("Expected Wait while machine controller is still WaitingForRackFirmwareUpgrade");
    };
    assert!(reason.contains(&host.host_snapshot.id.to_string()));
    assert!(reason.contains("pending=1"));

    let machine = db::machine::find_one(
        &pool,
        &host.host_snapshot.id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .expect("machine should exist");
    let rack_fw_details = machine
        .rack_fw_details
        .expect("machine should have rack firmware status");
    assert_eq!(rack_fw_details.status, RackFirmwareUpgradeState::InProgress);
    assert!(rack_fw_details.ended_at.is_none());

    Ok(())
}

/// test_firmware_upgrade_wait_for_complete_transitions_to_error_on_job_failure
/// verifies that a machine left WaitingForRackFirmwareUpgrade in
/// FailedFirmwareUpgrade moves the rack into Error.
#[crate::sqlx_test]
async fn test_firmware_upgrade_wait_for_complete_transitions_to_error_on_job_failure(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config_with_rack_profiles()),
            ..Default::default()
        },
    )
    .await;
    let (rack_id, host) = create_single_compute_rack(&env, &pool).await?;
    set_machine_host_reprovision_state(
        &pool,
        &host.host_snapshot.id,
        failed_rack_firmware_upgrade_state(),
    )
    .await?;
    env.rms_sim
        .set_firmware_job_status(librms::protos::rack_manager::GetFirmwareJobStatusResponse {
            status: librms::protos::rack_manager::ReturnCode::Success as i32,
            job_id: "child-job-1".to_string(),
            job_state: rms::FirmwareJobState::Failed as i32,
            state_description: "failed".to_string(),
            node_id: host.host_snapshot.id.to_string(),
            error_message: "upgrade failed".to_string(),
            ..Default::default()
        })
        .await;

    let scope = MaintenanceScope {
        machine_ids: vec![host.host_snapshot.id],
        activities: vec![MaintenanceActivity::FirmwareUpgrade {
            firmware_version: Some(r#"{"Id":"fw-json"}"#.to_string()),
            components: vec!["BMC".to_string()],
            force_update: false,
        }],
        ..Default::default()
    };
    let job = FirmwareUpgradeJob {
        job_id: Some("batch-job-1".to_string()),
        status: Some("in_progress".to_string()),
        started_at: Some(chrono::Utc::now()),
        batch_job_ids: vec!["batch-job-1".to_string()],
        machines: vec![FirmwareUpgradeDeviceStatus {
            node_id: host.host_snapshot.id.to_string(),
            mac: "00:11:22:33:44:55".to_string(),
            bmc_ip: "192.0.2.10".to_string(),
            status: "in_progress".to_string(),
            job_id: Some("child-job-1".to_string()),
            parent_job_id: Some("batch-job-1".to_string()),
            error_message: None,
        }],
        ..Default::default()
    };
    let mut txn = pool.begin().await?;
    db_rack::update(
        txn.as_mut(),
        &rack_id,
        &RackConfig {
            maintenance_requested: Some(scope),
            ..Default::default()
        },
    )
    .await?;
    db_rack::update_firmware_upgrade_job(txn.as_mut(), &rack_id, Some(&job)).await?;
    txn.commit().await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler_instance = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let fw_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
        },
    };
    let mut outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &fw_state, &mut ctx)
        .await?;
    match &outcome {
        StateHandlerOutcome::Transition { next_state, .. } => {
            assert!(
                matches!(next_state, RackState::Error { .. }),
                "Expected rack to transition to Error, got {:?}",
                next_state
            );
        }
        other => panic!(
            "Expected Transition to Error, got {:?}",
            std::mem::discriminant(other)
        ),
    }

    let rack_before_transition = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(
        rack_before_transition
            .config
            .maintenance_requested
            .is_some()
    );
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(rack.config.maintenance_requested.is_none());
    assert_eq!(
        rack.firmware_upgrade_job.unwrap().status.as_deref(),
        Some("failed")
    );

    let machine = db::machine::find_one(
        &pool,
        &host.host_snapshot.id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .expect("machine should exist");
    let rack_fw_details = machine
        .rack_fw_details
        .expect("machine should have rack firmware status");
    assert!(matches!(
        rack_fw_details.status,
        RackFirmwareUpgradeState::Failed { .. }
    ));
    assert!(rack_fw_details.ended_at.is_some());

    Ok(())
}

/// test_firmware_upgrade_wait_for_complete_waits_for_all_nodes_to_be_terminal_before_error
/// verifies that the rack keeps waiting while any tracked machine is still in
/// WaitingForRackFirmwareUpgrade, then errors only after every machine has left
/// that wait state and at least one failed.
#[crate::sqlx_test]
async fn test_firmware_upgrade_wait_for_complete_waits_for_all_nodes_to_be_terminal_before_error(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config_with_rack_profiles()),
            ..Default::default()
        },
    )
    .await;
    let (rack_id, host_a, host_b) = create_two_compute_rack(&env, &pool).await?;
    set_machine_host_reprovision_state(
        &pool,
        &host_a.host_snapshot.id,
        waiting_for_rack_firmware_upgrade_state(),
    )
    .await?;
    set_machine_host_reprovision_state(
        &pool,
        &host_b.host_snapshot.id,
        waiting_for_rack_firmware_upgrade_state(),
    )
    .await?;

    env.rms_sim
        .set_firmware_job_status(librms::protos::rack_manager::GetFirmwareJobStatusResponse {
            status: librms::protos::rack_manager::ReturnCode::Success as i32,
            job_id: "child-job-1".to_string(),
            job_state: rms::FirmwareJobState::Failed as i32,
            state_description: "failed".to_string(),
            node_id: host_a.host_snapshot.id.to_string(),
            error_message: "upgrade failed".to_string(),
            ..Default::default()
        })
        .await;
    env.rms_sim
        .set_firmware_job_status(librms::protos::rack_manager::GetFirmwareJobStatusResponse {
            status: librms::protos::rack_manager::ReturnCode::Success as i32,
            job_id: "child-job-2".to_string(),
            job_state: rms::FirmwareJobState::Running as i32,
            state_description: "running".to_string(),
            node_id: host_b.host_snapshot.id.to_string(),
            ..Default::default()
        })
        .await;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    rack.firmware_upgrade_job = Some(FirmwareUpgradeJob {
        job_id: Some("batch-job-1".to_string()),
        status: Some("in_progress".to_string()),
        started_at: Some(chrono::Utc::now()),
        batch_job_ids: vec!["batch-job-1".to_string()],
        machines: vec![
            FirmwareUpgradeDeviceStatus {
                node_id: host_a.host_snapshot.id.to_string(),
                mac: "00:11:22:33:44:55".to_string(),
                bmc_ip: "192.0.2.10".to_string(),
                status: "in_progress".to_string(),
                job_id: Some("child-job-1".to_string()),
                parent_job_id: Some("batch-job-1".to_string()),
                error_message: None,
            },
            FirmwareUpgradeDeviceStatus {
                node_id: host_b.host_snapshot.id.to_string(),
                mac: "00:11:22:33:44:66".to_string(),
                bmc_ip: "192.0.2.11".to_string(),
                status: "in_progress".to_string(),
                job_id: Some("child-job-2".to_string()),
                parent_job_id: Some("batch-job-1".to_string()),
                error_message: None,
            },
        ],
        ..Default::default()
    });

    let handler_instance = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let fw_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
        },
    };
    let mut outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &fw_state, &mut ctx)
        .await?;
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    assert!(
        matches!(outcome, StateHandlerOutcome::Wait { .. }),
        "Expected Wait while some tracked machines are still WaitingForRackFirmwareUpgrade"
    );

    let machine_a = db::machine::find_one(
        &pool,
        &host_a.host_snapshot.id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .expect("machine A should exist");
    let machine_b = db::machine::find_one(
        &pool,
        &host_b.host_snapshot.id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .expect("machine B should exist");
    assert!(matches!(
        machine_a
            .rack_fw_details
            .as_ref()
            .expect("machine A rack fw details")
            .status,
        RackFirmwareUpgradeState::Failed { .. }
    ));
    assert_eq!(
        machine_b
            .rack_fw_details
            .as_ref()
            .expect("machine B rack fw details")
            .status,
        RackFirmwareUpgradeState::InProgress
    );

    set_machine_host_reprovision_state(
        &pool,
        &host_a.host_snapshot.id,
        failed_rack_firmware_upgrade_state(),
    )
    .await?;
    set_machine_host_reprovision_state(
        &pool,
        &host_b.host_snapshot.id,
        waiting_for_rack_firmware_upgrade_state(),
    )
    .await?;

    env.rms_sim
        .set_firmware_job_status(librms::protos::rack_manager::GetFirmwareJobStatusResponse {
            status: librms::protos::rack_manager::ReturnCode::Success as i32,
            job_id: "child-job-2".to_string(),
            job_state: rms::FirmwareJobState::Completed as i32,
            state_description: "completed".to_string(),
            node_id: host_b.host_snapshot.id.to_string(),
            ..Default::default()
        })
        .await;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let mut outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &fw_state, &mut ctx)
        .await?;
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    assert!(
        matches!(outcome, StateHandlerOutcome::Wait { .. }),
        "Expected Wait while machine B is still WaitingForRackFirmwareUpgrade"
    );

    set_machine_host_reprovision_state(
        &pool,
        &host_b.host_snapshot.id,
        completed_rack_firmware_upgrade_state(),
    )
    .await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let mut outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &fw_state, &mut ctx)
        .await?;
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => {
            assert!(
                matches!(next_state, RackState::Error { .. }),
                "Expected rack to transition to Error after all tracked machines left firmware wait with a failure, got {:?}",
                next_state
            );
        }
        other => panic!(
            "Expected Transition to Error, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    let machine_b = db::machine::find_one(
        &pool,
        &host_b.host_snapshot.id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .expect("machine B should exist");
    assert_eq!(
        machine_b
            .rack_fw_details
            .as_ref()
            .expect("machine B rack fw details")
            .status,
        RackFirmwareUpgradeState::Completed
    );

    Ok(())
}

/// test_firmware_upgrade_wait_for_complete_retries_when_job_lookup_fails
/// verifies that a response-level lookup failure from GetFirmwareJobStatus does
/// not mark the device failed and instead keeps the rack waiting while the
/// machine remains in WaitingForRackFirmwareUpgrade.
#[crate::sqlx_test]
async fn test_firmware_upgrade_wait_for_complete_retries_when_job_lookup_fails(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config_with_rack_profiles()),
            ..Default::default()
        },
    )
    .await;
    let (rack_id, host) = create_single_compute_rack(&env, &pool).await?;
    set_machine_host_reprovision_state(
        &pool,
        &host.host_snapshot.id,
        waiting_for_rack_firmware_upgrade_state(),
    )
    .await?;
    env.rms_sim
        .set_firmware_job_status(librms::protos::rack_manager::GetFirmwareJobStatusResponse {
            status: librms::protos::rack_manager::ReturnCode::Failure as i32,
            job_id: "child-job-1".to_string(),
            state_description: "Job not found".to_string(),
            error_message: "Job not found: child-job-1".to_string(),
            ..Default::default()
        })
        .await;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    rack.firmware_upgrade_job = Some(FirmwareUpgradeJob {
        job_id: Some("batch-job-1".to_string()),
        status: Some("in_progress".to_string()),
        started_at: Some(chrono::Utc::now()),
        batch_job_ids: vec!["batch-job-1".to_string()],
        machines: vec![FirmwareUpgradeDeviceStatus {
            node_id: host.host_snapshot.id.to_string(),
            mac: "00:11:22:33:44:55".to_string(),
            bmc_ip: "192.0.2.10".to_string(),
            status: "in_progress".to_string(),
            job_id: Some("child-job-1".to_string()),
            parent_job_id: Some("batch-job-1".to_string()),
            error_message: None,
        }],
        ..Default::default()
    });

    let handler_instance = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let fw_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
        },
    };
    let mut outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &fw_state, &mut ctx)
        .await?;
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    assert!(
        matches!(outcome, StateHandlerOutcome::Wait { .. }),
        "Expected Wait while RMS job lookup is unavailable"
    );

    let persisted_rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let job = persisted_rack
        .firmware_upgrade_job
        .expect("rack firmware job should still be persisted");
    assert_eq!(job.status.as_deref(), Some("in_progress"));
    assert_eq!(job.machines[0].status, "in_progress");
    assert_eq!(
        job.machines[0].error_message.as_deref(),
        Some("Job not found: child-job-1")
    );

    Ok(())
}

/// test_firmware_upgrade_wait_for_complete_retries_on_transient_poll_error
/// verifies that transport-level polling failures do not immediately fail the
/// rack upgrade while machines remain in WaitingForRackFirmwareUpgrade.
#[crate::sqlx_test]
async fn test_firmware_upgrade_wait_for_complete_retries_on_transient_poll_error(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config_with_rack_profiles()),
            ..Default::default()
        },
    )
    .await;
    let (rack_id, host) = create_single_compute_rack(&env, &pool).await?;
    set_machine_host_reprovision_state(
        &pool,
        &host.host_snapshot.id,
        waiting_for_rack_firmware_upgrade_state(),
    )
    .await?;
    env.rms_sim
        .set_firmware_job_error("child-job-1", "mock transport failure")
        .await;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    rack.firmware_upgrade_job = Some(FirmwareUpgradeJob {
        job_id: Some("batch-job-1".to_string()),
        status: Some("in_progress".to_string()),
        started_at: Some(chrono::Utc::now()),
        batch_job_ids: vec!["batch-job-1".to_string()],
        machines: vec![FirmwareUpgradeDeviceStatus {
            node_id: host.host_snapshot.id.to_string(),
            mac: "00:11:22:33:44:55".to_string(),
            bmc_ip: "192.0.2.10".to_string(),
            status: "in_progress".to_string(),
            job_id: Some("child-job-1".to_string()),
            parent_job_id: Some("batch-job-1".to_string()),
            error_message: None,
        }],
        ..Default::default()
    });

    let handler_instance = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let fw_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
        },
    };
    let mut outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &fw_state, &mut ctx)
        .await?;
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    assert!(
        matches!(outcome, StateHandlerOutcome::Wait { .. }),
        "Expected Wait while RMS polling has a transport error"
    );

    let persisted_rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let job = persisted_rack
        .firmware_upgrade_job
        .expect("rack firmware job should still be persisted");
    assert_eq!(job.status.as_deref(), Some("in_progress"));
    assert_eq!(job.machines[0].status, "in_progress");
    assert!(
        job.machines[0]
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("mock transport failure"))
    );

    Ok(())
}

/// A retryable RMS rejection preserves the access token so the next
/// `NVOSUpdate(Start)` iteration can resubmit the request.
#[crate::sqlx_test]
async fn test_nvos_update_start_retries_rejection_with_access_token(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config_with_nmx_cluster_profile()),
            ..Default::default()
        },
    )
    .await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("NmxCluster")),
        &RackConfig::default(),
        None,
    )
    .await?;
    drop(txn);
    let switch_id = attach_switch_with_nvos_credentials(&env, &rack_id).await?;
    let config = RackConfig {
        maintenance_requested: Some(MaintenanceScope {
            switch_ids: vec![switch_id],
            activities: vec![MaintenanceActivity::NvosUpdate {
                config_json: r#"{"Id":"fw-nvos-default"}"#.to_string(),
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut txn = pool.acquire().await?;
    db_rack::update(&mut txn, &rack_id, &config).await?;

    crate::tests::rack_state_controller::fixtures::rack::set_rack_controller_state(
        &mut txn,
        &rack_id,
        RackState::Maintenance {
            maintenance_state: RackMaintenanceState::NVOSUpdate {
                nvos_update: NvosUpdateState::Start,
            },
        },
    )
    .await?;

    drop(txn);

    env.api
        .credential_manager
        .set_credentials(
            &CredentialKey::RackMaintenanceAccessToken {
                rack_id: rack_id.clone(),
            },
            &Credentials::UsernamePassword {
                username: "access_token".to_string(),
                password: "token".to_string(),
            },
        )
        .await
        .map_err(|error| eyre::eyre!("failed to set maintenance access token: {}", error))?;

    env.rms_sim
        .queue_apply_switch_system_image_response(
            librms::protos::rack_manager::ApplySwitchSystemImageResponse {
                response: Some(librms::protos::rack_manager::NodeBatchResponse {
                    status: librms::protos::rack_manager::ReturnCode::Failure as i32,
                    message: "RMS temporarily rejected the NVOS update".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;

    env.rms_sim
        .queue_apply_switch_system_image_response(
            librms::protos::rack_manager::ApplySwitchSystemImageResponse {
                response: Some(librms::protos::rack_manager::NodeBatchResponse {
                    status: librms::protos::rack_manager::ReturnCode::Success as i32,
                    job_id: "nvos-job-1".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;

    env.run_rack_controller_iteration().await;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    assert!(matches!(
        rack.controller_state.value,
        RackState::Maintenance {
            maintenance_state: RackMaintenanceState::NVOSUpdate {
                nvos_update: NvosUpdateState::Start,
            },
        }
    ));

    assert!(rack.nvos_update_job.is_none());

    let token = env
        .api
        .credential_manager
        .get_credentials(&CredentialKey::RackMaintenanceAccessToken {
            rack_id: rack_id.clone(),
        })
        .await
        .map_err(|error| eyre::eyre!("failed to get maintenance access token: {}", error))?
        .expect("retryable submission should preserve the access token");

    assert_eq!(
        token,
        Credentials::UsernamePassword {
            username: "access_token".to_string(),
            password: "token".to_string(),
        }
    );

    let requests = env
        .rms_sim
        .submitted_apply_switch_system_image_requests()
        .await;

    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].config_json, r#"{"Id":"fw-nvos-default"}"#);
    assert_eq!(requests[0].access_token.as_deref(), Some("token"));

    env.run_rack_controller_iteration().await;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    assert!(rack.nvos_update_job.is_some());

    assert!(matches!(
        rack.controller_state.value,
        RackState::Maintenance {
            maintenance_state: RackMaintenanceState::NVOSUpdate {
                nvos_update: NvosUpdateState::WaitForComplete,
            },
        }
    ));

    assert!(
        env.api
            .credential_manager
            .get_credentials(&CredentialKey::RackMaintenanceAccessToken {
                rack_id: rack_id.clone(),
            })
            .await
            .map_err(|error| eyre::eyre!("failed to get maintenance access token: {}", error))?
            .is_none()
    );

    let requests = env
        .rms_sim
        .submitted_apply_switch_system_image_requests()
        .await;

    assert_eq!(requests.len(), 2);

    assert!(
        requests
            .iter()
            .all(|request| request.access_token.as_deref() == Some("token"))
    );

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");

    assert!(switch.switch_reprovisioning_requested.is_none());

    Ok(())
}

#[crate::sqlx_test]
async fn test_nvos_update_wait_for_complete_persists_completed_status(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config_with_rack_profiles()),
            ..Default::default()
        },
    )
    .await;

    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;
    let started_at = chrono::Utc::now();
    let job_id = "nvos-job-1";

    let config = RackConfig {
        maintenance_requested: Some(MaintenanceScope {
            switch_ids: vec![switch_id],
            activities: vec![MaintenanceActivity::NvosUpdate {
                config_json: r#"{"Id":"fw-nvos-default"}"#.to_string(),
            }],
            ..Default::default()
        }),
        ..Default::default()
    };

    let job = NvosUpdateJob {
        job_id: Some("parent-job".to_string()),
        firmware_id: "fw-nvos-default".to_string(),
        image_filename: "nvos.img".to_string(),
        status: Some("in_progress".to_string()),
        started_at: Some(started_at),
        switches: vec![NvosUpdateSwitchStatus {
            node_id: switch_id.to_string(),
            mac: "00:11:22:33:44:55".to_string(),
            bmc_ip: "192.0.2.10".to_string(),
            nvos_ip: "192.0.2.20".to_string(),
            status: "in_progress".to_string(),
            job_id: Some(job_id.to_string()),
            error_message: None,
        }],
        ..Default::default()
    };

    let mut txn = pool.begin().await?;
    db_rack::update(txn.as_mut(), &rack_id, &config).await?;
    db_rack::update_nvos_update_job(txn.as_mut(), &rack_id, Some(&job)).await?;
    txn.commit().await?;

    env.rms_sim
        .set_switch_system_image_job_status(rms::GetSwitchSystemImageJobStatusResponse {
            status: rms::ReturnCode::Success as i32,
            job_id: job_id.to_string(),
            state: "completed".to_string(),
            node_id: switch_id.to_string(),
            ..Default::default()
        })
        .await;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let handler_instance = RackStateHandler::default();

    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();

    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let nvos_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::NVOSUpdate {
            nvos_update: NvosUpdateState::WaitForComplete,
        },
    };

    let mut outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &nvos_state, &mut ctx)
        .await?;

    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await?;
    }

    assert!(matches!(
        outcome,
        StateHandlerOutcome::Transition {
            next_state: RackState::Maintenance {
                maintenance_state: RackMaintenanceState::Completed,
            },
            ..
        }
    ));

    let persisted_rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let persisted_job = persisted_rack
        .nvos_update_job
        .expect("completed NVOS job should be persisted");

    assert_eq!(persisted_job.status.as_deref(), Some("completed"));
    assert!(persisted_job.completed_at.is_some());

    let mut txn = pool.acquire().await?;

    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");

    let switch_status = switch
        .nvos_update_status
        .expect("completed switch NVOS status should be persisted");

    assert_eq!(switch_status.task_id, job_id);

    assert!(matches!(
        switch_status.status,
        SwitchNvosUpdateState::Completed
    ));

    Ok(())
}

async fn queue_configure_nmx_cluster_v2_success(
    env: &TestEnv,
    switch_ids: &[SwitchId],
    secondary_switch_id: SwitchId,
    primary_switch_id: SwitchId,
    topology_type: &str,
) {
    env.rms_sim
        .queue_configure_scale_up_fabric_manager_v2_response(Ok(
            rms_v2::ConfigureScaleUpFabricManagerResponse {
                job_id: "configure-scale-up-fabric-job".to_string(),
            },
        ))
        .await;

    env.rms_sim
        .queue_get_job_status_response(Ok(rms::GetJobStatusResponse {
            job_states: vec![rms::JobStatus {
                job_id: "configure-scale-up-fabric-job".to_string(),
                execution_state: rms::JobExecutionState::Completed as i32,
                state_description: "completed".to_string(),
                ..Default::default()
            }],
        }))
        .await;

    env.rms_sim
        .queue_get_scale_up_fabric_status_response(Ok(rms::GetScaleUpFabricStatusResponse {
            status: rms::ReturnCode::Success as i32,
            fabric_status: Some(rms::ScaleUpFabricStatus {
                topology_type: topology_type.to_string(),
                switches: vec![
                    rms::ScaleUpFabricSwitchStatus {
                        node_id: secondary_switch_id.to_string(),
                        enabled: false,
                        fabric_manager_status: "ok".to_string(),
                        ..Default::default()
                    },
                    rms::ScaleUpFabricSwitchStatus {
                        node_id: primary_switch_id.to_string(),
                        enabled: true,
                        fabric_manager_status: "ok".to_string(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }),
            error_message: String::new(),
        }))
        .await;

    let fabric_manager_status_json =
        format!(r#"{{"status":"ok","addition-info":"{CONTROL_PLANE_STATE_CONFIGURED}"}}"#);

    env.rms_sim
        .queue_batch_get_scale_up_fabric_service_status_response(Ok(
            rms::BatchGetScaleUpFabricServiceStatusResponse {
                status: rms::ReturnCode::Success as i32,
                service_statuses: switch_ids
                    .iter()
                    .map(|switch_id| {
                        (
                            switch_id.to_string(),
                            rms::ScaleUpFabricServiceStatusEntry {
                                status_json: fabric_manager_status_json.clone(),
                                error_message: String::new(),
                            },
                        )
                    })
                    .collect(),
                ..Default::default()
            },
        ))
        .await;
}

async fn run_configure_nmx_cluster_v2_workflow(
    env: &TestEnv,
    rack_id: &RackId,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut rack = get_db_rack(env.db_reader().as_mut(), rack_id).await;
    let handler_instance = RackStateHandler::default();

    let mut services = env.rack_state_handler_services();
    services.rms_client = None;
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();

    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let start = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::ConfigureNmxCluster {
            configure_nmx_cluster: ConfigureNmxClusterState::Start,
        },
    };

    let job_wait = match handler_instance
        .handle_object_state(rack_id, &mut rack, &start, &mut ctx)
        .await?
    {
        StateHandlerOutcome::Transition { next_state, .. } => next_state,
        other => panic!(
            "Expected Transition, got {:?}",
            std::mem::discriminant(&other)
        ),
    };

    assert!(matches!(
        job_wait,
        RackState::Maintenance {
            maintenance_state: RackMaintenanceState::ConfigureNmxCluster {
                configure_nmx_cluster:
                    ConfigureNmxClusterState::WaitForScaleUpFabricManagerJob {
                        ref job_id,
                    },
            },
        } if job_id == "configure-scale-up-fabric-job"
    ));

    let next = match handler_instance
        .handle_object_state(rack_id, &mut rack, &job_wait, &mut ctx)
        .await?
    {
        StateHandlerOutcome::Transition { next_state, .. } => next_state,
        other => panic!(
            "Expected Transition, got {:?}",
            std::mem::discriminant(&other)
        ),
    };

    assert!(matches!(
        next,
        RackState::Maintenance {
            maintenance_state: RackMaintenanceState::Completed,
        }
    ));

    Ok(())
}

async fn assert_configure_nmx_cluster_v2_results(
    env: &TestEnv,
    pool: &sqlx::PgPool,
    switch_ids: &[SwitchId],
    secondary_switch_id: SwitchId,
    primary_switch_id: SwitchId,
    topology_type: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    assert!(
        env.rms_sim
            .submitted_configure_switch_certificate_requests()
            .await
            .is_empty()
    );

    assert!(
        env.rms_sim
            .submitted_batch_set_scale_up_fabric_state_requests()
            .await
            .is_empty()
    );

    assert!(
        env.rms_sim
            .submitted_batch_get_node_device_info_requests()
            .await
            .is_empty()
    );

    let configure_requests = env
        .rms_sim
        .submitted_configure_scale_up_fabric_manager_v2_requests()
        .await;

    let [configure_request] = configure_requests.as_slice() else {
        return Err(eyre::eyre!("expected exactly one V2 configure request").into());
    };

    let desired = configure_request
        .config
        .as_ref()
        .ok_or_else(|| eyre::eyre!("configure request should include desired fabric config"))?;

    assert_eq!(desired.topology_type, topology_type);
    assert!(desired.extra_static_configs.is_empty());
    assert_eq!(configure_request.primary_switch_node_id, None);
    assert_eq!(configure_request.domain, None);

    let configure_nodes = &configure_request
        .nodes
        .as_ref()
        .ok_or_else(|| eyre::eyre!("configure request should include all switches"))?
        .nodes;

    assert_eq!(configure_nodes.len(), switch_ids.len());

    assert!(switch_ids.iter().all(|switch_id| {
        configure_nodes
            .iter()
            .any(|node| node.node_id == switch_id.to_string())
    }));

    let job_status_requests = env.rms_sim.submitted_get_job_status_requests().await;

    let [job_status_request] = job_status_requests.as_slice() else {
        return Err(eyre::eyre!("expected exactly one job status request").into());
    };

    assert_eq!(job_status_request.job_id, "configure-scale-up-fabric-job");
    assert!(!job_status_request.include_child_job_states);

    let status_requests = env
        .rms_sim
        .submitted_get_scale_up_fabric_status_requests()
        .await;

    let [status_request] = status_requests.as_slice() else {
        return Err(eyre::eyre!("expected exactly one fabric status request").into());
    };

    assert_eq!(status_request.domain, None);

    let status_nodes = &status_request
        .nodes
        .as_ref()
        .ok_or_else(|| eyre::eyre!("status request should include all switches"))?
        .nodes;

    assert_eq!(status_nodes.len(), switch_ids.len());

    assert!(switch_ids.iter().all(|switch_id| {
        status_nodes
            .iter()
            .any(|node| node.node_id == switch_id.to_string())
    }));

    let fabric_manager_status_requests = env
        .rms_sim
        .submitted_batch_get_scale_up_fabric_service_status_requests()
        .await;

    let [fabric_manager_status_request] = fabric_manager_status_requests.as_slice() else {
        return Err(eyre::eyre!("expected exactly one fabric manager status request").into());
    };

    let fabric_manager_status_nodes = &fabric_manager_status_request
        .nodes
        .as_ref()
        .ok_or_else(|| eyre::eyre!("status persistence request should include all switches"))?
        .nodes;

    assert_eq!(fabric_manager_status_nodes.len(), switch_ids.len());

    assert!(switch_ids.iter().all(|switch_id| {
        fabric_manager_status_nodes
            .iter()
            .any(|node| node.node_id == switch_id.to_string())
    }));

    let mut txn = pool.acquire().await?;

    let primary_switch = db_switch::find_by_id(&mut txn, &primary_switch_id)
        .await?
        .expect("primary switch should exist");

    let secondary_switch = db_switch::find_by_id(&mut txn, &secondary_switch_id)
        .await?
        .expect("secondary switch should exist");

    assert!(primary_switch.is_primary);
    assert!(!secondary_switch.is_primary);

    let expected_fabric_manager_status = FabricManagerStatus {
        fabric_manager_state: FabricManagerState::Ok,
        addition_info: Some(CONTROL_PLANE_STATE_CONFIGURED.to_string()),
        reason: None,
        error_message: None,
    };

    assert_eq!(
        primary_switch.fabric_manager_status.as_ref(),
        Some(&expected_fabric_manager_status)
    );

    assert_eq!(
        secondary_switch.fabric_manager_status.as_ref(),
        Some(&expected_fabric_manager_status)
    );

    Ok(())
}

/// Runs the `ConfigureNmxCluster` workflow to completion under the given
/// `scale_up_fabric_manager_api_version` and asserts its RMS requests and
/// persisted results. `v1` and `v2` must behave identically, since the config
/// value does not select a workflow.
async fn run_configure_nmx_cluster_parity_case(
    pool: sqlx::PgPool,
    api_version: ScaleUpFabricManagerApiVersion,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = config_with_nmx_cluster_profile();
    config.rms.scale_up_fabric_manager_api_version = api_version;

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;

    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("NmxCluster")),
        &RackConfig::default(),
        None,
    )
    .await?;

    drop(txn);

    let switch_ids = attach_switches_with_nvos_credentials(&env, &rack_id, 2).await?;

    let [secondary_switch_id, primary_switch_id] = switch_ids.as_slice() else {
        return Err(eyre::eyre!("expected exactly two switch fixtures").into());
    };

    let secondary_switch_id = *secondary_switch_id;
    let primary_switch_id = *primary_switch_id;
    let topology_type = RackHardwareTopology::Gb200Nvl72r1C2g4Topology.to_string();

    let rack_config = RackConfig {
        maintenance_requested: Some(MaintenanceScope {
            switch_ids: vec![primary_switch_id],
            activities: vec![MaintenanceActivity::ConfigureNmxCluster],
            ..Default::default()
        }),
        ..Default::default()
    };

    let mut txn = pool.acquire().await?;
    db_rack::update(&mut txn, &rack_id, &rack_config).await?;
    db_switch::set_primary_switch_for_rack(&mut txn, &rack_id, &secondary_switch_id).await?;
    drop(txn);

    queue_configure_nmx_cluster_v2_success(
        &env,
        &switch_ids,
        secondary_switch_id,
        primary_switch_id,
        &topology_type,
    )
    .await;

    run_configure_nmx_cluster_v2_workflow(&env, &rack_id).await?;

    assert_configure_nmx_cluster_v2_results(
        &env,
        &pool,
        &switch_ids,
        secondary_switch_id,
        primary_switch_id,
        &topology_type,
    )
    .await
}

#[crate::sqlx_test]
async fn test_configure_nmx_cluster_v2_delegates_primary_setup_and_persists_observed_state(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    run_configure_nmx_cluster_parity_case(pool, ScaleUpFabricManagerApiVersion::V2).await
}

#[crate::sqlx_test]
async fn test_configure_nmx_cluster_v1_config_still_runs_v2_workflow(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    run_configure_nmx_cluster_parity_case(pool, ScaleUpFabricManagerApiVersion::V1).await
}

#[crate::sqlx_test]
async fn test_configure_nmx_cluster_v2_completed_job_with_unknown_profile_stops_flow(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = config_with_nmx_cluster_profile();
    config.rms.scale_up_fabric_manager_api_version = ScaleUpFabricManagerApiVersion::V2;

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;

    let rack_id = new_rack_id();
    let missing_profile_id = RackProfileId::new("RemovedNmxCluster");
    let mut txn = pool.acquire().await?;

    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&missing_profile_id),
        &RackConfig::default(),
        None,
    )
    .await?;

    drop(txn);

    attach_switches_with_nvos_credentials(&env, &rack_id, 2).await?;

    env.rms_sim
        .queue_get_job_status_response(Ok(rms::GetJobStatusResponse {
            job_states: vec![rms::JobStatus {
                job_id: "configure-scale-up-fabric-job".to_string(),
                execution_state: rms::JobExecutionState::Completed as i32,
                state_description: "completed".to_string(),
                ..Default::default()
            }],
        }))
        .await;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let handler_instance = RackStateHandler::default();

    let mut services = env.rack_state_handler_services();
    services.rms_client = None;
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();

    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let job_wait = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::ConfigureNmxCluster {
            configure_nmx_cluster: ConfigureNmxClusterState::WaitForScaleUpFabricManagerJob {
                job_id: "configure-scale-up-fabric-job".to_string(),
            },
        },
    };

    let outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &job_wait, &mut ctx)
        .await?;

    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => match next_state {
            RackState::Error { cause } => {
                assert!(cause.contains("rack profile is missing or unknown"));
            }
            other => panic!("Expected Error state, got {other:?}"),
        },
        other => panic!(
            "Expected Transition, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    assert!(
        env.rms_sim
            .submitted_get_scale_up_fabric_status_requests()
            .await
            .is_empty()
    );

    assert!(
        env.rms_sim
            .submitted_batch_get_scale_up_fabric_service_status_requests()
            .await
            .is_empty()
    );

    Ok(())
}

/// Verifies that a rack persisted in a `ConfigureNmxCluster` sub-state this
/// version does not run transitions to `Error` rather than resuming, and that
/// `maintenance_requested` is cleared so the rack does not loop back into
/// maintenance.
#[crate::sqlx_test]
async fn test_configure_nmx_cluster_retired_sub_state_transitions_to_error(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;

    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Empty")),
        &RackConfig {
            maintenance_requested: Some(MaintenanceScope::default()),
            ..Default::default()
        },
        None,
    )
    .await?;

    drop(txn);

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler_instance = RackStateHandler::default();

    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();

    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let retired_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::ConfigureNmxCluster {
            configure_nmx_cluster: ConfigureNmxClusterState::WaitForFabricStatus,
        },
    };

    let outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &retired_state, &mut ctx)
        .await?;

    let StateHandlerOutcome::Transition { next_state, .. } = outcome else {
        panic!("expected a transition out of a retired sub-state");
    };

    let RackState::Error { cause } = &next_state else {
        panic!("expected Error, got {next_state:?}");
    };

    assert!(
        cause.contains("WaitForFabricStatus"),
        "cause should name the retired sub-state, got {cause}"
    );

    assert!(
        rack.config.maintenance_requested.is_none(),
        "maintenance_requested should be cleared so the rack does not re-enter maintenance"
    );

    Ok(())
}

/// test_configure_nmx_cluster_transitions_to_completed verifies that
/// Maintenance::ConfigureNmxCluster transitions to Maintenance::Completed.
#[crate::sqlx_test]
async fn test_configure_nmx_cluster_transitions_to_completed(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Empty")),
        &RackConfig::default(),
        None,
    )
    .await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler_instance = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let nmx_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::PowerSequence {
            rack_power: RackPowerState::PoweringOn,
        },
    };
    let outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &nmx_state, &mut ctx)
        .await?;

    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => {
            assert!(
                matches!(
                    next_state,
                    RackState::Maintenance {
                        maintenance_state: RackMaintenanceState::Completed,
                    }
                ),
                "ConfigureNmxCluster should transition to Completed, got {:?}",
                next_state
            );
        }
        other => panic!(
            "Expected Transition, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    Ok(())
}

/// test_ready_topology_changed_transitions_to_discovering verifies that
/// Ready with topology_changed=true transitions back to Discovering.
#[crate::sqlx_test]
async fn test_ready_topology_changed_transitions_to_discovering(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Empty")),
        &RackConfig::default(),
        None,
    )
    .await?;

    let cfg = RackConfig {
        topology_changed: true,
        ..Default::default()
    };
    db_rack::update(&mut txn, &rack_id, &cfg).await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler_instance = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &RackState::Ready, &mut ctx)
        .await?;

    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => {
            assert!(
                matches!(next_state, RackState::Discovering),
                "Ready with topology_changed should transition to Discovering, got {:?}",
                next_state
            );
        }
        other => panic!(
            "Expected Transition to Discovering, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    Ok(())
}

/// test_ready_reprovision_requested_transitions_to_maintenance verifies that
/// Ready with reprovision_requested=true transitions back to Maintenance.
#[crate::sqlx_test]
async fn test_ready_reprovision_requested_transitions_to_maintenance(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Empty")),
        &RackConfig::default(),
        None,
    )
    .await?;

    let cfg = RackConfig {
        reprovision_requested: true,
        ..Default::default()
    };
    db_rack::update(&mut txn, &rack_id, &cfg).await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler_instance = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &RackState::Ready, &mut ctx)
        .await?;

    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => {
            assert!(
                matches!(next_state, RackState::Maintenance { .. }),
                "Ready with reprovision_requested should transition to Maintenance, got {:?}",
                next_state
            );
        }
        other => panic!(
            "Expected Transition to Maintenance, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    Ok(())
}

/// test_validation_failed_transitions_to_error verifies that
/// Validation(Failed) transitions to Error state.
#[crate::sqlx_test]
async fn test_validation_failed_transitions_to_error(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;

    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Empty")),
        &RackConfig::default(),
        None,
    )
    .await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler_instance = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let failed_state = RackState::Validating {
        validating_state: RackValidationState::Failed {
            run_id: "test-run".to_string(),
        },
    };
    let outcome = handler_instance
        .handle_object_state(&rack_id, &mut rack, &failed_state, &mut ctx)
        .await?;

    assert!(
        matches!(outcome, StateHandlerOutcome::DoNothing { .. }),
        "Validation(Failed) should wait for intervention, got {:?}",
        std::mem::discriminant(&outcome)
    );

    Ok(())
}

async fn set_switch_state(
    txn: &mut sqlx::PgConnection,
    switch_id: &SwitchId,
    state: model::switch::SwitchControllerState,
) {
    sqlx::query("UPDATE switches SET controller_state = $1 WHERE id = $2")
        .bind(serde_json::to_value(state).unwrap())
        .bind(switch_id)
        .execute(txn)
        .await
        .expect("update switch controller_state");
}

async fn set_power_shelf_state(
    txn: &mut sqlx::PgConnection,
    power_shelf_id: &carbide_uuid::power_shelf::PowerShelfId,
    state: model::power_shelf::PowerShelfControllerState,
) {
    sqlx::query("UPDATE power_shelves SET controller_state = $1 WHERE id = $2")
        .bind(serde_json::to_value(state).unwrap())
        .bind(power_shelf_id)
        .execute(txn)
        .await
        .expect("update power_shelf controller_state");
}

async fn create_test_power_shelf(
    txn: &mut sqlx::PgConnection,
    rack_id: &RackId,
    seed: u8,
) -> carbide_uuid::power_shelf::PowerShelfId {
    use carbide_uuid::power_shelf::PowerShelfId;
    let mut bytes = [0u8; 16];
    bytes[0] = seed;
    let power_shelf_id = PowerShelfId::from(uuid::Uuid::from_bytes(bytes));
    let new_power_shelf = model::power_shelf::NewPowerShelf {
        id: power_shelf_id,
        config: model::power_shelf::PowerShelfConfig {
            name: format!("ps-{}", seed),
            capacity: Some(6000),
            voltage: Some(480),
        },
        bmc_mac_address: None,
        metadata: None,
        rack_id: Some(rack_id.clone()),
    };
    db::power_shelf::create(txn, &new_power_shelf)
        .await
        .expect("create power shelf");
    power_shelf_id
}

/// Ready rack moves to Error when one of its switches enters Error.
#[crate::sqlx_test]
async fn test_ready_with_failed_switch_transitions_to_error(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;

    set_switch_state(
        pool.acquire().await?.as_mut(),
        &switch_id,
        model::switch::SwitchControllerState::Error {
            cause: "synthetic switch failure".to_string(),
        },
    )
    .await;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &RackState::Ready, &mut ctx)
        .await?;

    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => match next_state {
            RackState::Error { cause } => {
                assert!(
                    cause.contains("switch"),
                    "Error cause should mention failing switch, got: {}",
                    cause
                );
            }
            other => panic!("Expected Transition to Error, got {:?}", other),
        },
        other => panic!(
            "Expected Transition to Error, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    Ok(())
}

/// Ready rack moves to Error when an attached power shelf enters Error.
#[crate::sqlx_test]
async fn test_ready_with_failed_power_shelf_transitions_to_error(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;

    let rack_id = new_rack_id();
    {
        let mut txn = pool.begin().await?;
        db_rack::create(
            txn.as_mut(),
            &rack_id,
            Some(&RackProfileId::new("Empty")),
            &RackConfig::default(),
            None,
        )
        .await?;
        let power_shelf_id = create_test_power_shelf(txn.as_mut(), &rack_id, 0xa1).await;
        set_power_shelf_state(
            txn.as_mut(),
            &power_shelf_id,
            model::power_shelf::PowerShelfControllerState::Error {
                cause: "synthetic power shelf failure".to_string(),
            },
        )
        .await;
        let rack = get_db_rack(txn.as_mut(), &rack_id).await;
        db_rack::try_update_controller_state(
            txn.as_mut(),
            &rack_id,
            rack.controller_state.version,
            rack.controller_state.version.increment(),
            &RackState::Ready,
        )
        .await?;
        txn.commit().await?;
    }

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &RackState::Ready, &mut ctx)
        .await?;

    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => match next_state {
            RackState::Error { cause } => {
                assert!(
                    cause.contains("power shelf"),
                    "Error cause should mention failing power shelf, got: {}",
                    cause
                );
            }
            other => panic!("Expected Transition to Error, got {:?}", other),
        },
        other => panic!(
            "Expected Transition to Error, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    Ok(())
}

/// Ready rack with all healthy components stays in Ready.
#[crate::sqlx_test]
async fn test_ready_with_all_healthy_components_waits(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;

    set_switch_state(
        pool.acquire().await?.as_mut(),
        &switch_id,
        model::switch::SwitchControllerState::Ready,
    )
    .await;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &RackState::Ready, &mut ctx)
        .await?;

    assert!(
        matches!(
            outcome,
            StateHandlerOutcome::Wait { .. } | StateHandlerOutcome::DoNothing { .. }
        ),
        "Ready with healthy components should wait, got {:?}",
        std::mem::discriminant(&outcome)
    );

    Ok(())
}

/// Rack in Error transitions back to Ready once every component is Ready.
#[crate::sqlx_test]
async fn test_error_recovers_to_ready_when_all_components_ready(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;

    set_switch_state(
        pool.acquire().await?.as_mut(),
        &switch_id,
        model::switch::SwitchControllerState::Ready,
    )
    .await;
    crate::tests::rack_state_controller::fixtures::rack::set_rack_controller_state(
        pool.acquire().await?.as_mut(),
        &rack_id,
        RackState::Error {
            cause: "synthetic prior failure".to_string(),
        },
    )
    .await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let error_state = rack.controller_state.value.clone();

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &error_state, &mut ctx)
        .await?;

    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => {
            assert!(
                matches!(next_state, RackState::Ready),
                "Error with all-Ready components should transition to Ready, got {:?}",
                next_state
            );
        }
        other => panic!(
            "Expected Transition to Ready, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    Ok(())
}

/// Rack in Error does not auto-recover while any component is not yet Ready.
#[crate::sqlx_test]
async fn test_error_stays_in_error_when_components_not_all_ready(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool.clone(), TestEnvOverrides::default()).await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;

    set_switch_state(
        pool.acquire().await?.as_mut(),
        &switch_id,
        model::switch::SwitchControllerState::Initializing {
            initializing_state: model::switch::InitializingState::WaitForOsMachineInterface,
        },
    )
    .await;
    crate::tests::rack_state_controller::fixtures::rack::set_rack_controller_state(
        pool.acquire().await?.as_mut(),
        &rack_id,
        RackState::Error {
            cause: "synthetic prior failure".to_string(),
        },
    )
    .await?;

    let mut rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let error_state = rack.controller_state.value.clone();

    let handler = RackStateHandler::default();
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut db_writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut db_writes,
    };

    let outcome = handler
        .handle_object_state(&rack_id, &mut rack, &error_state, &mut ctx)
        .await?;

    assert!(
        matches!(outcome, StateHandlerOutcome::Wait { .. }),
        "Error with non-Ready components must not auto-recover, got {:?}",
        std::mem::discriminant(&outcome)
    );

    Ok(())
}

async fn get_db_rack<DB>(conn: &mut DB, rack_id: &RackId) -> Rack
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    db_rack::find_by(conn, ObjectColumnFilter::One(db_rack::IdColumn, rack_id))
        .await
        .unwrap()
        .pop()
        .unwrap()
}
