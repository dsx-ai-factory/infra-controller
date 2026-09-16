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
use carbide_rack_controller::firmware_object::FirmwareObjectFetcher;
use carbide_uuid::machine::HostMachineId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::{RackId, RackProfileId};
use carbide_uuid::switch::SwitchId;
use component_manager::compute_tray_manager::{
    Backend, ComputeTrayEndpoint, ComputeTrayFirmwareUpdateStatus, ComputeTrayManager,
    ComputeTrayResult,
};
use component_manager::error::ComponentManagerError;
use component_manager::mock::{MockComputeTrayManager, MockNvSwitchManager, MockPowerShelfManager};
use component_manager::nv_switch_manager::{
    ConfigureSwitchCertificateJobStatus, NvSwitchManager, SwitchComponentResult, SwitchEndpoint,
    SwitchFirmwareUpdateStatus, SwitchPowerStateResult, SwitchSlotAndTrayResult,
};
use component_manager::power_shelf_manager::{
    PowerShelfComponentResult, PowerShelfEndpoint, PowerShelfFirmwareUpdateStatus,
    PowerShelfFirmwareVersions, PowerShelfManager, PowerShelfPowerStateResult,
};
use component_manager::types::FirmwareUpdateOptions;
use mac_address::MacAddress;
use model::component_manager::{ComputeTrayComponent, PowerAction};
use model::power_shelf::{NewPowerShelf, PowerShelfConfig};
use model::rack::{MaintenanceActivity, RackConfig, RackState};
use model::rack_type::RackFirmwareObjectConfig;
use model::switch::{NewSwitch, SwitchConfig};
use model::test_support::HardwareInfoTemplate;
use rpc::forge as rpc;
use tonic::Request;

use crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_managed_host_with_hardware_info_template,
    create_test_env_with_overrides, get_config_with_rack_profiles,
};

#[derive(Debug, Default)]
struct RecordingComputeTrayManager {
    backend: Backend,
    inner: MockComputeTrayManager,
    firmware_update_options: Mutex<Vec<FirmwareUpdateOptions>>,
    target_versions: Mutex<Vec<String>>,
}

impl RecordingComputeTrayManager {
    fn clear_firmware_update_options(&self) {
        self.firmware_update_options.lock().unwrap().clear();
    }

    fn firmware_update_options(&self) -> Vec<FirmwareUpdateOptions> {
        self.firmware_update_options.lock().unwrap().clone()
    }

    fn target_versions(&self) -> Vec<String> {
        self.target_versions.lock().unwrap().clone()
    }
}

#[async_trait]
impl ComputeTrayManager for RecordingComputeTrayManager {
    fn name(&self) -> &str {
        "recording-compute-tray-manager"
    }

    fn backend(&self) -> Backend {
        self.backend
    }

    async fn power_control(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        action: PowerAction,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        self.inner.power_control(endpoints, action).await
    }

    async fn update_firmware(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        target_version: &str,
        components: &[ComputeTrayComponent],
        options: &FirmwareUpdateOptions,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        self.firmware_update_options
            .lock()
            .unwrap()
            .push(options.clone());

        self.target_versions
            .lock()
            .unwrap()
            .push(target_version.to_owned());

        self.inner
            .update_firmware(endpoints, target_version, components, options)
            .await
    }

    async fn get_firmware_status(
        &self,
        endpoints: &[ComputeTrayEndpoint],
    ) -> Result<Vec<ComputeTrayFirmwareUpdateStatus>, ComponentManagerError> {
        self.inner.get_firmware_status(endpoints).await
    }

    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError> {
        self.inner.list_firmware_bundles().await
    }
}

#[derive(Debug, Default)]
struct RecordingNvSwitchManager {
    inner: MockNvSwitchManager,
    target_versions: Mutex<Vec<String>>,
}

#[async_trait]
impl NvSwitchManager for RecordingNvSwitchManager {
    fn name(&self) -> &str {
        "recording-nv-switch-manager"
    }

    fn supports_firmware_object_json(&self) -> bool {
        true
    }

