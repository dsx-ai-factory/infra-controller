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
use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, Credentials};
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
use model::address_selection_strategy::AddressSelectionStrategy;
use model::component_manager::{ComputeTrayComponent, PowerAction};
use model::expected_machine::ExpectedMachine;
use model::expected_power_shelf::ExpectedPowerShelf;
use model::expected_rack::ExpectedRack;
use model::expected_switch::ExpectedSwitch;
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
    non_rms: Mutex<bool>,
}

#[async_trait]
impl NvSwitchManager for RecordingNvSwitchManager {
    fn name(&self) -> &str {
        "recording-nv-switch-manager"
    }

    fn supports_firmware_object_json(&self) -> bool {
        !*self.non_rms.lock().unwrap()
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
                access_token: Some("test-token".to_owned()),
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

    assert_eq!(
        options
            .iter()
            .map(|options| options.access_token.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("test-token"), Some("test-token")],
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
    let switch_bmc_mac = "02:00:00:00:00:01".parse::<MacAddress>()?;
    let power_shelf_bmc_mac = "02:00:00:00:00:02".parse::<MacAddress>()?;

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

    db::expected_switch::create(
        txn.as_mut(),
        ExpectedSwitch {
            bmc_mac_address: switch_bmc_mac,
            serial_number: "rack-switch".to_owned(),
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        },
    )
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
            bmc_mac_address: Some(switch_bmc_mac),
            metadata: None,
            rack_id: Some(rack_id.clone()),
            slot_number: Some(0),
            tray_index: Some(0),
        },
    )
    .await?;

