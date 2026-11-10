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

use carbide_credential_rotation::RotationGate;
use carbide_health_metrics::PerObjectMetricsRegistry;
use carbide_ib_fabric::IbFabricMonitor;
use carbide_ib_fabric::ib::IBFabricManagerImpl;
use carbide_machine_controller::context::MachineStateHandlerServices;
use carbide_machine_controller::handler::{
    MachineStateHandlerBuilder, PowerOptionConfig, ReachabilityParams,
};
use carbide_machine_controller::io::MachineStateControllerIO;
use carbide_nvlink_manager::NvlPartitionMonitor;
use carbide_nvlink_manager::nvlink::test_support::NmxcSimClient;
use carbide_redfish::libredfish::test_support::RedfishSim;
use carbide_test_harness::prelude::*;
use futures_util::FutureExt as _;
use health_report::HealthReport;
use model::machine::health_override::HARDWARE_HEALTH_OVERRIDE_PREFIX;
use model::machine::slas::MachineSlaConfig;
use model::machine::{InstanceState, ManagedHostState};
use state_controller::controller::StateController;
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tonic::IntoRequest;

pub(super) struct ControllerClients {
    pub(super) redfish: Arc<RedfishSim>,
    pub(super) ib: Arc<IBFabricManagerImpl>,
    pub(super) nmxc: Arc<NmxcSimClient>,
}

pub(super) struct InstanceControllers {
    machine: Mutex<StateController<MachineStateControllerIO>>,
    ib: IbFabricMonitor,
    nvl: Mutex<NvlPartitionMonitor>,
    _cancel_token: CancellationToken,
}

impl InstanceControllers {
    pub(super) fn new(harness: &TestHarness, clients: &ControllerClients) -> Self {
        let cancel_token = CancellationToken::new();
        let api = harness.api();
        let config = &api.runtime_config;
        Self {
            machine: Mutex::new(machine_state_controller(
                harness,
                clients,
                cancel_token.clone(),
            )),
            ib: IbFabricMonitor::new(
                api.database_connection.clone(),
                config.ib_fabrics.clone(),
                harness.test_meter.meter(),
                clients.ib.clone(),
                config.host_health,
                api.work_lock_manager_handle(),
            ),
            nvl: Mutex::new(NvlPartitionMonitor::new(
                api.database_connection.clone(),
                clients.nmxc.clone(),
                harness.test_meter.meter(),
                config
                    .nvlink_config
                    .clone()
                    .expect("NVLink configuration should exist"),
                config.host_health,
                api.work_lock_manager_handle(),
            )),
            _cancel_token: cancel_token,
        }
    }

    pub(super) async fn advance_instance_to_ready(
        &self,
        harness: &TestHarness,
        mh: &TestManagedHost,
        instance: &TestInstance,
    ) {
        harness.network_controller().run_single_iteration().await;
        self.until(mh, 10, InstanceState::WaitingForNetworkConfig)
            .await;
        assert_eq!(
            history(mh, 4).await,
            vec![
                assigned(InstanceState::DpaProvisioning),
                assigned(InstanceState::WaitingForDpaToBeReady),
                assigned(InstanceState::WaitingForNetworkSegmentToBeReady),
                assigned(InstanceState::WaitingForNetworkConfig),
            ]
        );
        for dpu in &mh.dpus {
            let status = dpu.network_status().dpu_agent_version("test").build().await;
            harness
                .api()
                .record_dpu_network_status(status.into_request())
                .await
                .unwrap();
        }
        self.machine
            .lock()
            .await
            .run_single_iteration()
            .boxed()
            .await;
        self.ib.run_single_iteration().boxed().await.unwrap();
        self.ib.run_single_iteration().boxed().await.unwrap();
        for _ in 0..3 {
            self.nvl
                .lock()
                .await
                .run_single_iteration()
                .boxed()
                .await
                .unwrap();
        }
        harness
            .api()
            .insert_machine_health_report(
                rpc::forge::InsertMachineHealthReportRequest {
                    machine_id: Some(mh.host.id.into()),
                    health_report_entry: Some(rpc::forge::HealthReportEntry {
                        report: Some(
                            HealthReport::empty(format!("{HARDWARE_HEALTH_OVERRIDE_PREFIX}health"))
                                .into(),
                        ),
                        ..Default::default()
                    }),
                }
                .into_request(),
            )
            .await
            .unwrap();
        self.until(mh, 20, InstanceState::Ready).await;
        assert_eq!(
            history(mh, 2).await,
            vec![
                assigned(InstanceState::WaitingForRebootToReady),
                assigned(InstanceState::Ready),
            ]
        );
        instance.rpc_instance().await;
    }

    async fn until(&self, mh: &TestManagedHost, max_iterations: u32, state: InstanceState) {
        let expected = assigned(state);
        for _ in 0..max_iterations {
            self.machine
                .lock()
                .await
                .run_single_iteration()
                .boxed()
                .await;
            if mh.host.machine().await.current_state() == &expected {
                return;
            }
        }
        panic!(
            "Expected Machine state condition not hit after {max_iterations} iterations; state is {:?}",
            mh.host.machine().await.current_state()
        );
    }
}

