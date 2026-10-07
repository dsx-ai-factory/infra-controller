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
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbide_redfish::boot_interface::BootInterfaceTarget;
use libredfish::model::service_root::RedfishVendor;
use libredfish::{PowerState, RoleId, SystemPowerControl};
use model::expected_entity::ExpectedEntity;
use model::machine::MachineInterfaceSnapshot;
use model::site_explorer::{
    BlueFieldOperatingMode, EndpointExplorationError, EndpointExplorationReport,
    InternalLockdownStatus, LockdownStatus,
};
use tokio::sync::Notify;

use crate::{AuthenticatedBmc, EndpointExplorer, SiteExplorationMetrics};

/// One recorded endpoint exploration and its boot-interface target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointExplorationCall {
    pub ip_address: IpAddr,
    pub boot_interface: Option<BootInterfaceTarget>,
}

#[derive(Clone)]
pub struct MockEndpointExplorationBlocker {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

impl MockEndpointExplorationBlocker {
    pub async fn wait_until_started(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.started.notified())
            .await
            .expect("timed out waiting for endpoint exploration to start");
    }

    pub fn release(&self) {
        self.release.notify_one();
    }
}

/// EndpointExplorer which returns predefined data.
///
/// `explore_endpoint` is always served from injected [`reports`]: in tests a
/// real explorer explores via the nv-redfish pool, which has no `RedfishSim`
/// behind it. A real explorer's `machine_setup`/`set_boot_order_dpu_first`, by
/// contrast, run on its libredfish pool (the `RedfishSim`), so a test backing
/// the API with this mock can attach one via [`Self::with_redfish_backend`] to
/// forward those two calls and keep `RedfishSim` assertions working.
///
/// [`reports`]: Self::reports
#[derive(Clone)]
pub struct MockEndpointExplorer {
    pub reports:
        Arc<Mutex<HashMap<IpAddr, Result<EndpointExplorationReport, EndpointExplorationError>>>>,
    pub precondition_result: Arc<Mutex<Result<(), EndpointExplorationError>>>,
    pub power_states: Arc<Mutex<HashMap<IpAddr, PowerState>>>,
    pub redfish_power_control_calls: Arc<Mutex<Vec<(SocketAddr, SystemPowerControl)>>>,
    /// Records every `redfish_chassis_reset` call (BMC address, chassis id, and
    /// requested action) so tests can assert the reset was routed correctly.
    pub redfish_chassis_reset_calls: Arc<Mutex<Vec<(SocketAddr, String, SystemPowerControl)>>>,
    /// Power-control actions that `redfish_power_control` should reject (the
    /// call is still recorded). Lets tests exercise the PowerCycle ->
    /// ACPowercycle fallback for a vendor that refuses `PowerCycle`.
    pub power_control_failures: Arc<Mutex<Vec<SystemPowerControl>>>,
    /// Records every call to `set_nic_mode` (BMC address + requested target
    /// mode) so tests can assert the auto-correct path fired with the
    /// right arguments.
    pub set_nic_mode_calls: Arc<Mutex<Vec<(SocketAddr, BlueFieldOperatingMode)>>>,
    /// Records each call to `explore_endpoint`.
    pub explore_endpoint_calls: Arc<Mutex<Vec<EndpointExplorationCall>>>,
    next_exploration_blocker: Arc<Mutex<Option<MockEndpointExplorationBlocker>>>,
    /// Authenticated BMC client that `machine_setup`/`set_boot_order_dpu_first`
    /// forward to (see [`Self::with_redfish_backend`]); `None` for the pure
    /// in-memory mock used by site-explorer's own tests.
    redfish_backend: Option<Arc<dyn AuthenticatedBmc>>,
}

impl Default for MockEndpointExplorer {
    fn default() -> Self {
        Self {
            reports: Arc::default(),
            precondition_result: Arc::new(Mutex::new(Ok(()))),
            power_states: Arc::default(),
            redfish_power_control_calls: Arc::default(),
            redfish_chassis_reset_calls: Arc::default(),
            power_control_failures: Arc::default(),
            set_nic_mode_calls: Arc::default(),
            explore_endpoint_calls: Arc::default(),
            next_exploration_blocker: Arc::default(),
            redfish_backend: None,
        }
    }
}

