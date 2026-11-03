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
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use bmc_mock::injection::InjectionStore;
use bmc_mock::ipmi_sim::{ConsoleOutputStreamFactory, IpmiSimConfig, IpmiSimHandle};
use bmc_mock::{
    ActionError, BmcState, Callbacks, CombinedServer, DeviceEffect, HardwareType, HostnameQuerying,
    MachineInfo, ResourceResetType,
};
use futures::FutureExt;
use tokio::sync::RwLock;
use tokio::time::Instant;
use tokio_util::task::AbortOnDropHandle;
use uuid::Uuid;

use crate::config::MachineATronContext;
use crate::console_output::ConsoleOutputController;
use crate::machine_state_machine::MachineStateError;
use crate::mock_ssh_server;
use crate::mock_ssh_server::{MockSshServerHandle, PromptBehavior};

/// Snapshot access and startup fallback for an embedded BMC. No independent file writer.
#[derive(Debug, Default)]
pub(super) struct BmcPersistence {
    saved: Option<bmc_mock::persistence::PersistedBmcState>,
    source: Option<bmc_mock::persistence::BmcSnapshotSource>,
}

impl BmcPersistence {
    pub(super) fn from_saved(saved: Option<bmc_mock::persistence::PersistedBmcState>) -> Self {
        Self {
            saved,
            source: None,
        }
    }

    pub(super) fn persisted(&self) -> Option<bmc_mock::persistence::PersistedBmcState> {
        self.source
            .as_ref()
            .and_then(|source| source.persisted())
            .or_else(|| self.saved.clone())
    }

    pub(super) fn effect_snapshot(&self) -> Option<bmc_mock::persistence::BmcEffectSnapshot> {
        self.source.as_ref()?.effect_snapshot()
    }

    pub(super) fn restore<C: Callbacks>(
        &self,
        state: &BmcState<C>,
    ) -> Result<(), bmc_mock::persistence::PersistenceError> {
        if let Some(snapshot) = self.persisted() {
            state.restore_persisted(&snapshot)?;
        }
        Ok(())
    }

    pub(super) fn attach<C: Callbacks>(&mut self, state: &BmcState<C>) {
        self.saved = Some(state.persisted());
        self.source = Some(state.snapshot_source());
    }
}

#[derive(Debug)]
pub(crate) enum BmcCommand {
    SetSystemPower {
        request: ResourceResetType,
        reply: Option<tokio::sync::oneshot::Sender<Result<(), ActionError>>>,
    },
    /// A physical BMC action. Factory reset waits for state publication and DHCP
    /// retry admission; chassis power effects retain asynchronous acceptance.
    DeviceEffect {
        effect: DeviceEffect,
        reply: Option<tokio::sync::oneshot::Sender<Result<(), ActionError>>>,
    },
    StateRefreshIndication,
}

/// Actor-owned DHCP recovery and lease renewal, independent of host boot and NVOS state machines.
/// At most one renewal task runs. Only the owning actor publishes its result.
#[derive(Debug)]
pub(super) struct BmcDhcpRecovery {
    owner: Uuid,
    router: Option<Router>,
    mounted_ip: Option<Ipv4Addr>,
    deadline: Option<Instant>,
    retry: Option<crate::dhcp_retry_fsm::DhcpRetryFsm>,
    renewal_interval: Option<Duration>,
    renewal: Option<BmcDhcpRenewal>,
    operation: BmcDhcpOperation,
}

/// The runtime drives I/O even when the actor pauses or waits for another API call.
/// Dropping the actor's handle aborts the request without any route effects.
struct BmcDhcpRenewal {
    expires_at: Instant,
    task: AbortOnDropHandle<eyre::Result<crate::dhcp_wrapper::DhcpResponseInfo>>,
}

impl std::fmt::Debug for BmcDhcpRenewal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BmcDhcpRenewal")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum BmcDhcpOperation {
    Reset,
    Renew,
}

impl Default for BmcDhcpRecovery {
    fn default() -> Self {
        Self {
            owner: Uuid::new_v4(),
            router: None,
            mounted_ip: None,
            deadline: None,
            retry: None,
            renewal_interval: None,
            renewal: None,
            operation: BmcDhcpOperation::Reset,
        }
    }
}

impl BmcDhcpRecovery {
    pub(super) fn with_renewal(interval: Duration) -> Self {
        Self {
            renewal_interval: (!interval.is_zero()).then_some(interval),
            ..Self::default()
        }
    }

    fn schedule_renewal(&mut self) {
        self.operation = BmcDhcpOperation::Renew;
        self.deadline = self
            .renewal_interval
            .map(|interval| Instant::now() + crate::run_jitter::jitter_interval(interval));
    }

    pub(super) async fn mount(&mut self, registry: &BmcMockRegistry, ip: Ipv4Addr, router: Router) {
        registry
            .mount(self.owner, self.mounted_ip.replace(ip), ip, router.clone())
            .await;
        self.router = Some(router);
        if self.deadline.is_none() {
            self.operation = BmcDhcpOperation::Renew;
            self.deadline = self
                .renewal_interval
                .map(|interval| Instant::now() + crate::run_jitter::first_run_offset(interval));
        }
    }

    pub(super) async fn reset(
        &mut self,
        update: bmc_mock::BmcResetUpdate,
    ) -> Result<(), ActionError> {
        if self.router.is_none() {
            return Err(ActionError::Internal(eyre::eyre!(
                "BMC router is not mounted"
            )));
        }
        let outage = update.commit().await?;
        // The pre-reset lease attempt must never publish a late offer after reset.
        self.renewal = None;
        self.deadline = Some(Instant::now() + outage);
        self.retry = None;
        self.operation = BmcDhcpOperation::Reset;
        Ok(())
    }

    pub(super) fn next_attempt_in(&self) -> Duration {
        if self.renewal.is_some() {
            // Cap the next actor sleep at 100ms, leaving commands responsive between polls.
            return Duration::from_millis(100);
        }
        self.deadline.map_or(Duration::MAX, |deadline| {
            deadline.saturating_duration_since(Instant::now())
        })
    }

