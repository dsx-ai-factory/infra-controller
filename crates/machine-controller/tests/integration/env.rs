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

use carbide_health_metrics::PerObjectMetricsRegistry;
use carbide_machine_controller::context::MachineStateHandlerServices;
use carbide_machine_controller::handler::{
    MachineStateHandlerBuilder, PowerOptionConfig, ReachabilityParams,
};
use carbide_machine_controller::io::MachineStateControllerIO;
use carbide_redfish::libredfish::test_support::RedfishSim;
use carbide_secrets::credentials::CredentialManager;
use carbide_test_harness::prelude::*;
use carbide_test_harness::{CarbideConfig, test_support};
use component_manager::component_manager::ComponentManager;
use futures::FutureExt as _;
use model::machine::slas::MachineSlaConfig;
use state_controller::controller::StateController;
use tokio_util::sync::CancellationToken;

pub(super) struct Env {
    pub(super) test_harness: TestHarness,
    pub(super) redfish_sim: Arc<RedfishSim>,
    machine_controller: StateController<MachineStateControllerIO>,
    _cancel_token: CancellationToken,
}

pub(super) struct EnvBuilder {
    pool: PgPool,
    runtime_config: CarbideConfig,
    component_manager: Option<Arc<ComponentManager>>,
    credential_manager: Option<Arc<dyn CredentialManager>>,
}

impl Env {
    pub(super) fn builder(pool: PgPool) -> EnvBuilder {
        let mut runtime_config = test_support::default_config::get();
        runtime_config.machine_state_controller.dpu_wait_time = chrono::Duration::zero();
        runtime_config.machine_state_controller.power_down_wait = chrono::Duration::zero();
        runtime_config.machine_state_controller.failure_retry_time = chrono::Duration::zero();
        runtime_config.machine_state_controller.uefi_boot_wait = chrono::Duration::zero();

        EnvBuilder {
            pool,
            runtime_config,
            component_manager: None,
            credential_manager: None,
        }
    }

    pub(super) async fn run_single_iteration(&mut self) {
        self.machine_controller.run_single_iteration().boxed().await;
    }
}

impl EnvBuilder {
    pub(super) fn configure_runtime(mut self, configure: impl FnOnce(&mut CarbideConfig)) -> Self {
        configure(&mut self.runtime_config);
        self
    }

    pub(super) fn with_component_manager(
        mut self,
        component_manager: Arc<ComponentManager>,
    ) -> Self {
        self.component_manager = Some(component_manager);
        self
    }

    pub(super) fn with_credential_manager(
        mut self,
        credential_manager: Arc<dyn CredentialManager>,
    ) -> Self {
        self.credential_manager = Some(credential_manager);
        self
    }

