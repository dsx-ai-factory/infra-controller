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

use std::sync::Arc;
use std::time::Duration;

use carbide_health_metrics::PerObjectMetricsRegistry;
use carbide_rack::test_support::RmsSim;
use carbide_redfish::libredfish::test_support::RedfishSim;
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_switch_controller::context::SwitchStateHandlerServices;
use carbide_switch_controller::handler::SwitchStateHandler;
use carbide_switch_controller::io::SwitchStateControllerIO;
use carbide_test_harness::{Api, TestHarness, rpc};
use carbide_uuid::machine::MachineInterfaceId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use component_manager::compute_tray_manager::Backend as ComputeBackend;
use component_manager::config::ComponentManagerConfig;
use component_manager::nv_switch_manager::Backend as NvSwitchBackend;
use component_manager::power_shelf_manager::Backend as PowerShelfBackend;
use db::switch as db_switch;
use model::allocation_type::AllocationType;
use model::switch::{ConfigureCertificateState, ConfiguringState, SwitchControllerState};
use model::test_support::rms_rack_profiles;
use sqlx::{PgConnection, PgPool};
use state_controller::controller::StateController;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Shared environment for switch controller integration tests.
pub(super) struct ControllerEnv {
    pub(super) api: Arc<Api>,
    pub(super) harness: TestHarness,
    pub(super) pool: PgPool,
    pub(super) redfish_sim: Arc<RedfishSim>,
    pub(super) test_credential_manager: Arc<TestCredentialManager>,
    pub(super) rms_sim: Arc<RmsSim>,
    /// Per-object metrics registry shared by test controller services.
    pub(super) per_object_metrics_registry: Arc<PerObjectMetricsRegistry>,
    switch_controller: Mutex<StateController<SwitchStateControllerIO>>,
}

impl ControllerEnv {
    /// Builds a controller test environment over `pool`.
    pub(super) async fn new(pool: PgPool) -> Self {
        let test_credential_manager = Arc::new(TestCredentialManager::default());
        let redfish_sim = Arc::new(RedfishSim::default());
        let rms_sim = Arc::new(RmsSim::default());
        let rack_profiles = rms_rack_profiles();
        let mut runtime_config = carbide_test_harness::test_support::default_config::get();
        runtime_config.rack_profiles = rack_profiles.clone();
        let per_object_metrics_registry = PerObjectMetricsRegistry::new(
            runtime_config
                .observability
                .per_object_metrics_for_classifications
                .clone(),
            Duration::from_secs(60),
        );
        let component_manager = component_manager::component_manager::build_component_manager(
            &ComponentManagerConfig {
                nv_switch_backend: NvSwitchBackend::Rms,
                power_shelf_backend: PowerShelfBackend::Rms,
                compute_tray_backend: ComputeBackend::Mock,
                nv_switch_use_state_controller: true,
                power_shelf_use_state_controller: true,
                ..Default::default()
            },
            rack_profiles,
            rms_sim.as_rms_client(),
            None,
            Some(pool.clone()),
            None,
        )
        .await
        .expect("test component manager should build");
        let component_manager = Arc::new(component_manager);
        let api_component_manager = component_manager.clone();
        let api_credential_manager = test_credential_manager.clone();
        let api_redfish_sim = redfish_sim.clone();
        let api_rms_client = rms_sim
            .as_rms_client()
            .expect("RMS simulator should provide a client");
        let harness = TestHarness::builder(pool.clone())
            .with_api_builder_fn(move |builder| {
                builder
                    .with_runtime_config(Arc::new(runtime_config))
                    .with_credential_manager(api_credential_manager)
                    .with_redfish_pool(api_redfish_sim)
                    .with_rms_client(api_rms_client)
                    .with_component_manager(api_component_manager)
            })
            .build()
            .await;
        let api = harness.api_arc();
        let switch_controller = StateController::builder()
            .database(pool.clone(), api.work_lock_manager_handle())
            .meter("carbide_switches", harness.test_meter.meter())
            .processor_id(uuid::Uuid::new_v4().to_string())
            .services(
                SwitchStateHandlerServices {
                    db_pool: pool.clone(),
                    component_manager: Some(component_manager),
                    credential_manager: test_credential_manager.clone(),
                    switch_mtls_services: default_switch_mtls_services(),
                    per_object_metrics_registry: per_object_metrics_registry.clone(),
                    redfish_client_pool: redfish_sim.clone(),
                    bmc_credential_ops: redfish_sim.clone(),
                    bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                        db::credential_rotation::CredentialRotationType::Bmc,
                    ),
                    bmc_rotation_enabled: false,
                }
                .into(),
            )
            .state_handler(Arc::new(SwitchStateHandler::default()))
            .build_for_manual_iterations(CancellationToken::new())
            .expect("switch state controller should build");

        Self {
            api,
            harness,
            pool,
            redfish_sim,
            test_credential_manager,
            rms_sim,
            per_object_metrics_registry,
            switch_controller: Mutex::new(switch_controller),
        }
    }

    /// Runs one iteration of the shared switch state controller.
    pub(super) async fn run_switch_controller_iteration(&self) {
        self.switch_controller
            .lock()
            .await
            .run_single_iteration()
            .await;
    }
}

/// Returns the default mTLS service list used by switch tests.
pub(super) fn default_switch_mtls_services() -> Vec<i32> {
    component_manager::config::switch_mtls_services_as_i32(
        &component_manager::config::effective_switch_mtls_services(&[]),
    )
}