fn assigned(instance_state: InstanceState) -> ManagedHostState {
    ManagedHostState::Assigned { instance_state }
}

async fn history(mh: &TestManagedHost, limit: usize) -> Vec<ManagedHostState> {
    let states: Vec<ManagedHostState> = mh
        .host
        .rpc_machine()
        .await
        .events
        .into_iter()
        .map(|e| serde_json::from_str(&e.event).unwrap())
        .collect();
    states[states.len().saturating_sub(limit)..].to_vec()
}

fn machine_state_controller(
    harness: &TestHarness,
    clients: &ControllerClients,
    cancel_token: CancellationToken,
) -> StateController<MachineStateControllerIO> {
    let api = harness.api();
    let meter = harness.test_meter.meter();
    let processor_id = uuid::Uuid::new_v4().to_string();
    let config = &api.runtime_config;
    let reachability_params = ReachabilityParams {
        dpu_wait_time: chrono::Duration::zero(),
        power_down_wait: chrono::Duration::zero(),
        failure_retry_time: chrono::Duration::zero(),
        scout_reporting_timeout: config.machine_state_controller.scout_reporting_timeout,
        waiting_for_measurements_timeout: config
            .machine_state_controller
            .waiting_for_measurements_timeout,
        uefi_boot_wait: chrono::Duration::zero(),
    };
    let power_options: PowerOptionConfig = config.power_manager_options.clone().into();
    let handler = MachineStateHandlerBuilder::builder()
        .hardware_models(config.get_firmware_config())
        .reachability_params(reachability_params)
        .attestation_enabled(config.attestation_enabled)
        .common_pools(api.common_pools().clone())
        .dpu_enable_secure_boot(config.dpu_config.dpu_enable_secure_boot)
        .machine_validation_config(config.machine_validation_config.clone())
        .bom_validation(config.bom_validation)
        .instance_autoreboot_period(config.machine_updater.instance_autoreboot_period.clone())
        .power_options_config(power_options)
        .dpf_sdk(None)
        .build();
    let rotation_gate = |family| RotationGate::new_for_family(family);
    let services = MachineStateHandlerServices {
        db_pool: api.database_connection.clone(),
        db_reader: api.database_connection.clone().into(),
        redfish_client_pool: clients.redfish.clone(),
        bmc_credential_ops: clients.redfish.clone(),
        ipmi_tool: carbide_ipmi::test_support(),
        site_config: config.machine_state_handler_site_config().into(),
        component_manager: None,
        credential_manager: api.credential_manager().clone(),
        bmc_rotation_gate: rotation_gate(db::credential_rotation::CredentialRotationType::Bmc),
        host_uefi_rotation_gate: rotation_gate(
            db::credential_rotation::CredentialRotationType::HostUefi,
        ),
        dpu_uefi_rotation_gate: rotation_gate(
            db::credential_rotation::CredentialRotationType::DpuUefi,
        ),
        dpu_bmc_service_rotation_gate: rotation_gate(
            db::credential_rotation::CredentialRotationType::DpuBmcService,
        ),
        nic_lockdown_rotation_gate: rotation_gate(
            db::credential_rotation::CredentialRotationType::LockdownIkm,
        ),
        per_object_metrics_registry: PerObjectMetricsRegistry::new(
            config
                .observability
                .per_object_metrics_for_classifications
                .clone(),
            Duration::from_secs(60),
        ),
        per_object_info: None,
    };
    StateController::<MachineStateControllerIO>::builder()
        .database(
            api.database_connection.clone(),
            api.work_lock_manager_handle(),
        )
        .meter("carbide_machines", meter)
        .processor_id(processor_id)
        .services(Arc::new(services))
        .state_handler(Arc::new(SerialHandler {
            inner: Mutex::new(handler),
        }))
        .io(Arc::new(MachineStateControllerIO {
            host_health: config.host_health,
            sla_config: MachineSlaConfig::new(config.machine_state_controller.failure_retry_time),
        }))
        .build_for_manual_iterations(cancel_token)
        .expect("machine state controller should be built")
}

#[derive(Debug)]
struct SerialHandler<H: StateHandler> {
    inner: Mutex<H>,
}

#[async_trait::async_trait]
impl<H: StateHandler> StateHandler for SerialHandler<H>
where
    H::ObjectId: Send + Sync,
    H::State: Send + Sync,
    H::ControllerState: Send + Sync,
    H::ContextObjects: Send + Sync,
{
    type ObjectId = H::ObjectId;
    type State = H::State;
    type ControllerState = H::ControllerState;
    type ContextObjects = H::ContextObjects;

    async fn handle_object_state(
        &self,
        object_id: &Self::ObjectId,
        state: &mut Self::State,
        controller_state: &Self::ControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<Self::ControllerState>, StateHandlerError> {
        self.inner
            .lock()
            .await
            .handle_object_state(object_id, state, controller_state, ctx)
            .await
    }
}