    /// One bounded attempt. Suppression, timeout and transport failures keep recovery
    /// pending with the shared RFC 2131 backoff (4s to 64s, plus/minus 1s jitter).
    /// The actor owns retries
    /// after the HTTP caller leaves; each API/UDP request is limited to ten seconds.
    pub(super) async fn advance(
        &mut self,
        registry: &BmcMockRegistry,
        client: &crate::dhcp_wrapper::DhcpClient,
        request: crate::dhcp_wrapper::DhcpRequestInfo,
    ) -> Option<crate::dhcp_wrapper::DhcpResponseInfo> {
        if self.renewal.is_none() && (self.deadline.is_none() || !self.next_attempt_in().is_zero())
        {
            return None;
        }
        // Factory reset abandons its lease before asking again. A renewal keeps
        // its current address until a successful response authorizes rekeying.
        if self.operation == BmcDhcpOperation::Reset
            && let Some(previous) = self.mounted_ip
        {
            registry.unmount(self.owner, previous).await;
            self.mounted_ip = None;
        }
        use crate::dhcp_retry_fsm::{Action, DhcpRetryFsm, Event};
        let retry = self
            .retry
            .unwrap_or_else(DhcpRetryFsm::new)
            .event(Event::TimerExpired)
            .0;
        let (result, expires_at) = if self.operation == BmcDhcpOperation::Renew {
            let pending = self.renewal.get_or_insert_with(|| {
                let client = client.clone();
                let expires_at = Instant::now() + Duration::from_secs(10);
                BmcDhcpRenewal {
                    expires_at,
                    task: AbortOnDropHandle::new(tokio::spawn(async move {
                        // Check the deadline first on every wake, including after a late
                        // connection-lock grant or UDP response. I/O never publishes routes.
                        tokio::select! {
                            biased;
                            _ = tokio::time::sleep_until(expires_at) => {
                                Err(eyre::eyre!("BMC DHCP renewal attempt expired"))
                            }
                            result = client.request_ip(request) => Ok(result?),
                        }
                    })),
                }
            });
            let expires_at = pending.expires_at;
            // A completed result can wait while the actor is paused. Its original
            // deadline still applies when the actor resumes and publishes the route.
            let result = if Instant::now() >= expires_at {
                Err(eyre::eyre!("BMC DHCP renewal attempt expired"))
            } else {
                match (&mut pending.task).now_or_never()? {
                    Ok(result) => result,
                    Err(error) => Err(error.into()),
                }
            };
            self.renewal = None;
            (result, Some(expires_at))
        } else {
            let result =
                match tokio::time::timeout(Duration::from_secs(10), client.request_ip(request))
                    .await
                {
                    Ok(result) => result.map_err(eyre::Report::from),
                    Err(error) => Err(error.into()),
                };
            (result, None)
        };
        let result = match result {
            Ok(info) => {
                let owns_route = registry
                    .owners
                    .lock()
                    .expect("route ownership mutex poisoned")
                    .get(&info.ip_address.to_string())
                    == Some(&self.owner);
                if expires_at.is_some_and(|deadline| Instant::now() >= deadline) {
                    Err(eyre::eyre!("BMC DHCP renewal attempt expired"))
                } else if self.mounted_ip != Some(info.ip_address) || !owns_route {
                    let router = self.router.clone().expect("DHCP requires a mounted BMC");
                    if registry
                        .mount_before(
                            self.owner,
                            self.mounted_ip,
                            info.ip_address,
                            router,
                            expires_at,
                        )
                        .await
                    {
                        self.mounted_ip = Some(info.ip_address);
                        Ok(info)
                    } else {
                        Err(eyre::eyre!("BMC DHCP renewal attempt expired"))
                    }
                } else {
                    Ok(info)
                }
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(info) => {
                self.retry = None;
                self.schedule_renewal();
                Some(info)
            }
            result => {
                let (retry, actions) = retry.event(Event::Failed(
                    crate::dhcp_retry_runtime::sample_dhcp_retry_jitter(),
                ));
                self.retry = Some(retry);
                for action in actions {
                    if let Action::Schedule { delay } = action {
                        self.deadline = Some(Instant::now() + delay);
                        tracing::debug!(?result, ?delay, "BMC DHCP recovery will retry");
                    }
                }
                None
            }
        }
    }
}

/// BmcMockWrapper launches a single instance of bmc-mock, configured to mock a single BMC for
/// either a DPU or a Host. It will rewrite certain responses to customize them for the machines
/// machine-a-tron is mocking.
pub(super) struct BmcMockWrapper<C: Callbacks> {
    app_context: Arc<MachineATronContext>,
    bmc_mock_router: Router,
    bmc_mock_state: BmcState<C>,
    hostname: Arc<dyn HostnameQuerying>,
    needs_ipmi_console: bool,
    requires_ssh_console: bool,
    stable_id: String,
    ssh_prompt_behavior: PromptBehavior,
}

impl<C: Callbacks> BmcMockWrapper<C> {
    pub(super) fn new(
        machine_info: &MachineInfo,
        app_context: Arc<MachineATronContext>,
        callbacks: Arc<C>,
        hostname: Arc<dyn HostnameQuerying>,
        host_id: Uuid,
        injection: Arc<InjectionStore>,
        timings: Option<&crate::lifecycle_timings::LifecycleTimings>,
    ) -> Self {
        let (bmc_mock_router, bmc_mock_state) = bmc_mock::machine_router_with_injection_store(
            machine_info,
            callbacks,
            host_id.to_string(),
            true,
            injection,
            bmc_mock::MachineRouterOptions {
                bmc_reset_duration: timings.map(|t| t.bmc_reset),
                firmware_upgrade_duration: timings.map(|t| t.firmware_upgrade),
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
        let console_output = ConsoleOutputController::new(self.stable_id.clone());
        let ssh_handle = if self.app_context.app_config.mock_bmc_ssh_server
            && (self.requires_ssh_console || self.bmc_mock_state.has_enabled_ssh_serial_console())
        {
            Some(
                mock_ssh_server::spawn(
                    None,
                    self.hostname.clone(),
                    None,
                    self.ssh_prompt_behavior,
                    Some(console_output.clone()),
                )
                .await
                .map_err(|error| MachineStateError::MockSshServer(error.to_string()))?,
            )
        } else {
            None
        };
        let ipmi_sim_handle = if self.need_ipmi_sim() {
            Some(self.start_ipmi_sim(&console_output).await?)
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
                console_output,
            }),
        )
    }

