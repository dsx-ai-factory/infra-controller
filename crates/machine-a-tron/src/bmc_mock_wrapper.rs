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

use axum::Router;
use bmc_mock::injection::InjectionStore;
use bmc_mock::ipmi_sim::{IpmiSimConfig, IpmiSimHandle};
use bmc_mock::{BmcState, Callbacks, CombinedServer, HardwareType, HostnameQuerying, MachineInfo};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::config::MachineATronContext;
use crate::machine_state_machine::MachineStateError;
use crate::mock_ssh_server;
use crate::mock_ssh_server::{MockSshServerHandle, PromptBehavior};

/// BmcMockWrapper launches a single instance of bmc-mock, configured to mock a single BMC for
/// either a DPU or a Host. It will rewrite certain responses to customize them for the machines
/// machine-a-tron is mocking.
pub(super) struct BmcMockWrapper {
    app_context: Arc<MachineATronContext>,
    bmc_mock_router: Router,
    bmc_mock_state: BmcState,
    hostname: Arc<dyn HostnameQuerying>,
    needs_ipmi_console: bool,
    requires_ssh_console: bool,
    stable_id: String,
    ssh_prompt_behavior: PromptBehavior,
}

impl BmcMockWrapper {
    pub(super) fn new(
        machine_info: &MachineInfo,
        app_context: Arc<MachineATronContext>,
        callbacks: Arc<dyn Callbacks>,
        hostname: Arc<dyn HostnameQuerying>,
        host_id: Uuid,
        injection: Arc<InjectionStore>,
        bmc_reset: Option<std::time::Duration>,
    ) -> Self {
        let (bmc_mock_router, bmc_mock_state) = bmc_mock::machine_router_with_injection_store(
            machine_info,
            callbacks,
            host_id.to_string(),
            true,
            injection,
            bmc_mock::MachineRouterOptions {
                bmc_reset_duration: bmc_reset,
                ..Default::default()
            },
        );

        let (ssh_prompt_behavior, requires_ssh_console) = match machine_info {
            MachineInfo::Dpu(_) => (PromptBehavior::Dpu, true),
            MachineInfo::Host(host) => match host.hw_type {
                HardwareType::DellPowerEdgeR750 | HardwareType::DellPowerEdgeR760Bf4 => {
                    (PromptBehavior::Dell, true)
                }
                HardwareType::LenovoGB300Nvl => (PromptBehavior::LenovoAmi, true),
                HardwareType::HpeProliantDl380aGen11 => (PromptBehavior::Hpe, true),
                _ => (PromptBehavior::Dell, false),
            },
        };

        BmcMockWrapper {
            app_context,
            bmc_mock_router,
            bmc_mock_state,
            hostname,
            needs_ipmi_console: machine_info.needs_ipmi_console(),
            requires_ssh_console,
            stable_id: host_id.to_string(),
            ssh_prompt_behavior,
        }
    }

    /// Starts per-machine console simulators when Redfish is served by a combined BMC mock.
    /// Returns `None` when no simulator is enabled for the hardware profile.
    pub(super) async fn start(&self) -> Result<Option<BmcMockWrapperHandle>, MachineStateError> {
        let ssh_handle = if self.app_context.app_config.mock_bmc_ssh_server
            && (self.requires_ssh_console || self.bmc_mock_state.has_enabled_ssh_serial_console())
        {
            Some(
                mock_ssh_server::spawn(
                    None,
                    self.hostname.clone(),
                    None,
                    self.ssh_prompt_behavior,
                    self.app_context.app_config.generate_console_logs,
                )
                .await
                .map_err(|error| MachineStateError::MockSshServer(error.to_string()))?,
            )
        } else {
            None
        };
        let ipmi_sim_handle = if self.need_ipmi_sim() {
            Some(self.start_ipmi_sim().await?)
        } else {
            None
        };
        let ssh_endpoint_port = ssh_handle.as_ref().map(|handle| handle.port);
        if let Some(port) = ssh_endpoint_port
            && !self.bmc_mock_state.set_serial_console_ssh_port(Some(port))
        {
            self.bmc_mock_state
                .set_simulated_serial_console_ssh_port(Some(port));
        }

        Ok(
            (ipmi_sim_handle.is_some() || ssh_handle.is_some()).then_some(BmcMockWrapperHandle {
                _bmc_mock: None,
                ssh_handle,
                ssh_endpoint_port,
                _ipmi_sim_handle: ipmi_sim_handle,
            }),
        )
    }

    async fn start_ipmi_sim(&self) -> Result<IpmiSimHandle, MachineStateError> {
        let console_prompt = format!("root@{} # ", self.hostname.get_hostname());
        bmc_mock::ipmi_sim::start(
            &self.bmc_mock_state,
            IpmiSimConfig {
                stable_id: self.stable_id.clone(),
                console_prompt,
            },
        )
        .await
        .map_err(MachineStateError::IpmiSim)
    }

    pub(super) fn router(&self) -> &Router {
        &self.bmc_mock_router
    }

    pub(super) fn state(&self) -> &BmcState {
        &self.bmc_mock_state
    }

    fn need_ipmi_sim(&self) -> bool {
        self.app_context.app_config.enable_ipmi_simulation && self.needs_ipmi_console
    }
}

#[derive(Debug)]
pub(super) struct BmcMockWrapperHandle {
    _bmc_mock: Option<CombinedServer>,
    pub(super) ssh_handle: Option<MockSshServerHandle>,
    ssh_endpoint_port: Option<u16>,
    _ipmi_sim_handle: Option<IpmiSimHandle>,
}

impl BmcMockWrapperHandle {
    pub(super) fn ipmi_port(&self) -> Option<u16> {
        self._ipmi_sim_handle.as_ref().map(|handle| handle.port)
    }

    pub(super) fn ssh_endpoint_port(&self) -> Option<u16> {
        self.ssh_endpoint_port
    }
}

/// BmcMockRegistry is shared state that MachineATron's mock hosts can use to register their BMC
/// mock routers, so that a single shared instance of BMC mock can delegate to them.
pub type BmcMockRegistry = Arc<RwLock<HashMap<String, Router>>>;
