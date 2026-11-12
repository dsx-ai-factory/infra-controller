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

use std::net::SocketAddr;
use std::sync::Arc;

use bmc_drivers::Rules;
use bmc_platform::{AuthError, IpmiOps, PlatformError};
use carbide_ipmi::IPMITool;
use carbide_redfish::nv_redfish::{NvRedfishClientPool, NvRedfishConnection, RedfishBmc};
use carbide_secrets::credentials::{CredentialReader, Credentials};
use carbide_utils::redfish::BmcAccessInfo;
use model::site_explorer::EndpointExplorationReport;
use nv_redfish::bmc_http::BmcCredentials;
use thiserror::Error;

use crate::credentials::ProxyAuthenticated;
use crate::observe::observe;
use crate::{
    BmcRef, ConnectError, ConnectedBmc, CredentialRequest, CredentialRequestError,
    DriverConnectorError, EndpointIpmiOps, ExploredConnectError, RuntimeAuthMode,
    RuntimeCredentialProvider, StoredRootCredentials,
};

/// Constructs and refreshes concrete pooled Redfish connections on behalf of
/// one named caller.
pub struct ConnectionManager {
    pool: Arc<NvRedfishClientPool>,
    credentials: Arc<dyn RuntimeCredentialProvider>,
    caller_identity: String,
    auth_mode: RuntimeAuthMode,
}

impl ConnectionManager {
    /// Creates a manager; `caller_identity` names the service requesting credentials.
    pub fn new(
        pool: Arc<NvRedfishClientPool>,
        credentials: Arc<dyn RuntimeCredentialProvider>,
        caller_identity: String,
    ) -> Result<Self, CredentialRequestError> {
        if caller_identity.trim().is_empty() {
            return Err(CredentialRequestError::EmptyCallerIdentity);
        }
        Ok(Self {
            pool,
            credentials,
            caller_identity,
            auth_mode: RuntimeAuthMode::Basic,
        })
    }

    /// Selects the runtime authentication mechanism. Basic auth is the default.
    pub const fn with_auth_mode(mut self, auth_mode: RuntimeAuthMode) -> Self {
        self.auth_mode = auth_mode;
        self
    }

    /// Acquires credentials and builds a pooled connection.
    pub async fn connect(
        &self,
        endpoint: BmcRef,
        ipmi: Option<Arc<dyn IpmiOps>>,
    ) -> Result<ConnectedBmc<RedfishBmc>, ConnectError> {
        let connection = self.authenticated_connection(&endpoint).await?;
        let selection = endpoint.selection();
        tracing::debug!(
            bmc = %endpoint.address(),
            drivers = %selection.drivers,
            quirks = ?selection.quirks,
            rules_hash = %selection.hash,
            "BMC connected with its selected drivers"
        );
        Ok(ConnectedBmc::new(
            endpoint,
            connection.bmc,
            connection.service_root,
            ipmi,
        ))
    }

    /// Evicts every pooled root for the BMC, re-issues credentials, and rebuilds.
    pub async fn refresh(
        &self,
        connected: &mut ConnectedBmc<RedfishBmc>,
    ) -> Result<(), ConnectError> {
        self.pool
            .invalidate_service_roots_for_bmc(connected.endpoint().address());
        let connection = self.authenticated_connection(connected.endpoint()).await?;
        connected.replace_redfish(connection.bmc, connection.service_root);
        Ok(())
    }

    /// Runs an operation and, in session mode, retries it once after a 401 or
    /// 403 with a freshly issued token.
    ///
    /// In basic mode the provider can only return the same root password, so
    /// a retry would just repeat the failed login against the BMC's lockout
    /// counter; the error is returned directly. Non-authentication errors and
    /// failures from the second attempt are returned without another refresh.
    pub async fn with_auth_retry<T, F>(
        &self,
        connected: &mut ConnectedBmc<RedfishBmc>,
        operation: F,
    ) -> Result<T, AuthRetryError>
    where
        F: AsyncFn(&ConnectedBmc<RedfishBmc>) -> Result<T, PlatformError>,
    {
        match operation(connected).await {
            Ok(value) => Ok(value),
            Err(error) if is_auth_error(&error) && self.auth_mode == RuntimeAuthMode::Session => {
                self.refresh(connected)
                    .await
                    .map_err(AuthRetryError::Refresh)?;
                operation(connected)
                    .await
                    .map_err(AuthRetryError::Operation)
            }
            Err(error) => Err(AuthRetryError::Operation(error)),
        }
    }