    async fn power_control(
        &self,
        endpoints: &[SwitchEndpoint],
        action: PowerAction,
    ) -> Result<Vec<SwitchComponentResult>, ComponentManagerError> {
        self.inner.power_control(endpoints, action).await
    }

    async fn queue_firmware_updates(
        &self,
        endpoints: &[SwitchEndpoint],
        target_version: &str,
        components: &[model::component_manager::NvSwitchComponent],
        options: &FirmwareUpdateOptions,
    ) -> Result<Vec<SwitchComponentResult>, ComponentManagerError> {
        self.target_versions
            .lock()
            .unwrap()
            .push(target_version.to_owned());

        self.inner
            .queue_firmware_updates(endpoints, target_version, components, options)
            .await
    }

    async fn get_firmware_status(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchFirmwareUpdateStatus>, ComponentManagerError> {
        self.inner.get_firmware_status(endpoints).await
    }

    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError> {
        self.inner.list_firmware_bundles().await
    }

    async fn get_slot_and_tray(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchSlotAndTrayResult>, ComponentManagerError> {
        self.inner.get_slot_and_tray(endpoints).await
    }

    async fn get_power_state(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchPowerStateResult>, ComponentManagerError> {
        self.inner.get_power_state(endpoints).await
    }

    async fn configure_switch_certificate(
        &self,
        endpoint: &SwitchEndpoint,
        domain_name: Option<&str>,
        services: Option<&[i32]>,
    ) -> Result<String, ComponentManagerError> {
        self.inner
            .configure_switch_certificate(endpoint, domain_name, services)
            .await
    }

    async fn get_configure_switch_certificate_job_status(
        &self,
        job_id: &str,
    ) -> Result<ConfigureSwitchCertificateJobStatus, ComponentManagerError> {
        self.inner
            .get_configure_switch_certificate_job_status(job_id)
            .await
    }
}

#[derive(Debug, Default)]
struct RecordingPowerShelfManager {
    inner: MockPowerShelfManager,
    target_versions: Mutex<Vec<String>>,
}

#[async_trait]
impl PowerShelfManager for RecordingPowerShelfManager {
    fn name(&self) -> &str {
        "recording-power-shelf-manager"
    }

    fn supports_firmware_object_json(&self) -> bool {
        true
    }

    async fn power_control(
        &self,
        endpoints: &[PowerShelfEndpoint],
        action: PowerAction,
    ) -> Result<Vec<PowerShelfComponentResult>, ComponentManagerError> {
        self.inner.power_control(endpoints, action).await
    }

    async fn update_firmware(
        &self,
        endpoints: &[PowerShelfEndpoint],
        target_version: &str,
        components: &[model::component_manager::PowerShelfComponent],
        options: &FirmwareUpdateOptions,
    ) -> Result<Vec<PowerShelfComponentResult>, ComponentManagerError> {
        self.target_versions
            .lock()
            .unwrap()
            .push(target_version.to_owned());

        self.inner
            .update_firmware(endpoints, target_version, components, options)
            .await
    }

    async fn get_firmware_status(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfFirmwareUpdateStatus>, ComponentManagerError> {
        self.inner.get_firmware_status(endpoints).await
    }

    async fn list_firmware(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfFirmwareVersions>, ComponentManagerError> {
        self.inner.list_firmware(endpoints).await
    }

    async fn get_power_state(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfPowerStateResult>, ComponentManagerError> {
        self.inner.get_power_state(endpoints).await
    }
}

#[derive(Debug)]
struct StaticFirmwareObjectFetcher {
    response: Mutex<Result<String, String>>,
    requested_urls: Mutex<Vec<String>>,
}

#[async_trait]
impl FirmwareObjectFetcher for StaticFirmwareObjectFetcher {
    async fn fetch(&self, url: &str, _timeout: std::time::Duration) -> Result<String, String> {
        self.requested_urls.lock().unwrap().push(url.to_owned());
        self.response.lock().unwrap().clone()
    }
}

#[crate::sqlx_test]
async fn compute_tray_direct_dispatch_forwards_force_update(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let compute_tray_manager = Arc::new(RecordingComputeTrayManager::default());
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            compute_tray_manager: Some(compute_tray_manager.clone()),
            ..Default::default()
        },
    )
    .await;
    let managed_host = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(GB200_COMPUTE_TRAY_1_INFO_JSON),
    )
    .await;
    compute_tray_manager.clear_firmware_update_options();

    for force_update in [false, true] {
        let response = crate::handlers::component_manager::update_component_firmware(
            &env.api,
            Request::new(rpc::UpdateComponentFirmwareRequest {
                target_version: r#"{"Id":"test-firmware"}"#.to_string(),
                access_token: None,
                force_update,
                bypass_state_controller: true,
                target: Some(
                    rpc::update_component_firmware_request::Target::ComputeTrays(
                        rpc::UpdateComputeTrayFirmwareTarget {
                            machine_ids: Some(::rpc::common::HostMachineIdList {
                                machine_ids: vec![managed_host.id.into()],
                            }),
                            bmc_macs: None,
                            components: vec![],
                        },
                    ),
                ),
            }),
        )
        .await?
        .into_inner();

        assert_eq!(response.results.len(), 1);
        assert_eq!(
            response.results[0].status,
            rpc::ComponentManagerStatusCode::Success as i32,
        );
    }

    let options = compute_tray_manager.firmware_update_options();
    assert_eq!(
        options
            .iter()
            .map(|options| options.force_update)
            .collect::<Vec<_>>(),
        vec![false, true],
    );
    assert!(
        options.iter().all(|options| options.access_token.is_none()),
        "the force-update fix must not change access-token handling",
    );

    assert_eq!(
        compute_tray_manager.target_versions(),
        vec![r#"{"Id":"test-firmware"}"#, r#"{"Id":"test-firmware"}"#,]
    );

    Ok(())
}

const FIRMWARE_OBJECT: &str = r#"{"Id":"desired-rack-firmware"}"#;
const FIRMWARE_OBJECT_URL: &str = "https://firmware.example.test/rack.json";

struct FirmwareObjectFixture {
    env: TestEnv,
    fetcher: Arc<StaticFirmwareObjectFetcher>,
    compute_tray_manager: Arc<RecordingComputeTrayManager>,
    nv_switch_manager: Arc<RecordingNvSwitchManager>,
    power_shelf_manager: Arc<RecordingPowerShelfManager>,
    rack_id: RackId,
    switch_id: SwitchId,
    power_shelf_id: PowerShelfId,
    machine_id: HostMachineId,
}

async fn create_firmware_object_fixture(
    pool: sqlx::PgPool,
) -> Result<FirmwareObjectFixture, Box<dyn std::error::Error>> {
    let fetcher = Arc::new(StaticFirmwareObjectFetcher {
        response: Mutex::new(Ok(FIRMWARE_OBJECT.to_owned())),
        requested_urls: Mutex::new(Vec::new()),
    });

    let compute_tray_manager = Arc::new(RecordingComputeTrayManager::default());
    let nv_switch_manager = Arc::new(RecordingNvSwitchManager::default());
    let power_shelf_manager = Arc::new(RecordingPowerShelfManager::default());
    let mut config = get_config_with_rack_profiles();
    let profile_without_source = config.rack_profiles.rack_profiles["NVL72"].clone();

    config
        .rack_profiles
        .rack_profiles
        .insert("NVL72_NO_SOURCE".to_owned(), profile_without_source);

    let profile = config.rack_profiles.rack_profiles.get_mut("NVL72").unwrap();

    profile.firmware_object = Some(RackFirmwareObjectConfig {
        url: FIRMWARE_OBJECT_URL.parse()?,
        access_token_credential: None,
        fetch_timeout: std::time::Duration::from_secs(7),
    });

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config),
            compute_tray_manager: Some(compute_tray_manager.clone()),
            compute_tray_use_state_controller: Some(true),
            nv_switch_manager: Some(nv_switch_manager.clone()),
            power_shelf_manager: Some(power_shelf_manager.clone()),
            firmware_object_fetcher: Some(fetcher.clone()),
            ..Default::default()
        },
    )
    .await;

    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let switch_id = SwitchId::from(uuid::Uuid::new_v4());
    let power_shelf_id = PowerShelfId::from(uuid::Uuid::new_v4());

    let mut txn = pool.begin().await?;

    db::rack::create(
        txn.as_mut(),
        &rack_id,
        Some(&RackProfileId::new("NVL72")),
        &RackConfig::default(),
        None,
    )
    .await?;

    sqlx::query("UPDATE racks SET controller_state = $1 WHERE id = $2")
        .bind(serde_json::to_value(RackState::Ready)?)
        .bind(&rack_id)
        .execute(txn.as_mut())
        .await?;

    db::switch::create(
        txn.as_mut(),
        &NewSwitch {
            id: switch_id,
            config: SwitchConfig {
                name: "rack-switch".to_owned(),
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            bmc_mac_address: Some("02:00:00:00:00:01".parse::<MacAddress>()?),
            metadata: None,
            rack_id: Some(rack_id.clone()),
            slot_number: Some(0),
            tray_index: Some(0),
        },
    )
    .await?;

    db::power_shelf::create(
        txn.as_mut(),
        &NewPowerShelf {
            id: power_shelf_id,
            config: PowerShelfConfig {
                name: "rack-power-shelf".to_owned(),
                capacity: None,
                voltage: None,
            },
            bmc_mac_address: Some("02:00:00:00:00:02".parse::<MacAddress>()?),
            metadata: None,
            rack_id: Some(rack_id.clone()),
        },
    )
    .await?;

    txn.commit().await?;

    let managed_host = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(GB200_COMPUTE_TRAY_1_INFO_JSON),
    )
    .await;

    sqlx::query("UPDATE machines SET rack_id = $1 WHERE id = $2")
        .bind(&rack_id)
        .bind(managed_host.id)
        .execute(&pool)
        .await?;

    Ok(FirmwareObjectFixture {
        env,
        fetcher,
        compute_tray_manager,
        nv_switch_manager,
        power_shelf_manager,
        rack_id,
        switch_id,
        power_shelf_id,
        machine_id: managed_host.id.into(),
    })
}

fn switch_request(
    switch_id: SwitchId,
    bypass: bool,
) -> Request<rpc::UpdateComponentFirmwareRequest> {
    Request::new(rpc::UpdateComponentFirmwareRequest {
        bypass_state_controller: bypass,
        target: Some(rpc::update_component_firmware_request::Target::Switches(
            rpc::UpdateSwitchFirmwareTarget {
                switch_ids: Some(rpc::SwitchIdList {
                    ids: vec![switch_id],
                }),
                bmc_macs: None,
                components: vec![],
            },
        )),
        ..Default::default()
    })
}

fn compute_request(
    machine_id: HostMachineId,
    bypass: bool,
) -> Request<rpc::UpdateComponentFirmwareRequest> {
    Request::new(rpc::UpdateComponentFirmwareRequest {
        bypass_state_controller: bypass,
        target: Some(
            rpc::update_component_firmware_request::Target::ComputeTrays(
                rpc::UpdateComputeTrayFirmwareTarget {
                    machine_ids: Some(::rpc::common::HostMachineIdList {
                        machine_ids: vec![machine_id],
                    }),
                    bmc_macs: None,
                    components: vec![],
                },
            ),
        ),
        ..Default::default()
    })
}

fn power_shelf_request(
    power_shelf_id: PowerShelfId,
    bypass: bool,
) -> Request<rpc::UpdateComponentFirmwareRequest> {
    Request::new(rpc::UpdateComponentFirmwareRequest {
        bypass_state_controller: bypass,
        target: Some(
            rpc::update_component_firmware_request::Target::PowerShelves(
                rpc::UpdatePowerShelfFirmwareTarget {
                    power_shelf_ids: Some(rpc::PowerShelfIdList {
                        ids: vec![power_shelf_id],
                    }),
                    pmc_macs: None,
                    components: vec![],
                },
            ),
        ),
        ..Default::default()
    })
}

async fn assert_rack_activity_version(
    fixture: &FirmwareObjectFixture,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut rack = db::rack::find_by(
        fixture.env.api.db_reader().as_mut(),
        db::ObjectColumnFilter::One(db::rack::IdColumn, &fixture.rack_id),
    )
    .await?
    .pop()
    .unwrap();

    let activity = rack
        .config
        .maintenance_requested
        .take()
        .unwrap()
        .activities
        .into_iter()
        .next()
        .unwrap();

    assert!(matches!(
        activity,
        MaintenanceActivity::FirmwareUpgrade {
            firmware_version: Some(version),
            ..
        } if version == FIRMWARE_OBJECT
    ));

    let mut txn = fixture.env.api.txn_begin().await?;
    db::rack::update(txn.as_mut(), &fixture.rack_id, &rack.config).await?;
    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn empty_version_resolves_profile_for_state_controller_paths(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = create_firmware_object_fixture(pool).await?;
    for request in [
        switch_request(fixture.switch_id, false),
        compute_request(fixture.machine_id, false),
        power_shelf_request(fixture.power_shelf_id, false),
    ] {
        crate::handlers::component_manager::update_component_firmware(&fixture.env.api, request)
            .await?;

        assert_rack_activity_version(&fixture).await?;
    }

    assert_eq!(
        fixture.fetcher.requested_urls.lock().unwrap().as_slice(),
        [FIRMWARE_OBJECT_URL; 3]
    );

    Ok(())
}

#[crate::sqlx_test]
async fn empty_version_resolves_profile_for_direct_rms_paths(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = create_firmware_object_fixture(pool).await?;
    for request in [
        switch_request(fixture.switch_id, true),
        compute_request(fixture.machine_id, true),
        power_shelf_request(fixture.power_shelf_id, true),
    ] {
        crate::handlers::component_manager::update_component_firmware(&fixture.env.api, request)
            .await?;
    }

    assert_eq!(
        fixture.compute_tray_manager.target_versions(),
        [FIRMWARE_OBJECT]
    );

    assert_eq!(
        fixture
            .nv_switch_manager
            .target_versions
            .lock()
            .unwrap()
            .as_slice(),
        [FIRMWARE_OBJECT]
    );

    assert_eq!(
        fixture
            .power_shelf_manager
            .target_versions
            .lock()
            .unwrap()
            .as_slice(),
        [FIRMWARE_OBJECT]
    );

    assert_eq!(
        fixture.fetcher.requested_urls.lock().unwrap().as_slice(),
        [FIRMWARE_OBJECT_URL; 3]
    );

    Ok(())
}

#[crate::sqlx_test]
async fn empty_version_resolution_fails_before_direct_rms_dispatch(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = create_firmware_object_fixture(pool.clone()).await?;
    sqlx::query("UPDATE racks SET rack_profile_id = $1 WHERE id = $2")
        .bind(RackProfileId::new("NVL72_NO_SOURCE"))
        .bind(&fixture.rack_id)
        .execute(&pool)
        .await?;

    let error = crate::handlers::component_manager::update_component_firmware(
        &fixture.env.api,
        compute_request(fixture.machine_id, true),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains("has no firmware_object source"));

    sqlx::query("UPDATE racks SET rack_profile_id = $1 WHERE id = $2")
        .bind(RackProfileId::new("NVL72"))
        .bind(&fixture.rack_id)
        .execute(&pool)
        .await?;

    for (fetch_response, expected_code, expected_message) in [
        (
            Ok(String::new()),
            tonic::Code::FailedPrecondition,
            "firmware_object is unusable",
        ),
        (
            Ok("not-json".to_owned()),
            tonic::Code::FailedPrecondition,
            "firmware_object is unusable",
        ),
        (
            Err("firmware object fetch failed".to_owned()),
            tonic::Code::Unavailable,
            "firmware object fetch failed",
        ),
    ] {
        *fixture.fetcher.response.lock().unwrap() = fetch_response;

        let error = crate::handlers::component_manager::update_component_firmware(
            &fixture.env.api,
            compute_request(fixture.machine_id, true),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code(), expected_code);
        assert!(error.message().contains(expected_message));
    }

    assert!(fixture.compute_tray_manager.target_versions().is_empty());

    Ok(())
}
