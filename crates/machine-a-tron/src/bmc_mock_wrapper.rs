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
use std::borrow::Borrow;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;

use axum::Router;
use bmc_mock::injection::InjectionStore;
use bmc_mock::ipmi_sim::{IpmiSimConfig, IpmiSimHandle};
use bmc_mock::{BmcState, Callbacks, CombinedServer, HardwareType, HostnameQuerying, MachineInfo};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::config::MachineATronConfig;
use crate::machine_state_machine::MachineStateError;
use crate::mock_ssh_server;
use crate::mock_ssh_server::{MockSshServerHandle, PromptBehavior};

/// Console simulators machine-a-tron starts next to a device's Redfish mock.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ConsoleSimulators {
    /// Serve the BMC's SSH serial console from an in-process mock SSH server.
    pub(super) ssh_server: bool,
    /// Serve IPMI from an external `ipmi_sim` process.
    pub(super) ipmi: bool,
}

impl From<&MachineATronConfig> for ConsoleSimulators {
    fn from(config: &MachineATronConfig) -> Self {
        Self {
            ssh_server: config.mock_bmc_ssh_server,
            ipmi: config.enable_ipmi_simulation,
        }
    }
}

/// BmcMockWrapper holds a single instance of bmc-mock, configured to mock a single BMC for
/// either a DPU or a Host. It will rewrite certain responses to customize them for the machines
/// machine-a-tron is mocking.
///
/// Each device builds one wrapper and keeps it for the life of the process. `SetupBmc` runs once
/// per device, after the BMC obtains its DHCP lease at startup, and is retried only when a console
/// simulator fails to start; the retry re-publishes this wrapper instead of building a new one.
/// Host power cycles never reach `SetupBmc`, so the BMC's Redfish model, accounts (including
/// passwords rotated by the control plane) and sessions carry across them. That state lives only
/// in memory: a process restart returns every BMC to its factory credentials.
pub(super) struct BmcMockWrapper {
    consoles: ConsoleSimulators,
    bmc_mock_router: Router,
    bmc_mock_state: BmcState,
    hostname: Arc<dyn HostnameQuerying>,
    needs_ipmi_console: bool,
    requires_ssh_console: bool,
    stable_id: String,
    ssh_prompt_behavior: PromptBehavior,
    /// Tags this wrapper's registry entries so it never removes another BMC's router.
    registration: Uuid,
    /// Registry key (the BMC's DHCP address) this router is currently published under.
    registered_authority: Option<String>,
}

