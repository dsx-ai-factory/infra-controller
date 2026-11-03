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
use std::sync::Arc;

use carbide_health_metrics::PerObjectMetricsRegistry;
use carbide_rack::test_support::RmsSim;
use carbide_rack_controller::config::RackConfig as ControllerConfig;
use carbide_rack_controller::context::RackStateHandlerServices;
use carbide_rack_controller::firmware_object::FirmwareObjectFetcher;
use carbide_rack_controller::handler::RackStateHandler;
use carbide_rack_controller::io::RackStateControllerIO;
use carbide_secrets::credentials::{CompositeCredentialManager, CredentialReader};
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_secrets::{ChainedCredentialReader, CredentialSnapshot, UsernamePassword};
use carbide_test_harness::network::segment::TestNetworkSegment;
use carbide_test_harness::prelude::*;
use carbide_test_harness::{CarbideConfig, test_support};
use carbide_uuid::rack::{RackId, RackProfileId};
use component_manager::component_manager::ComponentManager;
use component_manager::nv_switch_manager::NvSwitchManager;
use db::db_read::PgPoolReader;
use db::{DatabaseError, rack as db_rack};
use model::rack::RackConfig;
use model::test_support::ManagedHostConfig;
use sqlx::PgConnection;
use state_controller::controller::StateController;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(super) struct TestEnvOverrides {
    pub(super) config: Option<CarbideConfig>,
    pub(super) firmware_object_fetcher: Option<Arc<dyn FirmwareObjectFetcher>>,
    pub(super) rack_component_manager_enabled: Option<bool>,
    pub(super) nv_switch_manager: Option<Arc<dyn NvSwitchManager>>,
}

impl TestEnvOverrides {
    pub(super) fn with_config(config: CarbideConfig) -> Self {
        Self {
            config: Some(config),
            ..Default::default()
        }
    }
}

pub(super) struct TestEnv {
    pub(super) api: Arc<Api>,
    pub(super) pool: PgPool,
    pub(super) rms_sim: Arc<RmsSim>,
    pub(super) test_credential_manager: Arc<TestCredentialManager>,
    config: Arc<CarbideConfig>,
    component_manager: Arc<ComponentManager>,
    /// Firmware-object fetcher used by this environment's rack state handler.
    firmware_object_fetcher: Arc<dyn FirmwareObjectFetcher>,
    controller: Mutex<StateController<RackStateControllerIO>>,
    pub(super) test_harness: TestHarness,
    site_explorer: TestSiteExplorer,
    underlay_segment: TestNetworkSegment,
    admin_segment: TestNetworkSegment,
    _cancel_token: CancellationToken,
}

pub(super) fn get_config() -> CarbideConfig {
    test_support::default_config::get()
}

pub(super) async fn create_test_env(pool: PgPool) -> TestEnv {
    create_test_env_with_overrides(pool, TestEnvOverrides::default()).await
}

pub(super) fn create_test_env_with_overrides(
    pool: PgPool,
    overrides: TestEnvOverrides,
) -> impl std::future::Future<Output = TestEnv> {
    Box::pin(create_test_env_inner(pool, overrides))
}