    pub(super) async fn build(self) -> Env {
        let Self {
            pool,
            runtime_config,
            component_manager,
            credential_manager,
        } = self;
        let redfish_sim = Arc::new(RedfishSim::default());
        let controller_redfish_sim = redfish_sim.clone();
        let api_redfish_sim = redfish_sim.clone();
        let runtime_config = Arc::new(runtime_config);
        let api_runtime_config = runtime_config.clone();
        let test_harness = TestHarness::builder(pool.clone())
            .with_api_builder_fn(move |builder| {
                builder
                    .with_runtime_config(api_runtime_config)
                    .with_redfish_pool(api_redfish_sim)
            })
            .build()
            .await;
        let api = test_harness.api();
        let cancel_token = CancellationToken::new();

        let reachability_params = ReachabilityParams {
            dpu_wait_time: runtime_config.machine_state_controller.dpu_wait_time,
            power_down_wait: runtime_config.machine_state_controller.power_down_wait,
            failure_retry_time: runtime_config.machine_state_controller.failure_retry_time,
            scout_reporting_timeout: runtime_config
                .machine_state_controller
                .scout_reporting_timeout,
            waiting_for_measurements_timeout: runtime_config
                .machine_state_controller
                .waiting_for_measurements_timeout,
            uefi_boot_wait: runtime_config.machine_state_controller.uefi_boot_wait,
        };
        let power_options: PowerOptionConfig = runtime_config.power_manager_options.clone().into();
        let machine_handler = MachineStateHandlerBuilder::builder()
            .dpu_up_threshold(runtime_config.machine_state_controller.dpu_up_threshold)
            .dpu_nic_firmware_reprovision_update_enabled(
                runtime_config
                    .dpu_config
                    .dpu_nic_firmware_reprovision_update_enabled,
            )
            .hardware_models(runtime_config.get_firmware_config())
            .reachability_params(reachability_params)
            .attestation_enabled(runtime_config.attestation_enabled)
            .common_pools(api.common_pools().clone())
            .dpu_enable_secure_boot(runtime_config.dpu_config.dpu_enable_secure_boot)
            .machine_validation_config(runtime_config.machine_validation_config.clone())
            .bom_validation(runtime_config.bom_validation)
            .no_firmware_update_reset_retries(runtime_config.firmware_global.no_reset_retries)
            .instance_autoreboot_period(
                runtime_config
                    .machine_updater
                    .instance_autoreboot_period
                    .clone(),
            )
            .credential_reader(api.credential_manager().clone())
            .power_options_config(power_options)
            .build();
        let per_object_metrics_registry = PerObjectMetricsRegistry::new(
            runtime_config
                .observability
                .per_object_metrics_for_classifications
                .clone(),
            std::time::Duration::from_secs(60),
        );
        let services = MachineStateHandlerServices {
            db_pool: pool.clone(),
            db_reader: pool.clone().into(),
            redfish_client_pool: controller_redfish_sim.clone(),
            bmc_credential_ops: controller_redfish_sim,
            ipmi_tool: carbide_ipmi::test_support(),
            site_config: runtime_config.machine_state_handler_site_config().into(),
            component_manager,
            credential_manager: credential_manager
                .unwrap_or_else(|| api.credential_manager().clone()),
            // Zero TTL: every guard re-queries the aggregate so tests observe a
            // freshly staged rotation on the next iteration without waiting out
            // the cache window.
            bmc_rotation_gate: carbide_credential_rotation::RotationGate::with_ttl_and_family(
                std::time::Duration::ZERO,
                db::credential_rotation::CredentialRotationType::Bmc,
            ),
            // Zero TTL for the same reason as the BMC gate above.
            host_uefi_rotation_gate: carbide_credential_rotation::RotationGate::with_ttl_and_family(
                std::time::Duration::ZERO,
                db::credential_rotation::CredentialRotationType::HostUefi,
            ),
            // Zero TTL for the same reason as the BMC gate above.
            dpu_uefi_rotation_gate: carbide_credential_rotation::RotationGate::with_ttl_and_family(
                std::time::Duration::ZERO,
                db::credential_rotation::CredentialRotationType::DpuUefi,
            ),
            // Zero TTL for the same reason as the BMC gate above.
            dpu_bmc_service_rotation_gate:
                carbide_credential_rotation::RotationGate::with_ttl_and_family(
                    std::time::Duration::ZERO,
                    db::credential_rotation::CredentialRotationType::DpuBmcService,
                ),
            nic_lockdown_rotation_gate:
                carbide_credential_rotation::RotationGate::with_ttl_and_family(
                    std::time::Duration::ZERO,
                    db::credential_rotation::CredentialRotationType::LockdownIkm,
                ),
            per_object_metrics_registry,
            per_object_info: None,
        };
        let machine_controller = StateController::<MachineStateControllerIO>::builder()
            .database(pool, api.work_lock_manager_handle())
            .meter("carbide_machines", test_harness.test_meter.meter())
            .processor_id(uuid::Uuid::new_v4().to_string())
            .services(Arc::new(services))
            .iteration_config((&runtime_config.machine_state_controller.controller).into())
            .state_handler(Arc::new(machine_handler))
            .io(Arc::new(MachineStateControllerIO {
                host_health: runtime_config.host_health,
                sla_config: MachineSlaConfig::new(
                    runtime_config.machine_state_controller.failure_retry_time,
                ),
            }))
            .build_for_manual_iterations(cancel_token.clone())
            .expect("machine state controller should be built");

        Env {
            test_harness,
            redfish_sim,
            machine_controller,
            _cancel_token: cancel_token,
        }
    }
}

#[sqlx_test]
async fn builder_applies_runtime_configuration(pool: PgPool) {
    let dpu_up_threshold = chrono::Duration::seconds(7);
    let env = Env::builder(pool)
        .configure_runtime(|config| {
            config.machine_state_controller.dpu_up_threshold = dpu_up_threshold;
        })
        .build()
        .await;

    assert_eq!(
        env.test_harness
            .api()
            .runtime_config
            .machine_state_controller
            .dpu_up_threshold,
        dpu_up_threshold
    );
}