impl BmcMockWrapper {
    pub(super) fn new(
        machine_info: &MachineInfo,
        consoles: ConsoleSimulators,
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
            consoles,
            bmc_mock_router,
            bmc_mock_state,
            hostname,
            needs_ipmi_console: machine_info.needs_ipmi_console(),
            requires_ssh_console,
            stable_id: host_id.to_string(),
            ssh_prompt_behavior,
            registration: Uuid::new_v4(),
            registered_authority: None,
        }
    }

    /// Publishes this BMC's router in the shared registry under `ip_address`.
    ///
    /// Registering again under the same address replaces the entry, so a retried `SetupBmc`
    /// keeps the registry at one entry per BMC. If the address differs from the previous
    /// registration, the previous key is removed only while it still holds this wrapper's
    /// router: DHCP may have handed that address to another BMC in the meantime, and that BMC's
    /// registration is left alone. Replacing an entry that belongs to another BMC is logged,
    /// since it means two BMCs claim one DHCP address.
    pub(super) async fn register(&mut self, registry: &BmcMockRegistry, ip_address: Ipv4Addr) {
        let authority = ip_address.to_string();
        let mut registry = registry.write().await;
        if let Some(previous) = self.registered_authority.take()
            && previous != authority
            && registry
                .get(&previous)
                .is_some_and(|entry| entry.owner == self.registration)
        {
            registry.remove(&previous);
        }
        if let Some(replaced) = registry.get(&authority)
            && replaced.owner != self.registration
        {
            tracing::warn!(
                authority = %authority,
                replaced_owner = %replaced.owner,
                owner = %self.registration,
                "BMC registration replaced another BMC's router under the same address; two BMCs claim one DHCP address",
            );
        }
        registry.insert(
            authority.clone(),
            RegisteredBmc {
                owner: self.registration,
                router: self.bmc_mock_router.clone(),
            },
        );
        self.registered_authority = Some(authority);
    }

    /// Starts per-machine console simulators when Redfish is served by a combined BMC mock.
    /// Returns `None` when no simulator is enabled for the hardware profile.
    pub(super) async fn start(&self) -> Result<Option<BmcMockWrapperHandle>, MachineStateError> {
        let ssh_handle = if self.consoles.ssh_server
            && (self.requires_ssh_console || self.bmc_mock_state.has_enabled_ssh_serial_console())
        {
            Some(
                mock_ssh_server::spawn(None, self.hostname.clone(), None, self.ssh_prompt_behavior)
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

    pub(super) fn state(&self) -> &BmcState {
        &self.bmc_mock_state
    }

    fn need_ipmi_sim(&self) -> bool {
        self.consoles.ipmi && self.needs_ipmi_console
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

/// A BMC mock router published in the [`BmcMockRegistry`], tagged with the wrapper that owns it
/// so a wrapper moving to a new address can tell whether its old key still holds its own router.
pub struct RegisteredBmc {
    owner: Uuid,
    router: Router,
}

impl Borrow<Router> for RegisteredBmc {
    fn borrow(&self) -> &Router {
        &self.router
    }
}

/// BmcMockRegistry is shared state that MachineATron's mock hosts use to publish their BMC mock
/// routers, keyed by the BMC's DHCP address, so that a single shared listener can delegate to them.
pub type BmcMockRegistry = Arc<RwLock<HashMap<String, RegisteredBmc>>>;

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use base64::Engine;
    use base64::prelude::BASE64_STANDARD;
    use bmc_mock::mac_address_pool::PoolConfig as MacAddressPoolConfig;
    use bmc_mock::{
        DUMMY_FACTORY_PASSWORD, DUMMY_FACTORY_USERNAME, HostMachineInfo, MockPowerState,
        SetSystemPowerError, SystemPowerControl,
    };
    use mac_address::MacAddress;
    use tower::ServiceExt;

    use super::*;

    #[derive(Debug)]
    struct PoweredOn;

    impl Callbacks for PoweredOn {
        fn get_power_state(&self) -> MockPowerState {
            MockPowerState::On
        }

        fn send_power_command(
            &self,
            _reset_type: SystemPowerControl,
        ) -> Result<(), SetSystemPowerError> {
            Ok(())
        }

        fn state_refresh_indication(&self) {}
    }

    #[derive(Debug)]
    struct Localhost;

    impl HostnameQuerying for Localhost {
        fn get_hostname(&'_ self) -> Cow<'_, str> {
            Cow::Borrowed("localhost")
        }
    }

    fn dell_host_bmc() -> BmcMockWrapper {
        let mac = MacAddress::new([2, 0, 0, 0, 0, 3]);
        let machine_info = MachineInfo::Host(HostMachineInfo {
            hw_type: HardwareType::DellPowerEdgeR750,
            rack_placement: None,
            bmc_mac_address: mac,
            serial: "test-host".to_string(),
            dpus: Vec::new(),
            non_dpu_mac_address: None,
            nvos_mac_addresses: Vec::new(),
            switch_serial_number: None,
            hw_mac_addr_pool: MacAddressPoolConfig::new(mac, 24).unwrap(),
            delta_psu_power: None,
            initial_host_firmware: None,
            desired_host_firmware: None,
        });
        BmcMockWrapper::new(
            &machine_info,
            ConsoleSimulators::default(),
            Arc::new(PoweredOn),
            Arc::new(Localhost),
            Uuid::new_v4(),
            Arc::new(InjectionStore::new()),
            None,
        )
    }

    async fn registered_router(registry: &BmcMockRegistry, authority: &str) -> Router {
        registry
            .read()
            .await
            .get(authority)
            .map(|entry| entry.router.clone())
            .expect("BMC is registered")
    }

    fn basic_auth(password: &str) -> String {
        format!(
            "Basic {}",
            BASE64_STANDARD.encode(format!("{DUMMY_FACTORY_USERNAME}:{password}"))
        )
    }

    async fn get_systems(router: Router, password: &str) -> StatusCode {
        router
            .oneshot(
                Request::builder()
                    .uri("/redfish/v1/Systems")
                    .header(header::AUTHORIZATION, basic_auth(password))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn reregistering_under_the_same_address_keeps_one_entry() {
        let registry = BmcMockRegistry::default();
        let mut bmc = dell_host_bmc();
        let ip_address = Ipv4Addr::new(10, 0, 0, 5);

        bmc.register(&registry, ip_address).await;
        bmc.register(&registry, ip_address).await;

        let registry = registry.read().await;
        assert_eq!(registry.keys().collect::<Vec<_>>(), ["10.0.0.5"]);
        assert_eq!(registry["10.0.0.5"].owner, bmc.registration);
    }

    #[tokio::test]
    async fn reregistering_publishes_the_same_bmc_state() {
        // `SetupBmc` is retried when a console simulator fails to start. The retry publishes the
        // wrapper the actor already holds rather than a new one, so account changes made through
        // the registered router in the meantime stay in force.
        let registry = BmcMockRegistry::default();
        let mut bmc = dell_host_bmc();
        let ip_address = Ipv4Addr::new(10, 0, 0, 5);
        bmc.register(&registry, ip_address).await;

        // The factory password only unlocks the account service, like a BMC that requires a
        // password change before serving anything else.
        let router = registered_router(&registry, "10.0.0.5").await;
        assert_eq!(
            get_systems(router.clone(), DUMMY_FACTORY_PASSWORD).await,
            StatusCode::FORBIDDEN
        );
        let response = router
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri(format!(
                        "/redfish/v1/AccountService/Accounts/{DUMMY_FACTORY_USERNAME}"
                    ))
                    .header(header::AUTHORIZATION, basic_auth(DUMMY_FACTORY_PASSWORD))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"Password":"rotated"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        bmc.register(&registry, ip_address).await;
        assert!(bmc.start().await.unwrap().is_none());

        let router = registered_router(&registry, "10.0.0.5").await;
        assert_eq!(get_systems(router.clone(), "rotated").await, StatusCode::OK);
        assert_eq!(
            get_systems(router, DUMMY_FACTORY_PASSWORD).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn new_address_moves_the_registration() {
        let registry = BmcMockRegistry::default();
        let mut bmc = dell_host_bmc();

        bmc.register(&registry, Ipv4Addr::new(10, 0, 0, 5)).await;
        bmc.register(&registry, Ipv4Addr::new(10, 0, 0, 9)).await;

        let registry = registry.read().await;
        assert_eq!(registry.keys().collect::<Vec<_>>(), ["10.0.0.9"]);
        assert_eq!(registry["10.0.0.9"].owner, bmc.registration);
    }

    #[tokio::test]
    async fn another_bmc_under_the_same_address_replaces_the_entry() {
        // The registry cannot tell which of two BMCs claiming one DHCP address is right, so the
        // latest registration wins and the replacement is logged.
        let registry = BmcMockRegistry::default();
        let mut first = dell_host_bmc();
        let mut second = dell_host_bmc();
        let address = Ipv4Addr::new(10, 0, 0, 5);

        first.register(&registry, address).await;
        second.register(&registry, address).await;

        let registry = registry.read().await;
        assert_eq!(registry.keys().collect::<Vec<_>>(), ["10.0.0.5"]);
        assert_eq!(registry["10.0.0.5"].owner, second.registration);
    }

    #[tokio::test]
    async fn new_address_leaves_another_bmc_under_the_old_address_alone() {
        let registry = BmcMockRegistry::default();
        let mut first = dell_host_bmc();
        let mut second = dell_host_bmc();
        let old_address = Ipv4Addr::new(10, 0, 0, 5);

        first.register(&registry, old_address).await;
        // DHCP hands the first BMC's old address to the second BMC before the first BMC
        // registers under its new one.
        second.register(&registry, old_address).await;
        first.register(&registry, Ipv4Addr::new(10, 0, 0, 9)).await;

        let registry = registry.read().await;
        let mut keys = registry.keys().collect::<Vec<_>>();
        keys.sort();
        assert_eq!(keys, ["10.0.0.5", "10.0.0.9"]);
        assert_eq!(registry["10.0.0.5"].owner, second.registration);
        assert_eq!(registry["10.0.0.9"].owner, first.registration);
    }
}