    async fn start_ipmi_sim(
        &self,
        console_output: &ConsoleOutputController,
    ) -> Result<IpmiSimHandle, MachineStateError> {
        let console_prompt = format!("root@{} # ", self.hostname.get_hostname());
        let console_output = console_output.clone();
        let console_output: ConsoleOutputStreamFactory = Box::new(move || console_output.stream());
        bmc_mock::ipmi_sim::start(
            &self.bmc_mock_state,
            IpmiSimConfig {
                stable_id: self.stable_id.clone(),
                console_prompt,
            },
            Some(console_output),
        )
        .await
        .map_err(MachineStateError::IpmiSim)
    }

    pub(super) fn router(&self) -> &Router {
        &self.bmc_mock_router
    }

    pub(super) fn state(&self) -> &BmcState<C> {
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
    console_output: ConsoleOutputController,
}

impl BmcMockWrapperHandle {
    pub(super) fn ipmi_port(&self) -> Option<u16> {
        self._ipmi_sim_handle.as_ref().map(|handle| handle.port)
    }

    pub(super) fn ssh_endpoint_port(&self) -> Option<u16> {
        self.ssh_endpoint_port
    }

    pub(super) fn console_output_start(&self) {
        self.console_output.start();
    }

    pub(super) fn console_output_stop(&self) {
        self.console_output.stop();
    }
}

/// BmcMockRegistry is shared state that MachineATron's mock hosts can use to register their BMC
/// mock routers, so that a single shared instance of BMC mock can delegate to them.
#[derive(Clone, Debug, Default)]
pub struct BmcMockRegistry {
    routes: Arc<RwLock<HashMap<String, Router>>>,
    owners: Arc<std::sync::Mutex<HashMap<String, Uuid>>>,
}

impl From<BmcMockRegistry> for Arc<RwLock<HashMap<String, Router>>> {
    fn from(registry: BmcMockRegistry) -> Self {
        registry.routes
    }
}

impl BmcMockRegistry {
    /// Inspects current routes without changing their ownership.
    pub async fn read(&self) -> tokio::sync::RwLockReadGuard<'_, HashMap<String, Router>> {
        self.routes.read().await
    }

    /// A successful DHCP lease authorizes replacing its destination. Removal of
    /// the former address is conditional: another actor may already own that lease.
    pub(super) async fn mount(
        &self,
        owner: Uuid,
        previous: Option<Ipv4Addr>,
        ip: Ipv4Addr,
        router: Router,
    ) {
        self.mount_before(owner, previous, ip, router, None).await;
    }

    /// Renewal publication must still be authorized after waiting for the route locks.
    async fn mount_before(
        &self,
        owner: Uuid,
        previous: Option<Ipv4Addr>,
        ip: Ipv4Addr,
        router: Router,
        expires_at: Option<Instant>,
    ) -> bool {
        let mut routes = if let Some(expires_at) = expires_at {
            match tokio::time::timeout_at(expires_at, self.routes.write()).await {
                Ok(routes) => routes,
                Err(_) => return false,
            }
        } else {
            self.routes.write().await
        };
        let mut owners = self.owners.lock().expect("route ownership mutex poisoned");
        if expires_at.is_some_and(|deadline| Instant::now() >= deadline) {
            return false;
        }
        if let Some(previous) = previous.filter(|previous| *previous != ip) {
            let key = previous.to_string();
            if owners.get(&key) == Some(&owner) {
                routes.remove(&key);
                owners.remove(&key);
            }
        }
        let key = ip.to_string();
        if let Some(previous_owner) = owners.insert(key.clone(), owner)
            && previous_owner != owner
        {
            tracing::debug!(%ip, %owner, %previous_owner, "DHCP lease transfers a simulated route");
        }
        routes.insert(key, router);
        true
    }

    pub(super) async fn unmount(&self, owner: Uuid, ip: Ipv4Addr) {
        let mut routes = self.routes.write().await;
        let mut owners = self.owners.lock().expect("route ownership mutex poisoned");
        let key = ip.to_string();
        if owners.get(&key) == Some(&owner) {
            owners.remove(&key);
            routes.remove(&key);
        }
    }
}

#[cfg(test)]
mod persistence_tests {
    use bmc_mock::mac_address_pool::{Config, MacAddressPool, PoolConfig};
    use mac_address::MacAddress;

    use super::*;

    fn state() -> BmcState<bmc_mock::test_support::TestCallbacks> {
        let base = MacAddress::new([2, 0, 0, 0, 0, 1]);
        let range = PoolConfig::new(base, 24).unwrap();
        let mut pool = MacAddressPool::new(Config {
            pool: Some(range),
            ranges: None,
        });
        let info = MachineInfo::Host(bmc_mock::HostMachineInfo::new(
            HardwareType::GenericAmi,
            Vec::new(),
            &mut pool,
            range,
        ));
        bmc_mock::machine_router(
            &info,
            Arc::new(bmc_mock::test_support::TestCallbacks::default()),
            "test".into(),
            false,
            Default::default(),
        )
        .1
    }

    #[test]
    fn snapshot_survives_an_unstarted_or_dropped_bmc() {
        let original = state();
        original
            .account_service_state
            .change_factory_default_password("saved-password");
        let snapshot = original.persisted();
        let mut holder = BmcPersistence::from_saved(Some(snapshot.clone()));
        assert_eq!(holder.persisted(), Some(snapshot.clone()));
        let restored = state();
        restored
            .account_service_state
            .change_factory_default_password("configured-password");
        holder.restore(&restored).unwrap();
        assert_eq!(restored.persisted(), snapshot);
        holder.attach(&restored);
        drop(restored);
        assert_eq!(holder.persisted(), Some(snapshot));
    }
}

#[cfg(test)]
mod recovery_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::Request;
    use bmc_mock::mac_address_pool::{Config, MacAddressPool, PoolConfig};
    use dhcproto::v4::relay::{RelayCode, RelayInfo};
    use dhcproto::v4::{
        Decodable, Decoder, DhcpOption, HType, Message, MessageType, Opcode, OptionCode,
    };
    use dhcproto::{Encodable, Encoder};
    use mac_address::MacAddress;
    use tokio::net::UdpSocket;
    use tokio::sync::{mpsc, oneshot};
    use tower::ServiceExt;

    use super::*;
    use crate::dhcp_wrapper::{DhcpClient, DhcpRequestInfo};
    use crate::dhcp_wrapper_udp::UdpDhcpClient;

    struct ResetCallbacks(
        mpsc::UnboundedSender<(DeviceEffect, oneshot::Sender<Result<(), ActionError>>)>,
    );