    db::expected_power_shelf::create(
        txn.as_mut(),
        ExpectedPowerShelf {
            bmc_mac_address: power_shelf_bmc_mac,
            serial_number: "rack-power-shelf".to_owned(),
            rack_id: Some(rack_id.clone()),
            ..Default::default()
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
            bmc_mac_address: Some(power_shelf_bmc_mac),
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
async fn empty_version_skips_current_non_rms_switches(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (fixture, [_, switch_mac, _]) = create_pre_ingestion_firmware_fixture(pool.clone()).await?;

    let manager = &fixture.nv_switch_manager;
    *manager.non_rms.lock().unwrap() = true;

    let desired = crate::handlers::firmware::get_desired_firmware_versions(
        &fixture.env.api,
        Request::new(rpc::GetDesiredFirmwareVersionsRequest {}),
    )
    .await?
    .into_inner()
    .entries
    .into_iter()
    .find(|entry| !entry.component_versions.is_empty())
    .unwrap();

    let mut report = model::site_explorer::EndpointExplorationReport {
        versions: desired
            .component_versions
            .into_iter()
            .map(|(key, version)| Ok((serde_json::from_value(key.into())?, version)))
            .collect::<Result<_, serde_json::Error>>()?,
        ..Default::default()
    };

    let mut txn = pool.begin().await?;

    let address = db::machine_interface::lookup_bmc_ip_by_mac_address(&mut *txn, switch_mac)
        .await?
        .into_iter()
        .next()
        .unwrap();

    db::explored_endpoints::insert(address, &report, false, txn.as_mut()).await?;
    sqlx::query("UPDATE switches SET bmc_mac_address = $1 WHERE id = $2")
        .bind(switch_mac)
        .bind(fixture.switch_id)
        .execute(txn.as_mut())
        .await?;

    txn.commit().await?;

    let id_request = switch_request(fixture.switch_id, true).into_inner();

    let mac_request = rpc::UpdateComponentFirmwareRequest {
        bypass_state_controller: true,
        target: Some(rpc::update_component_firmware_request::Target::Switches(
            rpc::UpdateSwitchFirmwareTarget {
                switch_ids: None,
                bmc_macs: Some(rpc::MacAddressList {
                    mac_addresses: vec![switch_mac.to_string()],
                }),
                components: vec![],
            },
        )),
        ..Default::default()
    };

    for request in [id_request.clone(), mac_request] {
        let response = crate::handlers::component_manager::update_component_firmware(
            &fixture.env.api,
            Request::new(request),
        )
        .await?
        .into_inner();

        assert_eq!(response.results.len(), 1);

        assert_eq!(
            response.results[0].status,
            rpc::ComponentManagerStatusCode::Success as i32
        );
    }

    assert!(manager.target_versions.lock().unwrap().is_empty());
    assert!(fixture.fetcher.requested_urls.lock().unwrap().is_empty());

    *manager.non_rms.lock().unwrap() = false;
    *fixture.fetcher.response.lock().unwrap() = Err("firmware source unavailable".to_owned());

    let error = crate::handlers::component_manager::update_component_firmware(
        &fixture.env.api,
        Request::new(id_request.clone()),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), tonic::Code::Unavailable);
    assert!(manager.target_versions.lock().unwrap().is_empty());

    *manager.non_rms.lock().unwrap() = true;

    report
        .versions
        .values_mut()
        .for_each(|version| *version = "outdated".to_owned());

    sqlx::query("UPDATE explored_endpoints SET exploration_report = $1 WHERE address = $2")
        .bind(sqlx::types::Json(&report))
        .bind(address)
        .execute(&pool)
        .await?;

    crate::handlers::component_manager::update_component_firmware(
        &fixture.env.api,
        Request::new(id_request),
    )
    .await?;

    assert_eq!(manager.target_versions.lock().unwrap().as_slice(), [""]);

    Ok(())
}

async fn create_pre_ingestion_firmware_fixture(
    pool: sqlx::PgPool,
) -> Result<(FirmwareObjectFixture, [MacAddress; 3]), Box<dyn std::error::Error>> {
    let fixture = create_firmware_object_fixture(pool.clone()).await?;
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let compute_mac: MacAddress = "02:00:00:00:00:11".parse()?;
    let switch_mac: MacAddress = "02:00:00:00:00:12".parse()?;
    let shelf_mac: MacAddress = "02:00:00:00:00:13".parse()?;
    let nvos_mac: MacAddress = "02:00:00:00:00:14".parse()?;
    let mut txn = pool.begin().await?;
    let underlay = db::network_segment::find_by_name(txn.as_mut(), "UNDERLAY").await?;

    db::expected_rack::create(
        txn.as_mut(),
        &ExpectedRack {
            rack_id: rack_id.clone(),
            rack_profile_id: RackProfileId::new("NVL72"),
            ..Default::default()
        },
    )
    .await?;

    for mac in [compute_mac, switch_mac, shelf_mac, nvos_mac] {
        db::machine_interface::create(
            txn.as_mut(),
            std::slice::from_ref(&underlay),
            &mac,
            false,
            AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await?;
    }

    db::expected_machine::create(
        txn.as_mut(),
        ExpectedMachine {
            id: None,
            bmc_mac_address: compute_mac,
            data: rpc::ExpectedMachine {
                chassis_serial_number: "pre-ingestion-compute".to_owned(),
                rack_id: Some(rack_id.clone()),
                ..Default::default()
            }
            .try_into()?,
        },
    )
    .await?;

    db::expected_switch::create(
        txn.as_mut(),
        ExpectedSwitch {
            bmc_mac_address: switch_mac,
            nvos_mac_addresses: vec![nvos_mac],
            serial_number: "pre-ingestion-switch".to_owned(),
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        },
    )
    .await?;

    db::expected_power_shelf::create(
        txn.as_mut(),
        ExpectedPowerShelf {
            bmc_mac_address: shelf_mac,
            serial_number: "pre-ingestion-power-shelf".to_owned(),
            rack_id: Some(rack_id),
            ..Default::default()
        },
    )
    .await?;

    txn.commit().await?;

    let bmc_keys = [compute_mac, switch_mac, shelf_mac].map(|mac| CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: mac,
        },
    });

    let nvos_key = CredentialKey::SwitchNvosAdmin {
        bmc_mac_address: switch_mac,
    };

    let credentials = Credentials::UsernamePassword {
        username: "test-user".to_owned(),
        password: "test-password".to_owned(),
    };

    for key in bmc_keys.into_iter().chain([nvos_key]) {
        fixture
            .env
            .api
            .credential_manager
            .set_credentials(&key, &credentials)
            .await
            .map_err(|error| error.to_string())?;
    }

    Ok((fixture, [compute_mac, switch_mac, shelf_mac]))
}

#[crate::sqlx_test]
async fn empty_version_resolves_profile_for_pre_ingestion_macs(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (
        FirmwareObjectFixture {
            env,
            compute_tray_manager,
            nv_switch_manager,
            power_shelf_manager,
            switch_id,
            power_shelf_id,
            machine_id,
            ..
        },
        [compute_mac, switch_mac, shelf_mac],
    ) = create_pre_ingestion_firmware_fixture(pool).await?;

    for (mut request, mac) in [
        (compute_request(machine_id, false).into_inner(), compute_mac),
        (switch_request(switch_id, false).into_inner(), switch_mac),
        (
            power_shelf_request(power_shelf_id, false).into_inner(),
            shelf_mac,
        ),
    ] {
        let macs = Some(rpc::MacAddressList {
            mac_addresses: vec![mac.to_string()],
        });

        match request.target.as_mut().unwrap() {
            rpc::update_component_firmware_request::Target::ComputeTrays(target) => {
                target.machine_ids = None;
                target.bmc_macs = macs;
            }
            rpc::update_component_firmware_request::Target::Switches(target) => {
                target.switch_ids = None;
                target.bmc_macs = macs;
            }
            rpc::update_component_firmware_request::Target::PowerShelves(target) => {
                target.power_shelf_ids = None;
                target.pmc_macs = macs;
            }
            rpc::update_component_firmware_request::Target::Racks(_) => unreachable!(),
        }

        let response = crate::handlers::component_manager::update_component_firmware(
            &env.api,
            Request::new(request),
        )
        .await?
        .into_inner();

        assert_eq!(
            response.results,
            vec![rpc::ComponentResult {
                status: rpc::ComponentManagerStatusCode::Success as i32,
                mac_address: Some(mac.to_string()),
                ..Default::default()
            }]
        );
    }

    for versions in [
        compute_tray_manager.target_versions(),
        nv_switch_manager.target_versions.lock().unwrap().clone(),
        power_shelf_manager.target_versions.lock().unwrap().clone(),
    ] {
        assert_eq!(versions, [FIRMWARE_OBJECT]);
    }

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