async fn create_test_env_inner(pool: PgPool, overrides: TestEnvOverrides) -> TestEnv {
    let config = Arc::new(overrides.config.unwrap_or_else(get_config));
    let credentials = Arc::new(TestCredentialManager::default());
    let rms_sim = Arc::new(RmsSim::default());
    let firmware_object_fetcher = overrides
        .firmware_object_fetcher
        .unwrap_or_else(|| Arc::new(reqwest::Client::new()));
    let mut profiles = model::test_support::rack::rms_rack_profiles();
    // Incomplete custom profiles must reach the handler's call-time validation.
    profiles.rack_profiles.extend(
        config
            .rack_profiles
            .rack_profiles
            .iter()
            .filter(|(_, p)| {
                p.product_family.is_some()
                    && p.rack_capabilities
                        .switch
                        .vendor
                        .as_deref()
                        .is_some_and(|v| !v.trim().is_empty())
                    && p.rack_capabilities
                        .power_shelf
                        .vendor
                        .as_deref()
                        .is_some_and(|v| !v.trim().is_empty())
            })
            .map(|(id, profile)| (id.clone(), profile.clone())),
    );
    let mut component_manager = component_manager::component_manager::build_component_manager(
        &component_manager::config::ComponentManagerConfig {
            nv_switch_backend: component_manager::nv_switch_manager::Backend::Rms,
            power_shelf_backend: component_manager::power_shelf_manager::Backend::Rms,
            compute_tray_backend: component_manager::compute_tray_manager::Backend::Mock,
            nv_switch_use_state_controller: true,
            power_shelf_use_state_controller: true,
            ..Default::default()
        },
        profiles.clone(),
        rms_sim.as_rms_client(),
        None,
        Some(pool.clone()),
        None,
    )
    .await
    .expect("test component manager should build");
    if let Some(manager) = overrides.nv_switch_manager {
        component_manager.nv_switch = manager;
    }
    let component_manager = Arc::new(component_manager);
    let api_config = config.clone();
    let api_credentials = Arc::new(CompositeCredentialManager::new(
        ChainedCredentialReader::from(vec![
            Box::new(CredentialSnapshot {
                dpu_redfish_factory_default: Some(UsernamePassword {
                    username: "root".into(),
                    password: "dpuredfish_dpuhardwaredefault".into(),
                }),
                dpu_redfish_site_default: Some(UsernamePassword {
                    username: "root".into(),
                    password: "dpuredfish_sitedefault".into(),
                }),
                host_redfish_site_default: Some(UsernamePassword {
                    username: "root".into(),
                    password: "hostredfish_sitedefault".into(),
                }),
                machine_identity: Some(carbide_secrets::MachineIdentityConfig {
                    encryption_keys: HashMap::from([(
                        "test".into(),
                        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
                    )]),
                }),
                ..Default::default()
            }) as Box<dyn CredentialReader>,
            Box::new(credentials.clone()),
        ]),
        credentials.clone(),
    ));
    let api_component_manager = component_manager.clone();
    let api_rms = rms_sim.as_rms_client();
    let api_fetcher = firmware_object_fetcher.clone();
    let test_harness = TestHarness::builder(pool.clone())
        .with_api_builder_fn(move |builder| {
            let builder = builder
                .with_runtime_config(api_config)
                .with_credential_manager(api_credentials)
                .with_component_manager(api_component_manager)
                .with_firmware_object_fetcher(api_fetcher);
            match api_rms {
                Some(client) => builder.with_rms_client(client),
                None => builder,
            }
        })
        .build()
        .await;
    let api = test_harness.api_arc();
    let domain = test_harness.test_domain().await;
    let nc = test_harness.network_controller();
    let underlay_segment = nc.create_underlay_segment(&domain).await;
    let admin_segment = nc.create_admin_segment(&domain).await;
    let explorer_config = carbide_site_explorer::config::SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        explorations_per_run: 100,
        concurrent_explorations: 100,
        machines_created_per_run: 1,
        create_machines: Arc::new(true.into()),
        create_switches: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        rotate_switch_nvos_credentials: Arc::new(false.into()),
        ..Default::default()
    };
    let endpoint_explorer =
        Arc::new(carbide_site_explorer::test_support::MockEndpointExplorer::default());
    let exploration_service = Arc::new(carbide_site_explorer::EndpointExplorationService::new(
        pool.clone(),
        endpoint_explorer.clone(),
        Arc::new(config.get_firmware_config()),
    ));
    let mut profiles = model::test_support::rms_rack_profiles();
    profiles
        .rack_profiles
        .extend(config.rack_profiles.rack_profiles.clone());
    let explorer = TestSiteExplorer::new(
        carbide_site_explorer::SiteExplorer::new(
            pool.clone(),
            explorer_config,
            test_harness.test_meter.meter(),
            exploration_service,
            endpoint_explorer.clone(),
            api.common_pools().clone(),
            api.work_lock_manager_handle(),
            profiles,
            rms_sim.as_rms_client().map(|client| {
                Arc::new(component_manager::rms::rms_machine_info_provider(client)) as _
            }),
            credentials.clone(),
            false,
        ),
        endpoint_explorer,
    );
    let services = services(
        &pool,
        &config,
        &rms_sim,
        &credentials,
        Some(component_manager.clone()),
        firmware_object_fetcher.clone(),
    );
    let cancel_token = CancellationToken::new();
    let controller = StateController::builder()
        .database(pool.clone(), api.work_lock_manager_handle())
        .meter("carbide_racks", test_harness.test_meter.meter())
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(
            RackStateHandlerServices {
                component_manager: overrides
                    .rack_component_manager_enabled
                    .unwrap_or(true)
                    .then(|| component_manager.clone()),
                ..services
            }
            .into(),
        )
        .state_handler(Arc::new(RackStateHandler::default()))
        .build_for_manual_iterations(cancel_token.clone())
        .expect("rack controller should build");
    TestEnv {
        api,
        pool,
        rms_sim,
        test_credential_manager: credentials,
        config,
        component_manager,
        firmware_object_fetcher,
        controller: Mutex::new(controller),
        test_harness,
        site_explorer: explorer,
        underlay_segment,
        admin_segment,
        _cancel_token: cancel_token,
    }
}

fn services(
    pool: &PgPool,
    config: &CarbideConfig,
    rms_sim: &RmsSim,
    credentials: &Arc<TestCredentialManager>,
    component_manager: Option<Arc<ComponentManager>>,
    firmware_object_fetcher: Arc<dyn FirmwareObjectFetcher>,
) -> RackStateHandlerServices {
    RackStateHandlerServices {
        db_pool: pool.clone(),
        site_config: ControllerConfig {
            rms: config.rms.clone(),
            rack_validation_config: config.rack_validation_config.clone(),
            rack_profiles: config.rack_profiles.clone(),
        }
        .into(),
        nvos_update_manager: rms_sim
            .as_rms_client()
            .map(|client| Arc::new(component_manager::rms::rms_nvos_update_manager(client)) as _),
        rack_firmware_update_manager: rms_sim.as_rms_client().map(|client| {
            Arc::new(component_manager::rms::rms_rack_firmware_update_manager(
                client,
            )) as _
        }),
        credential_manager: credentials.clone(),
        component_manager,
        switch_mtls_services: config.switch_state_controller.switch_mtls_services.clone(),
        firmware_object_fetcher,
        per_object_metrics_registry: PerObjectMetricsRegistry::new(
            config
                .observability
                .per_object_metrics_for_classifications
                .clone(),
            std::time::Duration::from_secs(60),
        ),
    }
}