    impl Callbacks for ResetCallbacks {
        async fn computer_system_reset(&self, _: ResourceResetType) -> Result<(), ActionError> {
            Ok(())
        }
        async fn device_effect(&self, effect: DeviceEffect) -> Result<(), ActionError> {
            let (reply, response) = oneshot::channel();
            self.0.send((effect, reply)).unwrap();
            response.await.unwrap()
        }
        fn state_refresh_indication(&self) {}
    }

    async fn complete_attempt(
        recovery: &mut BmcDhcpRecovery,
        registry: &BmcMockRegistry,
        client: &DhcpClient,
        request: impl Fn() -> DhcpRequestInfo,
    ) -> Option<crate::dhcp_wrapper::DhcpResponseInfo> {
        tokio::time::timeout(Duration::from_secs(11), async {
            loop {
                let result = recovery.advance(registry, client, request()).await;
                if recovery.renewal.is_none() {
                    return result;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn rekey_and_unmount_preserve_a_reassigned_lease() {
        let registry = BmcMockRegistry::default();
        let mut first = BmcDhcpRecovery::default();
        let mut second = BmcDhcpRecovery::default();
        let old = Ipv4Addr::new(192, 0, 2, 10);
        let new = Ipv4Addr::new(192, 0, 2, 11);
        let first_router =
            Router::new().route("/identity", axum::routing::get(|| async { "first" }));
        let second_router =
            Router::new().route("/identity", axum::routing::get(|| async { "second" }));
        first.mount(&registry, old, first_router.clone()).await;
        assert_eq!(first.next_attempt_in(), Duration::MAX);
        // Core reassigned the released lease before the first BMC renewed.
        second.mount(&registry, old, second_router).await;
        first.mount(&registry, new, first_router).await;
        registry.unmount(first.owner, old).await;
        for (ip, expected) in [(old, "second"), (new, "first")] {
            let router = registry.read().await[&ip.to_string()].clone();
            let response = router
                .oneshot(Request::get("/identity").body(Body::empty()).unwrap())
                .await
                .unwrap();
            let body = axum::body::to_bytes(response.into_body(), 64)
                .await
                .unwrap();
            assert_eq!(body.as_ref(), expected.as_bytes());
        }
        registry.unmount(first.owner, new).await;
        assert!(registry.read().await.contains_key(&old.to_string()));
        assert!(!registry.read().await.contains_key(&new.to_string()));
    }

    #[tokio::test(start_paused = true)]
    async fn first_renewals_spread_across_the_whole_interval() {
        let registry = BmcMockRegistry::default();
        let interval = Duration::from_secs(900);
        let mut offsets = Vec::new();
        for _ in 0..1000 {
            let mut renewal = BmcDhcpRecovery::with_renewal(interval);
            renewal
                .mount(&registry, Ipv4Addr::LOCALHOST, Router::new())
                .await;
            offsets.push(renewal.next_attempt_in());
        }
        assert!(offsets.iter().all(|offset| *offset < interval));
        assert!(*offsets.iter().max().unwrap() - *offsets.iter().min().unwrap() > interval / 2);
    }

    #[tokio::test]
    async fn renewal_keeps_a_lease_on_failure_and_rekeys_only_on_a_new_address() {
        let registry = BmcMockRegistry::default();
        let interval = Duration::from_secs(900);
        let mut renewal = BmcDhcpRecovery::with_renewal(interval);
        assert_eq!(
            renewal.next_attempt_in(),
            Duration::MAX,
            "startup still owns initial DHCP"
        );
        let old_ip = Ipv4Addr::new(192, 0, 2, 10);
        let new_ip = Ipv4Addr::new(192, 0, 2, 11);
        let router = Router::new().route("/identity", axum::routing::get(|| async { "BMC" }));
        renewal.mount(&registry, old_ip, router).await;
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let std::net::SocketAddr::V4(server_address) = server.local_addr().unwrap() else {
            unreachable!()
        };
        let (client, service) = UdpDhcpClient::start(
            server_address,
            "127.0.0.1:0".parse().unwrap(),
            Ipv4Addr::LOCALHOST,
        )
        .await
        .unwrap();
        let client = DhcpClient::UdpRelay(client);
        let server_task = tokio::spawn(async move {
            let mut buffer = [0; 4096];
            for reply in [
                Some((MessageType::Offer, old_ip)),
                Some((MessageType::Ack, old_ip)),
                Some((MessageType::Nak, old_ip)),
                None, // A lost response must retain the route just like a rejection.
                Some((MessageType::Offer, new_ip)),
                Some((MessageType::Ack, new_ip)),
                Some((MessageType::Offer, new_ip)),
                Some((MessageType::Ack, new_ip)),
            ] {
                let (length, source) = server.recv_from(&mut buffer).await.unwrap();
                let request = Message::decode(&mut Decoder::new(&buffer[..length])).unwrap();
                let Some((kind, ip)) = reply else { continue };
                let mut response = Message::default();
                response
                    .set_opcode(Opcode::BootReply)
                    .set_htype(HType::Eth)
                    .set_xid(request.xid())
                    .set_chaddr(request.chaddr())
                    .set_giaddr(request.giaddr())
                    .set_yiaddr(ip);
                response.opts_mut().insert(DhcpOption::MessageType(kind));
                response
                    .opts_mut()
                    .insert(DhcpOption::ServerIdentifier(Ipv4Addr::LOCALHOST));
                let mut bytes = Vec::new();
                response.encode(&mut Encoder::new(&mut bytes)).unwrap();
                server.send_to(&bytes, source).await.unwrap();
            }
        });
        let request = || DhcpRequestInfo {
            mac_address: MacAddress::new([2, 0, 0, 0, 0, 1]),
            relay_address: Ipv4Addr::new(192, 0, 2, 1),
            vendor_class: Some("NVIDIA/BF/BMC"),
        };
        renewal.deadline = Some(Instant::now() + interval);
        assert!(
            renewal
                .advance(&registry, &client, request())
                .await
                .is_none()
        );
        let owner = registry.owners.lock().unwrap()[&old_ip.to_string()];
        renewal.deadline = Some(Instant::now());
        assert_eq!(
            complete_attempt(&mut renewal, &registry, &client, request)
                .await
                .unwrap()
                .ip_address,
            old_ip
        );
        assert_eq!(registry.owners.lock().unwrap()[&old_ip.to_string()], owner);
        assert!(renewal.next_attempt_in() > Duration::from_secs(800));

        for (failure, bounds) in [("server rejection", 2..=5), ("no response", 6..=9)] {
            renewal.deadline = Some(Instant::now());
            assert!(
                complete_attempt(&mut renewal, &registry, &client, request)
                    .await
                    .is_none(),
                "{failure}"
            );
            assert_eq!(renewal.mounted_ip, Some(old_ip), "{failure}");
            assert!(
                bounds.contains(&renewal.next_attempt_in().as_secs()),
                "{failure}"
            );
            let route = registry.read().await[&old_ip.to_string()].clone();
            let response = route
                .oneshot(Request::get("/identity").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                axum::body::to_bytes(response.into_body(), 64)
                    .await
                    .unwrap()
                    .as_ref(),
                b"BMC"
            );
        }
        renewal.deadline = Some(Instant::now());
        assert_eq!(
            complete_attempt(&mut renewal, &registry, &client, request)
                .await
                .unwrap()
                .ip_address,
            new_ip
        );
        assert_eq!(renewal.mounted_ip, Some(new_ip));
        assert!(renewal.retry.is_none());
        assert!(
            (Duration::from_secs(800)..=Duration::from_secs(990))
                .contains(&renewal.next_attempt_in())
        );
        let routes = registry.read().await;
        assert!(!routes.contains_key(&old_ip.to_string()));
        let route = routes[&new_ip.to_string()].clone();
        drop(routes);
        assert!(
            route
                .oneshot(Request::get("/identity").body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
                .is_success()
        );
        // A prior owner can still remember this IP after another actor claims
        // it. A fresh same-IP grant must restore the router authorized by Core.
        let mut other = BmcDhcpRecovery::default();
        other.mount(&registry, new_ip, Router::new()).await;
        renewal.deadline = Some(Instant::now());
        complete_attempt(&mut renewal, &registry, &client, request)
            .await
            .unwrap();
        assert_eq!(
            registry.owners.lock().unwrap()[&new_ip.to_string()],
            renewal.owner
        );
        let route = registry.read().await[&new_ip.to_string()].clone();
        assert!(
            route
                .oneshot(Request::get("/identity").body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
                .is_success()
        );
        server_task.await.unwrap();
        service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn factory_reset_supersedes_pending_and_unpolled_renewal_responses() {
        for queued_ack in [false, true] {
            let (commands, mut requests) = mpsc::unbounded_channel();
            let range = PoolConfig::new(MacAddress::new([2, 0, 0, 0, 3, 0]), 24).unwrap();
            let mut pool = MacAddressPool::new(Config {
                pool: Some(range),
                ranges: None,
            });
            let machine = MachineInfo::Host(bmc_mock::HostMachineInfo::new(
                HardwareType::GenericAmi,
                Vec::new(),
                &mut pool,
                range,
            ));
            let (router, state) = bmc_mock::machine_router(
                &machine,
                Arc::new(ResetCallbacks(commands)),
                "pending-renewal".into(),
                false,
                Default::default(),
            );
            let registry = BmcMockRegistry::default();
            let old_ip = Ipv4Addr::new(192, 0, 2, 10);
            let stale_ip = Ipv4Addr::new(192, 0, 2, 11);
            let mut recovery = BmcDhcpRecovery::with_renewal(Duration::from_secs(900));
            recovery.mount(&registry, old_ip, router.clone()).await;
            recovery.deadline = Some(Instant::now());

            let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let std::net::SocketAddr::V4(server_address) = server.local_addr().unwrap() else {
                unreachable!()
            };
            let (client, service) = UdpDhcpClient::start(
                server_address,
                "127.0.0.1:0".parse().unwrap(),
                Ipv4Addr::LOCALHOST,
            )
            .await
            .unwrap();
            let client = DhcpClient::UdpRelay(client);
            let (waiting, mut ack_pending) = oneshot::channel();
            let (release, released) = oneshot::channel();
            let (sent, ack_sent) = oneshot::channel();
            let server_task = tokio::spawn(async move {
                let mut waiting = Some(waiting);
                let mut released = Some(released);
                let mut sent = Some(sent);
                let mut buffer = [0; 4096];
                for kind in [MessageType::Offer, MessageType::Ack, MessageType::Nak] {
                    let (length, source) = server.recv_from(&mut buffer).await.unwrap();
                    let request = Message::decode(&mut Decoder::new(&buffer[..length])).unwrap();
                    if kind == MessageType::Ack {
                        waiting.take().unwrap().send(()).unwrap();
                        released.take().unwrap().await.unwrap();
                    }
                    let mut response = Message::default();
                    response
                        .set_opcode(Opcode::BootReply)
                        .set_htype(HType::Eth)
                        .set_xid(request.xid())
                        .set_chaddr(request.chaddr())
                        .set_giaddr(request.giaddr())
                        .set_yiaddr(stale_ip);
                    response.opts_mut().insert(DhcpOption::MessageType(kind));
                    response
                        .opts_mut()
                        .insert(DhcpOption::ServerIdentifier(Ipv4Addr::LOCALHOST));
                    let mut bytes = Vec::new();
                    response.encode(&mut Encoder::new(&mut bytes)).unwrap();
                    server.send_to(&bytes, source).await.unwrap();
                    if kind == MessageType::Ack {
                        sent.take().unwrap().send(()).unwrap();
                    }
                }
            });
            let request = || DhcpRequestInfo {
                mac_address: machine.bmc_mac_address(),
                relay_address: Ipv4Addr::new(192, 0, 2, 1),
                vendor_class: Some("NVIDIA/BF/BMC"),
            };
            // Both DHCP phases return control to the actor while waiting for the server.
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    assert!(
                        recovery
                            .advance(&registry, &client, request())
                            .await
                            .is_none()
                    );
                    if ack_pending.try_recv().is_ok() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(recovery.next_attempt_in(), Duration::from_millis(100));
            let mut release = Some(release);
            let mut ack_sent = Some(ack_sent);
            if queued_ack {
                release.take().unwrap().send(()).unwrap();
                ack_sent.take().unwrap().await.unwrap();
                // No actor poll (as during pause): a ready reply cannot publish a route.
                assert_eq!(recovery.mounted_ip, Some(old_ip));
                assert!(!registry.read().await.contains_key(&stale_ip.to_string()));
            }
            let reset = Request::post("/redfish/v1/Managers/Self/Actions/Manager.ResetToDefaults")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"ResetType":"ResetAll"}"#))
                .unwrap();
            let response = tokio::spawn(router.oneshot(reset));
            tokio::time::timeout(Duration::from_millis(250), async {
                let (DeviceEffect::ResetBmcToDefaults(update), reply) =
                    requests.recv().await.unwrap()
                else {
                    panic!("expected BMC factory reset")
                };
                reply.send(recovery.reset(update).await).unwrap();
                assert!(response.await.unwrap().unwrap().status().is_success());
            })
            .await
            .unwrap();
            assert_eq!(state.factory_reset_generation(), 1);
            assert!(recovery.renewal.is_none());
            assert_eq!(recovery.operation, BmcDhcpOperation::Reset);
            if !queued_ack {
                release.take().unwrap().send(()).unwrap();
                ack_sent.take().unwrap().await.unwrap();
            }
            recovery.deadline = Some(Instant::now());
            assert!(
                recovery
                    .advance(&registry, &client, request())
                    .await
                    .is_none()
            );
            assert!(
                registry.read().await.is_empty(),
                "stale ACK remounted a route"
            );
            assert!(recovery.retry.is_some());
            server_task.await.unwrap();
            service.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn expired_renewal_rejects_late_udp_replies_and_preserves_a_new_owner() {
        async fn respond(
            server: &UdpSocket,
            request: &Message,
            source: std::net::SocketAddr,
            kind: MessageType,
            ip: Ipv4Addr,
        ) {
            let mut response = Message::default();
            response
                .set_opcode(Opcode::BootReply)
                .set_htype(HType::Eth)
                .set_xid(request.xid())
                .set_chaddr(request.chaddr())
                .set_giaddr(request.giaddr())
                .set_yiaddr(ip);
            response.opts_mut().insert(DhcpOption::MessageType(kind));
            response
                .opts_mut()
                .insert(DhcpOption::ServerIdentifier(Ipv4Addr::LOCALHOST));
            let mut bytes = Vec::new();
            response.encode(&mut Encoder::new(&mut bytes)).unwrap();
            server.send_to(&bytes, source).await.unwrap();
        }

        for delayed_reply in [MessageType::Offer, MessageType::Ack] {
            let registry = BmcMockRegistry::default();
            let old_ip = Ipv4Addr::new(192, 0, 2, 10);
            let granted_ip = Ipv4Addr::new(192, 0, 2, 11);
            let mut recovery = BmcDhcpRecovery::with_renewal(Duration::from_secs(900));
            recovery.mount(&registry, old_ip, Router::new()).await;
            recovery.deadline = Some(Instant::now());
            let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let std::net::SocketAddr::V4(server_address) = server.local_addr().unwrap() else {
                unreachable!()
            };
            let (client, service) = UdpDhcpClient::start(
                server_address,
                "127.0.0.1:0".parse().unwrap(),
                Ipv4Addr::LOCALHOST,
            )
            .await
            .unwrap();
            let client = DhcpClient::UdpRelay(client);
            let request = || DhcpRequestInfo {
                mac_address: MacAddress::new([2, 0, 0, 0, 4, 0]),
                relay_address: Ipv4Addr::new(192, 0, 2, 1),
                vendor_class: Some("NVIDIA/BF/BMC"),
            };
            let (pending_request, source) = tokio::time::timeout(Duration::from_secs(1), async {
                let mut buffer = [0; 4096];
                loop {
                    assert!(
                        recovery
                            .advance(&registry, &client, request())
                            .await
                            .is_none()
                    );
                    match server.try_recv_from(&mut buffer) {
                        Ok((length, source)) => {
                            let message =
                                Message::decode(&mut Decoder::new(&buffer[..length])).unwrap();
                            if delayed_reply == MessageType::Ack
                                && message.opts().get(OptionCode::MessageType)
                                    == Some(&DhcpOption::MessageType(MessageType::Discover))
                            {
                                respond(&server, &message, source, MessageType::Offer, granted_ip)
                                    .await;
                            } else {
                                return (message, source);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) => panic!("failed to receive renewal request: {error}"),
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            let expected_request = if delayed_reply == MessageType::Offer {
                MessageType::Discover
            } else {
                MessageType::Request
            };
            assert_eq!(
                pending_request.opts().get(OptionCode::MessageType),
                Some(&DhcpOption::MessageType(expected_request))
            );

            let mut newer = BmcDhcpRecovery::default();
            newer
                .mount(
                    &registry,
                    granted_ip,
                    Router::new().route("/identity", axum::routing::get(|| async { "newer" })),
                )
                .await;

            // Paused actors never call advance. Deliver the response after the absolute
            // ten-second attempt deadline, then resume with a newer route owner present.
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(11)).await;
            respond(&server, &pending_request, source, delayed_reply, granted_ip).await;
            assert!(
                recovery
                    .advance(&registry, &client, request())
                    .await
                    .is_none()
            );
            // The runtime may finish the attempt while its actor is paused. Neither
            // a late OFFER nor ACK may publish a route or continue the expired exchange.
            tokio::task::yield_now().await;
            let mut buffer = [0; 4096];
            assert_eq!(
                server.try_recv_from(&mut buffer).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "{delayed_reply:?}"
            );
            assert!(recovery.renewal.is_none());
            assert!(recovery.retry.is_some());
            assert!(
                (Duration::from_secs(3)..=Duration::from_secs(5))
                    .contains(&recovery.next_attempt_in())
            );
            assert_eq!(recovery.mounted_ip, Some(old_ip));
            assert_eq!(
                registry.owners.lock().unwrap()[&granted_ip.to_string()],
                newer.owner
            );
            let routes = registry.read().await;
            assert_eq!(routes.len(), 2);
            assert!(routes.contains_key(&old_ip.to_string()));
            let route = routes[&granted_ip.to_string()].clone();
            drop(routes);
            let response = route
                .oneshot(Request::get("/identity").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                axum::body::to_bytes(response.into_body(), 64)
                    .await
                    .unwrap()
                    .as_ref(),
                b"newer"
            );
            tokio::time::resume();
            service.shutdown().await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn renewal_expiring_while_waiting_for_route_publication_preserves_both_routes() {
        let registry = BmcMockRegistry::default();
        let old_ip = Ipv4Addr::new(192, 0, 2, 10);
        let granted_ip = Ipv4Addr::new(192, 0, 2, 11);
        let mut first = BmcDhcpRecovery::default();
        let mut newer = BmcDhcpRecovery::default();
        first.mount(&registry, old_ip, Router::new()).await;
        newer.mount(&registry, granted_ip, Router::new()).await;
        let read_guard = registry.read().await;
        let mut publication = Box::pin(registry.mount_before(
            first.owner,
            Some(old_ip),
            granted_ip,
            Router::new(),
            Some(Instant::now() + Duration::from_secs(10)),
        ));
        assert!(publication.as_mut().now_or_never().is_none());
        tokio::time::advance(Duration::from_secs(11)).await;
        // Releasing the lock makes the timeout's inner future ready after expiry.
        drop(read_guard);
        assert!(!publication.await);
        let routes = registry.read().await;
        assert!(routes.contains_key(&old_ip.to_string()));
        assert!(routes.contains_key(&granted_ip.to_string()));
        let owners = registry.owners.lock().unwrap();
        assert_eq!(owners[&old_ip.to_string()], first.owner);
        assert_eq!(owners[&granted_ip.to_string()], newer.owner);
    }

    // Single-thread scheduling intentionally queues the renewal before the
    // actor's RPC. The runtime must drive that waiter while the actor awaits.
    #[tokio::test(flavor = "current_thread")]
    async fn api_renewal_does_not_park_the_shared_connection_lock() {
        use rpc::forge_api_client::ForgeApiClient;
        use rpc::forge_tls_client::ForgeClientT;

        #[derive(Debug, Default)]
        struct Gate {
            calls: AtomicUsize,
            entered: tokio::sync::Notify,
            release: tokio::sync::Notify,
        }
        #[derive(Debug)]
        struct Provider(Arc<Gate>);
        #[async_trait::async_trait]
        impl tonic_client_wrapper::ConnectionProvider<ForgeClientT> for Provider {
            async fn provide_connection(&self) -> Result<ForgeClientT, tonic::Status> {
                if self.0.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    // connection() holds the generated client's shared mutex here.
                    self.0.entered.notify_one();
                    self.0.release.notified().await;
                }
                Err(tonic::Status::unavailable("test connection refused"))
            }

            async fn connection_is_stale(&self, _: std::time::SystemTime) -> bool {
                false
            }

            fn connection_url(&self) -> &str {
                "https://api.test"
            }
        }

        let gate = Arc::new(Gate::default());
        let api = crate::api_client::ApiClient(ForgeApiClient::build(Provider(gate.clone())));
        let blocker_client = api.0.clone();
        let blocker = tokio::spawn(async move { blocker_client.connection().await });
        tokio::time::timeout(Duration::from_secs(1), gate.entered.notified())
            .await
            .expect("first call must hold the shared connection lock");
        let registry = BmcMockRegistry::default();
        let old_ip = Ipv4Addr::new(192, 0, 2, 10);
        let mut recovery = BmcDhcpRecovery::with_renewal(Duration::from_secs(900));
        recovery.mount(&registry, old_ip, Router::new()).await;
        recovery.deadline = Some(Instant::now());
        let client = DhcpClient::Api(api.clone());
        let request = || DhcpRequestInfo {
            mac_address: MacAddress::new([2, 0, 0, 0, 4, 0]),
            relay_address: Ipv4Addr::new(192, 0, 2, 1),
            vendor_class: Some("NVIDIA/BF/BMC"),
        };
        assert!(
            recovery
                .advance(&registry, &client, request())
                .await
                .is_none()
        );
        tokio::task::yield_now().await;

        // The actor starts another RPC in the same pass, without polling renewal
        // again. Previously its own queued renewal waiter held the FIFO permit.
        let mut actor_rpc = Box::pin(api.discover_dhcp(
            request().mac_address,
            request().relay_address.to_string(),
            None,
            request().vendor_class,
        ));
        assert!(actor_rpc.as_mut().now_or_never().is_none());
        gate.release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(1), actor_rpc)
            .await
            .expect("renewal must not block the same actor's subsequent API call");
        assert!(result.is_err());
        assert!(blocker.await.unwrap().is_err());
        // Even while this actor leaves the handle unpolled (as when paused),
        // another shared API caller completes without waiting for an actor pass.
        assert!(
            tokio::time::timeout(Duration::from_secs(1), api.0.connection())
                .await
                .expect("a paused actor must not park the shared connection permit")
                .is_err()
        );
        assert!(gate.calls.load(Ordering::SeqCst) >= 4);
        assert!(
            recovery
                .advance(&registry, &client, request())
                .await
                .is_none()
        );
        assert!(recovery.renewal.is_none());
        assert!(recovery.retry.is_some());
        assert_eq!(recovery.mounted_ip, Some(old_ip));
        assert!(registry.read().await.contains_key(&old_ip.to_string()));
    }

    #[tokio::test]
    async fn dropping_recovery_cancels_its_pending_renewal() {
        let (mut reply, response) = oneshot::channel::<crate::dhcp_wrapper::DhcpResponseInfo>();
        let recovery = BmcDhcpRecovery {
            renewal: Some(BmcDhcpRenewal {
                expires_at: Instant::now() + Duration::from_secs(10),
                task: AbortOnDropHandle::new(tokio::spawn(async move { Ok(response.await?) })),
            }),
            ..Default::default()
        };
        assert!(!reply.is_closed());
        drop(recovery);
        tokio::time::timeout(Duration::from_secs(1), reply.closed())
            .await
            .expect("dropping recovery must abort the pending task");
    }

    /// An abandoned address never serves its former BMC during DHCP recovery,
    /// including when another device receives it before the response arrives.
    #[tokio::test]
    async fn factory_reset_retries_dhcp_and_retires_a_reassigned_address() {
        let (commands, mut requests) = mpsc::unbounded_channel();
        let range = PoolConfig::new(MacAddress::new([2, 0, 0, 0, 2, 0]), 24).unwrap();
        let mut pool = MacAddressPool::new(Config {
            pool: Some(range),
            ranges: None,
        });
        let machine = MachineInfo::Host(bmc_mock::HostMachineInfo::new(
            HardwareType::GenericAmi,
            Vec::new(),
            &mut pool,
            range,
        ));
        let (router, state) = bmc_mock::machine_router(
            &machine,
            Arc::new(ResetCallbacks(commands)),
            "recovery".into(),
            false,
            Default::default(),
        );
        let router = router.route("/identity", axum::routing::get(|| async { "first" }));
        let baseline = state.persisted();
        state
            .account_service_state
            .change_factory_default_password("configured-password");
        let registry = BmcMockRegistry::default();
        let old_ip = Ipv4Addr::new(192, 0, 2, 10);
        let new_ip = Ipv4Addr::new(192, 0, 2, 11);
        let mut recovery = BmcDhcpRecovery::with_renewal(Duration::ZERO);
        recovery.mount(&registry, old_ip, router.clone()).await;
        assert_eq!(recovery.next_attempt_in(), Duration::MAX);
        let path = "/redfish/v1/Managers/Self/Actions/Manager.ResetToDefaults";
        let request = Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"ResetType":"ResetAll"}"#))
            .unwrap();
        let response = tokio::spawn(router.clone().oneshot(request));
        let (DeviceEffect::ResetBmcToDefaults(update), reply) = requests.recv().await.unwrap()
        else {
            panic!("expected BMC factory reset")
        };
        reply.send(recovery.reset(update).await).unwrap();
        assert!(response.await.unwrap().unwrap().status().is_success());
        assert_eq!(state.factory_reset_generation(), 1);
        let restored = serde_json::to_value(state.persisted()).unwrap();
        let baseline = serde_json::to_value(baseline).unwrap();
        assert_eq!(restored["accounts"], baseline["accounts"]);
        assert_eq!(state.persisted().factory_reset_generation(), 1);

        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let std::net::SocketAddr::V4(server_address) = server.local_addr().unwrap() else {
            unreachable!()
        };
        let (client, service) = UdpDhcpClient::start(
            server_address,
            "127.0.0.1:0".parse().unwrap(),
            Ipv4Addr::LOCALHOST,
        )
        .await
        .unwrap();
        let client = DhcpClient::UdpRelay(client);
        let mac = machine.bmc_mac_address();
        let relay = Ipv4Addr::new(192, 0, 2, 1);
        let (response_pending, response_ready) = oneshot::channel();
        let (release_response, response_released) = oneshot::channel();
        let server_task = tokio::spawn(async move {
            let mut response_pending = Some(response_pending);
            let mut response_released = Some(response_released);
            let mut buffer = [0; 4096];
            for response_type in [MessageType::Nak, MessageType::Offer, MessageType::Ack] {
                let (length, source) = server.recv_from(&mut buffer).await.unwrap();
                let request = Message::decode(&mut Decoder::new(&buffer[..length])).unwrap();
                assert_eq!(request.chaddr(), mac.bytes());
                assert_eq!(
                    request.opts().get(OptionCode::ClassIdentifier),
                    Some(&DhcpOption::ClassIdentifier(b"NVIDIA/BF/BMC".to_vec()))
                );
                let Some(DhcpOption::RelayAgentInformation(info)) =
                    request.opts().get(OptionCode::RelayAgentInformation)
                else {
                    panic!("missing relay option")
                };
                assert_eq!(
                    info.get(RelayCode::LinkSelection),
                    Some(&RelayInfo::LinkSelection(relay))
                );
                if response_type == MessageType::Offer {
                    response_pending.take().unwrap().send(()).unwrap();
                    response_released.take().unwrap().await.unwrap();
                }
                let mut response = Message::default();
                response
                    .set_opcode(Opcode::BootReply)
                    .set_htype(HType::Eth)
                    .set_xid(request.xid())
                    .set_chaddr(request.chaddr())
                    .set_giaddr(request.giaddr())
                    .set_yiaddr(new_ip);
                response
                    .opts_mut()
                    .insert(DhcpOption::MessageType(response_type));
                response
                    .opts_mut()
                    .insert(DhcpOption::ServerIdentifier(Ipv4Addr::LOCALHOST));
                let mut encoded = Vec::new();
                response.encode(&mut Encoder::new(&mut encoded)).unwrap();
                server.send_to(&encoded, source).await.unwrap();
            }
        });
        let request = || DhcpRequestInfo {
            mac_address: mac,
            relay_address: relay,
            vendor_class: Some("NVIDIA/BF/BMC"),
        };
        assert!(
            recovery
                .advance(&registry, &client, request())
                .await
                .is_none()
        );
        assert!(!registry.read().await.contains_key(&old_ip.to_string()));
        assert!(
            (Duration::from_secs(2)..=Duration::from_secs(5)).contains(&recovery.next_attempt_in())
        );
        // Expire the observed backoff; no recovery state or response is fabricated.
        recovery.deadline = Some(Instant::now());
        let shared = bmc_mock::combined_router(registry.clone());
        let old_request = || {
            Request::get("/identity")
                .header("forwarded", format!("host={old_ip}"))
                .body(Body::empty())
                .unwrap()
        };
        let (info, ()) = tokio::join!(recovery.advance(&registry, &client, request()), async {
            response_ready.await.unwrap();
            assert_eq!(
                shared
                    .clone()
                    .oneshot(old_request())
                    .await
                    .unwrap()
                    .status(),
                axum::http::StatusCode::NOT_FOUND,
                "the old BMC answered while a lease response was delayed"
            );
            let mut second = BmcDhcpRecovery::default();
            let second_router =
                Router::new().route("/identity", axum::routing::get(|| async { "second" }));
            second.mount(&registry, old_ip, second_router).await;
            let response = shared.clone().oneshot(old_request()).await.unwrap();
            let body = axum::body::to_bytes(response.into_body(), 64)
                .await
                .unwrap();
            assert_eq!(body.as_ref(), b"second");
            release_response.send(()).unwrap();
        });
        let info = info.unwrap();
        assert_eq!(info.ip_address, new_ip);
        assert_eq!(recovery.next_attempt_in(), Duration::MAX);
        let routes = registry.read().await;
        assert!(routes.contains_key(&old_ip.to_string()));
        let moved = routes[&new_ip.to_string()].clone();
        drop(routes);
        assert!(
            moved
                .oneshot(
                    Request::get("/redfish/v1/Managers/Self")
                        .body(Body::empty())
                        .unwrap()
                )
                .await
                .unwrap()
                .status()
                .is_success()
        );
        assert_eq!(state.factory_reset_generation(), 1);
        let response = shared.oneshot(old_request()).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), 64)
            .await
            .unwrap();
        assert_eq!(
            body.as_ref(),
            b"second",
            "old-owner recovery displaced the new BMC"
        );
        // A same-address renewal adds no extra route for either BMC.
        recovery.mount(&registry, new_ip, router).await;
        assert_eq!(registry.read().await.len(), 2);
        server_task.await.unwrap();
        service.shutdown().await.unwrap();
    }
}