    async fn authenticated_connection(
        &self,
        endpoint: &BmcRef,
    ) -> Result<NvRedfishConnection, ConnectError> {
        let request = CredentialRequest::new(
            self.caller_identity.clone(),
            endpoint.mac_address(),
            endpoint.address(),
            self.auth_mode,
        );
        let lease = self
            .credentials
            .issue(&request)
            .await
            .map_err(ConnectError::Credentials)?;
        let (address, credentials) = (endpoint.address(), lease.into_credentials());
        observe(endpoint, None, "create_client", async {
            self.pool
                .connection_with_bmc_credentials(address, credentials)
                .await
                .map_err(PlatformError::from_redfish)
        })
        .await
        .map_err(ConnectError::Transport)
    }
}

/// Connects a service to BMCs through the drivers a site's rules select,
/// authenticating with each BMC's stored root credentials.
pub struct DriverConnector {
    connections: ConnectionManager,
    direct_pool: Arc<NvRedfishClientPool>,
    rules: Rules,
    ipmi_tool: Arc<dyn IPMITool>,
}

/// The pools a [`DriverConnector`] connects through.
pub struct DriverPools {
    /// Carries connections with the stored credentials. When it routes
    /// through nico-bmc-proxy, Redfish connections never read them.
    pub routed: Arc<NvRedfishClientPool>,
    /// Dials BMCs without nico-bmc-proxy, for connections with explicit
    /// credentials the proxy cannot use.
    pub direct: Arc<NvRedfishClientPool>,
}

impl DriverConnector {
    /// `caller_identity` names the service in credential requests, and
    /// `ipmi_tool` serves drivers that restart or reset over IPMI.
    pub fn new(
        pools: DriverPools,
        credential_reader: Arc<dyn CredentialReader>,
        caller_identity: &str,
        rules: Rules,
        ipmi_tool: Arc<dyn IPMITool>,
    ) -> Result<Self, DriverConnectorError> {
        if pools.direct.authenticates_upstream() {
            return Err(DriverConnectorError::ProxiedDirectPool);
        }
        let credentials: Arc<dyn RuntimeCredentialProvider> =
            if pools.routed.authenticates_upstream() {
                Arc::new(ProxyAuthenticated)
            } else {
                Arc::new(StoredRootCredentials::new(credential_reader))
            };
        let connections =
            ConnectionManager::new(pools.routed, credentials, caller_identity.to_string())?;
        Ok(Self {
            connections,
            direct_pool: pools.direct,
            rules,
            ipmi_tool,
        })
    }

    /// Connects to the BMC at `access`, selecting drivers for the identity
    /// `report` recorded and reaching IPMI at the port its first manager names.
    pub async fn connect(
        &self,
        access: &BmcAccessInfo,
        report: &EndpointExplorationReport,
    ) -> Result<ConnectedBmc<RedfishBmc>, ExploredConnectError> {
        let endpoint = BmcRef::from_exploration(access, report, &self.rules)?;
        let ipmi = EndpointIpmiOps::new(
            self.ipmi_tool.clone(),
            ipmi_address(&endpoint, report),
            endpoint.credential_key().clone(),
        );
        Ok(self
            .connections
            .connect(endpoint, Some(Arc::new(ipmi)))
            .await?)
    }

    /// Connects to the BMC at `access` with `credentials` instead of its
    /// stored ones, through the direct pool because nico-bmc-proxy
    /// authenticates with the stored credentials. The connection is not
    /// cached, refreshed, or given IPMI, which would use the stored
    /// credentials; its reference still names the root credential key, which
    /// nothing on the connection reads.
    pub async fn connect_with_credentials(
        &self,
        access: &BmcAccessInfo,
        report: &EndpointExplorationReport,
        Credentials::UsernamePassword { username, password }: Credentials,
    ) -> Result<ConnectedBmc<RedfishBmc>, ExploredConnectError> {
        let endpoint = BmcRef::from_exploration(access, report, &self.rules)?;
        if password.is_empty() {
            return Err(ConnectError::Credentials(PlatformError::Auth(
                AuthError::CredentialsUnavailable,
            ))
            .into());
        }
        let address = endpoint.address();
        let connection = observe(&endpoint, None, "create_client", async {
            self.direct_pool
                .uncached_connection_with_bmc_credentials(
                    address,
                    BmcCredentials::new(username, password),
                )
                .await
                .map_err(PlatformError::from_redfish)
        })
        .await
        .map_err(ConnectError::Transport)?;
        Ok(ConnectedBmc::new(
            endpoint,
            connection.bmc,
            connection.service_root,
            None,
        ))
    }
}

fn ipmi_address(endpoint: &BmcRef, report: &EndpointExplorationReport) -> SocketAddr {
    let port = report
        .managers
        .first()
        .and_then(|manager| manager.ipmi_port)
        .unwrap_or(carbide_ipmi::DEFAULT_IPMI_PORT);
    SocketAddr::new(endpoint.address().ip(), port)
}