impl TestEnv {
    pub(super) fn db_reader(&self) -> PgPoolReader {
        self.pool.clone().into()
    }
    /// Creates an instance of RackStateHandlerServices that are suitable for this
    /// test environment
    pub(super) fn rack_state_handler_services(&self) -> RackStateHandlerServices {
        services(
            &self.pool,
            &self.config,
            &self.rms_sim,
            &self.test_credential_manager,
            Some(self.component_manager.clone()),
            self.firmware_object_fetcher.clone(),
        )
    }
    pub(super) async fn run_rack_controller_iteration(&self) {
        self.controller.lock().await.run_single_iteration().await;
    }
}

pub(super) fn new_host(
    env: &TestEnv,
    config: ManagedHostConfig,
) -> impl std::future::Future<Output = eyre::Result<model::machine::ManagedHostStateSnapshot>> + '_
{
    // Keep host ingestion's large future out of the enclosing rack test future
    // so multi-device fixtures fit the default test thread stack.
    Box::pin(new_host_inner(env, config))
}

async fn new_host_inner(
    env: &TestEnv,
    mut config: ManagedHostConfig,
) -> eyre::Result<model::machine::ManagedHostStateSnapshot> {
    // Match the legacy ingestion fixture: an explicit rack assignment still
    // carries the host serial, and RMS inventory reads per-device credentials.
    config
        .expected_machine_data
        .get_or_insert_default()
        .serial_number = config.serial.clone();
    for bmc_mac_address in
        std::iter::once(config.bmc_mac_address).chain(config.dpus.iter().map(|d| d.bmc_mac_address))
    {
        env.api
            .credential_manager()
            .set_credentials(
                &carbide_secrets::credentials::CredentialKey::BmcCredentials {
                    credential_type: carbide_secrets::credentials::BmcCredentialType::BmcRoot {
                        bmc_mac_address,
                    },
                },
                &carbide_secrets::credentials::Credentials::UsernamePassword {
                    username: "root".into(),
                    password: "notforprod".into(),
                },
            )
            .await?;
    }
    let (mut mh, _) = env
        .test_harness
        .managed_host_builder(&env.site_explorer, env.underlay_segment)
        .with_dpu_primary_interfaces(env.admin_segment)
        .with_config(config)
        .build()
        .await;
    for dpu in &mh.dpus {
        dpu.discover_oob_iface(env.admin_segment).await;
    }
    mh.host.discover_primary_iface(env.admin_segment).await;
    mh.report_dpu_network_status().await;
    mh.insert_empty_host_health_report(format!(
        "{}health",
        model::machine::health_override::HARDWARE_HEALTH_OVERRIDE_PREFIX
    ))
    .await;
    // Rack tests require ingested hosts with the same converged Ready state;
    // machine-controller transitions are covered by its owning suite.
    mh.advance_to_converged_ready().await;
    Ok(db::managed_host::load_snapshot(
        &mut env.db_reader(),
        &mh.host.id.into(),
        Default::default(),
    )
    .await
    .transpose()
    .expect("managed host should exist")?)
}
pub(super) struct TestRackDbBuilder {
    rack_id: RackId,
    rack_profile_id: Option<RackProfileId>,
}

impl Default for TestRackDbBuilder {
    fn default() -> Self {
        TestRackDbBuilder {
            rack_id: RackId::new(uuid::Uuid::new_v4().to_string()),
            rack_profile_id: Some(RackProfileId::new(
                model::test_support::TEST_RMS_RACK_PROFILE_ID,
            )),
        }
    }
}

impl TestRackDbBuilder {
    pub(super) fn new() -> TestRackDbBuilder {
        TestRackDbBuilder {
            ..Default::default()
        }
    }

    pub(super) fn with_rack_id(mut self, id: RackId) -> Self {
        self.rack_id = id;
        self
    }

    pub(super) fn with_rack_profile_id(mut self, rack_profile_id: impl Into<String>) -> Self {
        self.rack_profile_id = Some(RackProfileId::new(rack_profile_id));
        self
    }

    pub(super) async fn persist(&self, txn: &mut PgConnection) -> Result<RackId, DatabaseError> {
        let rack_config = RackConfig::default();
        db_rack::create(
            txn,
            &self.rack_id,
            self.rack_profile_id.as_ref(),
            &rack_config,
            None,
        )
        .await?;

        Ok(self.rack_id.clone())
    }
}