/// Creates a discovered switch and the BMC/NVOS endpoints needed by the
/// controller state flows.
pub(super) async fn new_switch(
    env: &ControllerEnv,
    name: Option<String>,
    _location: Option<String>,
) -> eyre::Result<SwitchId> {
    let fixture_id = uuid::Uuid::new_v4();
    let bytes = fixture_id.as_bytes();
    let fixture_index: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM switches")
        .fetch_one(&env.pool)
        .await?;
    let address_subnet = u8::try_from(fixture_index / 100)?;
    let address_host = u8::try_from(10 + (fixture_index % 100) * 2)?;
    let bmc_ip = std::net::Ipv4Addr::new(198, 18, address_subnet, address_host);
    let nvos_ip = std::net::Ipv4Addr::new(198, 18, address_subnet, address_host + 1);
    let bmc_mac =
        mac_address::MacAddress::new([0x02, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4]]);
    let nvos_mac =
        mac_address::MacAddress::new([0x02, bytes[5], bytes[6], bytes[7], bytes[8], bytes[9]]);
    let name = name.unwrap_or_else(|| "Switch1".to_string());
    let (nvos_username, nvos_password) = if name == "Switch4" {
        (
            Some("nvos_admin1".to_string()),
            Some("nvos_pass1".to_string()),
        )
    } else {
        (None, None)
    };
    let switch = env
        .harness
        .create_expected_switch(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".to_string(),
            bmc_password: "Pwd2023x0x0x0x7".to_string(),
            switch_serial_number: format!("SW-{}", &fixture_id.simple().to_string()[..24]),
            metadata: Some(rpc::forge::Metadata {
                name,
                ..Default::default()
            }),
            nvos_username,
            nvos_password,
            nvos_mac_addresses: vec![nvos_mac.to_string()],
            bmc_ip_address: bmc_ip.to_string(),
            nvos_ip_address: Some(nvos_ip.to_string()),
            ..Default::default()
        })
        .await
        .create_switch(0, 0)
        .await;

    let mut txn = env.pool.begin().await?;
    let segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version, network_segment_type) \
         VALUES ($1, 'V1-T0', 'underlay') RETURNING id",
    )
    .bind(format!("{fixture_id}-underlay"))
    .fetch_one(txn.as_mut())
    .await?;
    let bmc_interface_id: MachineInterfaceId = sqlx::query_scalar(
        "INSERT INTO machine_interfaces \
             (switch_id, association_type, segment_id, mac_address, \
              primary_interface, hostname, interface_type) \
         VALUES ($1, 'Switch', $2, $3, false, 'bmc', 'Bmc') \
         RETURNING id",
    )
    .bind(switch.id)
    .bind(segment_id)
    .bind(bmc_mac)
    .fetch_one(txn.as_mut())
    .await?;
    db::machine_interface_address::insert(
        txn.as_mut(),
        bmc_interface_id,
        bmc_ip.into(),
        AllocationType::Dhcp,
    )
    .await?;
    let nvos_interface_id: MachineInterfaceId = sqlx::query_scalar(
        "INSERT INTO machine_interfaces \
             (segment_id, mac_address, primary_interface, hostname) \
         VALUES ($1, $2, false, 'nvos') RETURNING id",
    )
    .bind(segment_id)
    .bind(nvos_mac)
    .fetch_one(txn.as_mut())
    .await?;
    db::machine_interface_address::insert(
        txn.as_mut(),
        nvos_interface_id,
        nvos_ip.into(),
        AllocationType::Dhcp,
    )
    .await?;
    txn.commit().await?;

    Ok(switch.id)
}

/// Sets the switch controller state directly in the database.
pub(super) async fn set_switch_controller_state(
    txn: &mut PgConnection,
    switch_id: &SwitchId,
    state: SwitchControllerState,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE switches SET controller_state = $1 WHERE id = $2")
        .bind(serde_json::to_value(state).unwrap())
        .bind(switch_id)
        .execute(txn)
        .await?;

    Ok(())
}

/// Associates a switch with a rack directly in the database.
pub(super) async fn set_switch_rack_id(
    txn: &mut PgConnection,
    switch_id: &SwitchId,
    rack_id: &RackId,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE switches SET rack_id = $1 WHERE id = $2")
        .bind(rack_id)
        .bind(switch_id)
        .execute(txn)
        .await?;
    Ok(())
}

/// Transitions the persisted switch controller state.
pub(super) async fn transition_switch_controller_state(
    txn: &mut PgConnection,
    switch_id: &SwitchId,
    new_state: SwitchControllerState,
) -> Result<(), Box<dyn std::error::Error>> {
    let switch = db_switch::find_by_id(txn, switch_id)
        .await?
        .expect("switch should exist");
    db_switch::try_update_controller_state(
        txn,
        *switch_id,
        switch.controller_state.version,
        switch.controller_state.version.increment(),
        &new_state,
    )
    .await?;
    Ok(())
}

/// Returns the initial configure-certificate controller state.
pub(super) fn configure_certificate_start_state() -> SwitchControllerState {
    SwitchControllerState::Configuring {
        config_state: ConfiguringState::ConfigureCertificate {
            configure_certificate: ConfigureCertificateState::Start,
        },
    }
}

/// Returns the configure-certificate wait state for `job_id`.
pub(super) fn configure_certificate_wait_state(job_id: &str) -> SwitchControllerState {
    SwitchControllerState::Configuring {
        config_state: ConfiguringState::ConfigureCertificate {
            configure_certificate: ConfigureCertificateState::WaitForComplete {
                job_id: job_id.to_string(),
            },
        },
    }
}

/// Helper function to mark switch as deleted
pub(super) async fn mark_switch_as_deleted(
    txn: &mut PgConnection,
    switch_id: &SwitchId,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE switches SET deleted = NOW() WHERE id = $1")
        .bind(switch_id)
        .execute(txn)
        .await?;

    Ok(())
}