/// Failure from the one-shot authenticated operation API.
#[derive(Debug, Error)]
pub enum AuthRetryError {
    /// Credential refresh or connection reconstruction failed.
    #[error("BMC authentication refresh failed: {0}")]
    Refresh(ConnectError),
    /// The operation failed without a retry or after its only retry.
    #[error("BMC operation failed: {0}")]
    Operation(PlatformError),
}

const fn is_auth_error(error: &PlatformError) -> bool {
    matches!(
        error,
        PlatformError::Auth(AuthError::InvalidCredentials | AuthError::InsufficientPrivilege)
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    use arc_swap::ArcSwap;
    use axum::extract::{Request, State};
    use axum::http::header::AUTHORIZATION;
    use axum::middleware::{Next, from_fn_with_state};
    use axum::response::Response;
    use bmc_drivers::Rules;
    use bmc_mock::CombinedServer;
    use bmc_mock::test_support::{lenovo_thinksystem_sr670_router, serve_https};
    use bmc_platform::{AuthError, PlatformError};
    use carbide_redfish::nv_redfish::{
        NvRedfishClientPool, RedfishBmc, new_pool, new_proxied_pool,
    };
    use carbide_secrets::credentials::Credentials;
    use carbide_test_support::value_scenarios;
    use carbide_utils::HostPortPair;
    use carbide_utils::redfish::{BmcAccessInfo, redfish_basic_authorization_context};
    use mac_address::MacAddress;
    use model::site_explorer::{ComputerSystem, EndpointExplorationReport, Manager, ServiceRoot};
    use nv_redfish::resource::PowerState;

    use super::{DriverConnector, DriverPools, ipmi_address, is_auth_error};
    use crate::test_support::{RecordingStore, endpoint};
    use crate::{
        ConnectError, ConnectedBmc, CredentialRequest, DriverConnectorError, ExploredConnectError,
        RuntimeAuthMode,
    };

    fn direct_pool() -> Arc<NvRedfishClientPool> {
        new_pool(Arc::new(ArcSwap::from_pointee(None)))
    }

    /// A pool routed through an unreachable nico-bmc-proxy.
    fn proxied_pool() -> Arc<NvRedfishClientPool> {
        new_proxied_pool(
            HostPortPair::HostAndPort("bmc-proxy.example".to_string(), 1079),
            "tls.crt",
            "tls.key",
            "ca.crt",
        )
    }

    fn try_connector(
        routed: Arc<NvRedfishClientPool>,
        direct: Arc<NvRedfishClientPool>,
        store: &Arc<RecordingStore>,
    ) -> Result<DriverConnector, DriverConnectorError> {
        DriverConnector::new(
            DriverPools { routed, direct },
            store.clone(),
            "test",
            Rules::new(Vec::new()).expect("empty rules are valid"),
            carbide_ipmi::test_support(),
        )
    }

    fn connector(routed: Arc<NvRedfishClientPool>, store: &Arc<RecordingStore>) -> DriverConnector {
        try_connector(routed, direct_pool(), store).expect("the pools are in order")
    }

    /// Explicit credentials would reach nico-bmc-proxy, which authenticates
    /// with the stored ones instead.
    #[test]
    fn a_proxied_direct_pool_is_rejected() {
        assert!(matches!(
            try_connector(direct_pool(), proxied_pool(), &stored_root()),
            Err(DriverConnectorError::ProxiedDirectPool)
        ));
    }

    fn stored_root() -> Arc<RecordingStore> {
        Arc::new(RecordingStore::new(Ok(Some(Credentials::new(
            "root", "password",
        )))))
    }

    /// nico-bmc-proxy authenticates upstream and strips credentials, so a
    /// proxied connector's Redfish connections must not read the root password.
    #[tokio::test]
    async fn proxied_redfish_connections_never_read_the_stored_password() {
        let proxied = proxied_pool();
        let request = CredentialRequest::new(
            "test".to_string(),
            MacAddress::new([2, 0, 0, 0, 0, 1]),
            "192.0.2.10:443".parse().expect("valid socket address"),
            RuntimeAuthMode::Basic,
        );
        for (name, pool, reads) in [("direct", direct_pool(), 1), ("proxied", proxied, 0)] {
            let store = stored_root();
            connector(pool, &store)
                .connections
                .credentials
                .issue(&request)
                .await
                .unwrap_or_else(|error| panic!("{name} issues credentials: {error}"));
            assert_eq!(store.reads(), reads, "{name}");
        }
    }

    /// A recorded Lenovo BMC served over TLS, the explored report and access
    /// metadata that reach it, and every `Authorization` header it receives.
    struct ExploredMock {
        _server: CombinedServer,
        access: BmcAccessInfo,
        report: EndpointExplorationReport,
        authorizations: Arc<Mutex<Vec<String>>>,
    }

    impl ExploredMock {
        fn serve() -> Self {
            let authorizations = Arc::new(Mutex::new(Vec::new()));
            let router = lenovo_thinksystem_sr670_router().layer(from_fn_with_state(
                authorizations.clone(),
                record_authorization,
            ));
            let (server, base_url) = serve_https("driver-connector", router);
            Self {
                _server: server,
                access: BmcAccessInfo {
                    host: "127.0.0.1".to_string(),
                    port: base_url.port(),
                    mac_address: MacAddress::new([2, 0, 0, 0, 0, 5]),
                },
                report: EndpointExplorationReport {
                    service_root: Some(ServiceRoot {
                        vendor: Some("Lenovo".to_string()),
                        ..Default::default()
                    }),
                    systems: vec![ComputerSystem {
                        id: "1".to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                authorizations,
            }
        }

        /// Whether the BMC received requests, all carrying `username` and `password`.
        fn authenticated_only_as(&self, username: &str, password: &str) -> bool {
            let (expected, _) = redfish_basic_authorization_context(username, Some(password));
            let authorizations = self.authorizations.lock().expect("lock is not poisoned");
            !authorizations.is_empty() && authorizations.iter().all(|seen| *seen == expected)
        }
    }

    async fn record_authorization(
        State(authorizations): State<Arc<Mutex<Vec<String>>>>,
        request: Request,
        next: Next,
    ) -> Response {
        if let Some(authorization) = request
            .headers()
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
        {
            authorizations
                .lock()
                .expect("lock is not poisoned")
                .push(authorization.to_string());
        }
        next.run(request).await
    }

    async fn power_state(bmc: ConnectedBmc<RedfishBmc>) -> Option<PowerState> {
        bmc.power()
            .expect("a power driver is selected")
            .state()
            .await
            .expect("the power state reads")
    }

    /// Connecting from an explored report authenticates with the stored root
    /// credentials and runs operations through the selected drivers.
    #[tokio::test]
    async fn drivers_backend_connects_from_an_explored_report() {
        let bmc = ExploredMock::serve();
        let store = stored_root();

        let connected = connector(direct_pool(), &store)
            .connect(&bmc.access, &bmc.report)
            .await
            .expect("the explored BMC connects");

        assert_eq!(power_state(connected).await, Some(PowerState::On));
        assert_eq!(store.reads(), 1);
        assert!(bmc.authenticated_only_as("root", "password"));
    }

    /// Explicit credentials reach the BMC directly even when the stored ones
    /// route through nico-bmc-proxy, and the stored ones are never read.
    #[tokio::test]
    async fn explicit_credentials_connect_directly_without_the_stored_ones() {
        let bmc = ExploredMock::serve();
        let store = stored_root();

        let connected = connector(proxied_pool(), &store)
            .connect_with_credentials(
                &bmc.access,
                &bmc.report,
                Credentials::new("operator", "secret"),
            )
            .await
            .expect("the explored BMC connects");

        assert_eq!(power_state(connected).await, Some(PowerState::On));
        assert_eq!(store.reads(), 0);
        assert!(bmc.authenticated_only_as("operator", "secret"));
    }

    #[test]
    fn ipmi_uses_the_first_managers_port_or_the_standard_one() {
        value_scenarios!(run = |ipmi_ports: Vec<Option<u16>>| {
            let report = EndpointExplorationReport {
                managers: ipmi_ports
                    .into_iter()
                    .map(|ipmi_port| Manager { ipmi_port, ..Default::default() })
                    .collect(),
                ..Default::default()
            };
            ipmi_address(&endpoint(BTreeSet::new()), &report).to_string()
        };
            "reported" {
                vec![Some(6230), Some(6231)] => "192.0.2.10:6230".to_string(),
            }
            "standard" {
                vec![] => "192.0.2.10:623".to_string(),
                vec![None, Some(6231)] => "192.0.2.10:623".to_string(),
            }
        );
    }

    #[test]
    fn only_unauthorized_and_forbidden_trigger_one_shot_refresh() {
        value_scenarios!(run = |error: PlatformError| is_auth_error(&error);
            "refresh once" {
                PlatformError::Auth(AuthError::InvalidCredentials) => true,
                PlatformError::Auth(AuthError::InsufficientPrivilege) => true,
            }
            "return directly" {
                PlatformError::Auth(AuthError::PasswordChangeRequired { account_uri: None }) => false,
                PlatformError::Unsupported => false,
                PlatformError::Unreachable => false,
                PlatformError::LockedDown => false,
            }
        );
    }

    #[tokio::test]
    async fn explicit_credentials_require_a_password() {
        let bmc = ExploredMock::serve();
        let result = connector(direct_pool(), &stored_root())
            .connect_with_credentials(&bmc.access, &bmc.report, Credentials::new("operator", ""))
            .await;
        assert!(matches!(
            result,
            Err(ExploredConnectError::Connect(ConnectError::Credentials(
                PlatformError::Auth(AuthError::CredentialsUnavailable)
            )))
        ));
    }
}