impl MockEndpointExplorer {
    pub fn explore_endpoint_call_count(&self) -> usize {
        self.explore_endpoint_calls.lock().unwrap().len()
    }

    pub fn block_next_exploration(&self) -> MockEndpointExplorationBlocker {
        let blocker = MockEndpointExplorationBlocker {
            started: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        };
        *self.next_exploration_blocker.lock().unwrap() = Some(blocker.clone());
        blocker
    }

    /// Make `redfish_power_control` reject the given action, so tests can
    /// simulate a vendor that refuses `PowerCycle`.
    pub fn fail_power_control(&self, action: SystemPowerControl) {
        self.power_control_failures.lock().unwrap().push(action);
    }

    pub fn insert_endpoints(&self, endpoints: Vec<(IpAddr, EndpointExplorationReport)>) {
        self.insert_endpoint_results(
            endpoints
                .into_iter()
                .map(|(address, report)| (address, Ok(report)))
                .collect(),
        )
    }

    pub fn insert_endpoint_result(
        &self,
        address: IpAddr,
        result: Result<EndpointExplorationReport, EndpointExplorationError>,
    ) {
        self.insert_endpoint_results(vec![(address, result)]);
    }

    pub fn insert_endpoint_results(
        &self,
        endpoints: Vec<(
            IpAddr,
            Result<EndpointExplorationReport, EndpointExplorationError>,
        )>,
    ) {
        let mut guard = self.reports.lock().unwrap();
        for (address, result) in endpoints {
            guard.insert(address, result);
        }
    }

    pub fn set_precondition_result(&self, result: Result<(), EndpointExplorationError>) {
        *self.precondition_result.lock().unwrap() = result;
    }

    /// Forward `machine_setup`/`set_boot_order_dpu_first` to `backend` (a real,
    /// `RedfishSim`-backed authenticated BMC client) instead of no-op'ing them;
    /// see the type docs.
    pub fn with_redfish_backend(mut self, backend: Arc<dyn AuthenticatedBmc>) -> Self {
        self.redfish_backend = Some(backend);
        self
    }
}

#[async_trait::async_trait]
impl EndpointExplorer for MockEndpointExplorer {
    async fn check_preconditions(
        &self,
        _metrics: &mut SiteExplorationMetrics,
    ) -> Result<(), EndpointExplorationError> {
        self.precondition_result.lock().unwrap().clone()
    }

    async fn explore_endpoint(
        &self,
        bmc_ip_address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
        _expected: Option<&ExpectedEntity>,
        _last_error: Option<&EndpointExplorationError>,
        boot_interface: Option<&BootInterfaceTarget>,
    ) -> Result<EndpointExplorationReport, EndpointExplorationError> {
        tracing::info!(%bmc_ip_address, "Endpoint is getting explored");
        self.explore_endpoint_calls
            .lock()
            .unwrap()
            .push(EndpointExplorationCall {
                ip_address: bmc_ip_address.ip(),
                boot_interface: boot_interface.cloned(),
            });
        let blocker = self.next_exploration_blocker.lock().unwrap().take();
        if let Some(blocker) = blocker {
            blocker.started.notify_one();
            blocker.release.notified().await;
        }
        let guard = self.reports.lock().unwrap();
        let res = guard.get(&bmc_ip_address.ip()).unwrap_or_else(|| {
            panic!(
                "MockEndpointExplorer has no report for {}; registered: {:?}",
                bmc_ip_address.ip(),
                guard.keys().collect::<Vec<_>>()
            )
        });
        res.clone()
    }
}

#[async_trait::async_trait]
impl AuthenticatedBmc for MockEndpointExplorer {
    async fn redfish_reset_bmc(
        &self,
        _address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
        _reset_type: Option<libredfish::ManagerResetType>,
    ) -> Result<(), EndpointExplorationError> {
        Ok(())
    }

    async fn ipmitool_reset_bmc(
        &self,
        _address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError> {
        Ok(())
    }

    async fn redfish_get_power_state(
        &self,
        address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
    ) -> Result<PowerState, EndpointExplorationError> {
        Ok(self
            .power_states
            .lock()
            .unwrap()
            .get(&address.ip())
            .copied()
            .unwrap_or(PowerState::On))
    }

    async fn redfish_power_control(
        &self,
        address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
        action: SystemPowerControl,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_power_control_calls
            .lock()
            .unwrap()
            .push((address, action));
        if self
            .power_control_failures
            .lock()
            .unwrap()
            .contains(&action)
        {
            return Err(EndpointExplorationError::Unreachable {
                details: Some(format!("mock: {action:?} refused")),
            });
        }
        Ok(())
    }

    async fn redfish_chassis_reset(
        &self,
        address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
        chassis_id: &str,
        action: SystemPowerControl,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_chassis_reset_calls.lock().unwrap().push((
            address,
            chassis_id.to_string(),
            action,
        ));
        Ok(())
    }

    async fn have_credentials(&self, _interface: &MachineInterfaceSnapshot) -> bool {
        true
    }

    async fn disable_secure_boot(
        &self,
        _address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError> {
        Ok(())
    }

    async fn lockdown(
        &self,
        _address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
        _action: libredfish::EnabledDisabled,
    ) -> Result<(), EndpointExplorationError> {
        Ok(())
    }

    async fn lockdown_status(
        &self,
        _address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
    ) -> Result<LockdownStatus, EndpointExplorationError> {
        Ok(LockdownStatus {
            status: InternalLockdownStatus::Disabled,
            message: String::new(),
        })
    }

    async fn machine_setup(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        boot_interface: Option<&carbide_redfish::boot_interface::BootInterfaceTarget>,
    ) -> Result<(), EndpointExplorationError> {
        match &self.redfish_backend {
            Some(backend) => {
                backend
                    .machine_setup(address, interface, boot_interface)
                    .await
            }
            None => Ok(()),
        }
    }

    async fn set_boot_order_dpu_first(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        boot_interface: &carbide_redfish::boot_interface::BootInterfaceTarget,
    ) -> Result<(), EndpointExplorationError> {
        match &self.redfish_backend {
            Some(backend) => {
                backend
                    .set_boot_order_dpu_first(address, interface, boot_interface)
                    .await
            }
            None => Ok(()),
        }
    }

    async fn set_nic_mode(
        &self,
        address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
        mode: BlueFieldOperatingMode,
    ) -> Result<(), EndpointExplorationError> {
        self.set_nic_mode_calls
            .lock()
            .unwrap()
            .push((address, mode));
        Ok(())
    }

    async fn is_viking(
        &self,
        _bmc_ip_address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
    ) -> Result<bool, EndpointExplorationError> {
        Ok(false)
    }

    async fn clear_nvram(
        &self,
        _bmc_ip_address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError> {
        Ok(())
    }

    async fn create_bmc_user(
        &self,
        _address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
        _username: &str,
        _password: &str,
        _role_id: RoleId,
    ) -> Result<(), EndpointExplorationError> {
        Ok(())
    }

    async fn delete_bmc_user(
        &self,
        _address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
        _username: &str,
    ) -> Result<(), EndpointExplorationError> {
        Ok(())
    }

    async fn set_bmc_root_password(
        &self,
        _address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
        _new_password: &str,
    ) -> Result<(), EndpointExplorationError> {
        Ok(())
    }

    async fn probe_bmc_vendor(
        &self,
        _address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
    ) -> Result<RedfishVendor, EndpointExplorationError> {
        Ok(RedfishVendor::Unknown)
    }

    async fn enable_infinite_boot(
        &self,
        _address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError> {
        Ok(())
    }

    async fn is_infinite_boot_enabled(
        &self,
        _address: SocketAddr,
        _interface: &MachineInterfaceSnapshot,
    ) -> Result<Option<bool>, EndpointExplorationError> {
        Ok(None)
    }
}
