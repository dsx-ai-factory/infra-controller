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
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use carbide_client_cache::{ClientCache, TlsMaterialEvent};
use carbide_secrets::SecretsError;
use carbide_secrets::credentials::{CredentialKey, CredentialReader, Credentials};
pub use iface::{
    Filter, GetPartitionOptions, IBFabric, IBFabricConfig, IBFabricManager, IBFabricRawResponse,
    IBFabricVersions,
};
pub use model::ib::{IBMtu, IBRateLimit, IBServiceLevel};

use crate::config::{self, UfmAuthMode};
use crate::errors::IbError;

mod disable;
mod iface;
mod rest;
mod ufmclient;

#[cfg(feature = "test-support")]
pub mod fakes;
#[cfg(feature = "test-support")]
mod mock;

#[derive(Copy, Clone, Default, PartialEq, Eq)]
pub enum IBFabricManagerType {
    #[default]
    Disable,
    #[cfg(feature = "test-support")]
    Mock,
    Rest,
}

pub struct IBFabricManagerImpl {
    config: IBFabricManagerConfig,
    credential_reader: Arc<dyn CredentialReader>,
    #[cfg(feature = "test-support")]
    mock_fabric: Arc<mock::MockIBFabric>,
    disable_fabric: Arc<dyn IBFabric>,
    /// Application-lifetime cache of built REST clients, one per fabric, keyed
    /// by fabric name and fingerprinted by [`fabric_client_fingerprint`].
    /// [`IBFabricManager::new_client`] reuses an entry until the fabric's
    /// credentials, endpoint, or on-disk TLS material change; see there for
    /// the full policy.
    rest_clients: ClientCache<String, u64, Arc<dyn IBFabric>>,
    /// Directory containing the SPIFFE certificate files selected by an empty
    /// UFM credential. Fixed for the manager's lifetime.
    /// Explicit client-certificate mode uses the same files without reading a credential.
    spiffe_cert_dir: PathBuf,
}

/// Authentication input hashed into a fabric client's cache fingerprint.
#[derive(Debug, Hash, PartialEq, Eq)]
enum FabricClientAuth {
    /// Explicit certificate authentication has no credential input.
    ClientCertificate,
    /// Token or legacy authentication retains the complete credential input.
    Credentials(Credentials),
}

/// Fingerprint of everything a REST client is built from by value: the
/// resolved endpoint and the resolved authentication input. Changed credentials
/// (secret rotation) produce a new fingerprint and therefore a rebuilt client. A
/// manager's endpoints are fixed for its lifetime, but the endpoint is hashed
/// too so the fingerprint covers every by-value input. The SPIFFE certificate
/// directory is a fixed path, not an input, so it is not hashed.
/// Certificate material lives on disk rather than in the fingerprint, so the
/// client cache tracks its rotation separately by modification time.
fn fabric_client_fingerprint(endpoint: &str, auth: &FabricClientAuth) -> u64 {
    let mut hasher = DefaultHasher::new();
    endpoint.hash(&mut hasher);
    auth.hash(&mut hasher);
    hasher.finish()
}

/// Logs the REST client cache's TLS material observations in UFM terms.
fn log_tls_material_event(event: TlsMaterialEvent<'_>) {
    match event {
        TlsMaterialEvent::Unreadable { path, error } => tracing::debug!(
            path = %path.display(),
            %error,
            "could not read UFM TLS material mtime; keeping the cached client"
        ),
        TlsMaterialEvent::Newer { path } => tracing::info!(
            path = %path.display(),
            "UFM TLS material changed on disk; rebuilding the fabric client to pick it up"
        ),
    }
}

impl IBFabricManagerImpl {
    /// Gets the mocked fabric manager that is used within tests
    #[cfg(feature = "test-support")]
    pub fn get_mock_manager(&self) -> Arc<mock::MockIBFabric> {
        self.mock_fabric.clone()
    }

    /// Resolves one fabric's runtime authentication and matching cache input so explicit policy
    /// cannot diverge from client reuse behavior.
    async fn resolve_auth_method(
        &self,
        fabric_name: &str,
    ) -> Result<(rest::AuthMethod, FabricClientAuth), IbError> {
        let configured_auth = self.config.auth_modes.get(fabric_name).copied();

        // Explicit certificate mode has no secret input and must work without a stored credential.
        if configured_auth == Some(UfmAuthMode::ClientCertificate) {
            return Ok((
                rest::client_certificate_auth(&self.spiffe_cert_dir),
                FabricClientAuth::ClientCertificate,
            ));
        }

        // Token and legacy modes share the existing reader error classification.
        let key = &CredentialKey::UfmAuth {
            fabric: fabric_name.to_string(),
        };
        let token_required = configured_auth == Some(UfmAuthMode::Token);
        let token_required_error = || {
            IbError::internal(format!(
                "Cannot create UFM client: token is required because auth is \"token\": {}",
                key.to_key_str()
            ))
        };
        let credentials = self
            .credential_reader
            .get_credentials(key)
            .await
            .map_err(|error| match error {
                error @ SecretsError::UfmCredentialReadBlocked { .. } => {
                    IbError::CredentialConfigurationError(error.to_string())
                }
                error => IbError::internal(format!(
                    "Cannot create UFM client: secret manager error: {error}"
                )),
            })?
            .ok_or_else(|| {
                if token_required {
                    token_required_error()
                } else {
                    IbError::internal(format!(
                        "Cannot create UFM client: vault key not found or token is not set: {}",
                        key.to_key_str()
                    ))
                }
            })?;

        let Credentials::UsernamePassword { password, .. } = &credentials;
        // Explicit token mode rejects blank values; omission preserves legacy certificate selection.
        let auth_method = if token_required {
            if password.trim().is_empty() {
                return Err(token_required_error());
            }
            rest::AuthMethod::Token(password.clone())
        } else {
            rest::auth_method(password, &self.spiffe_cert_dir)
        };

        Ok((auth_method, FabricClientAuth::Credentials(credentials)))
    }
}

#[derive(Clone)]
pub struct IBFabricManagerConfig {
    /// List of endpoint per fabric
    pub endpoints: HashMap<String, Vec<String>>,
    /// Explicit UFM authentication modes by fabric; absence preserves legacy selection.
    pub auth_modes: HashMap<String, UfmAuthMode>,
    pub manager_type: IBFabricManagerType,
    pub max_partition_per_tenant: i32,
    pub mtu: IBMtu,
    pub rate_limit: IBRateLimit,
    pub service_level: IBServiceLevel,
    pub allow_insecure_fabric_configuration: bool,
    /// The interval at which ib fabric monitor runs
    pub fabric_manager_run_interval: std::time::Duration,
}

impl Default for IBFabricManagerConfig {
    fn default() -> Self {
        IBFabricManagerConfig {
            allow_insecure_fabric_configuration: false,
            endpoints: HashMap::default(),
            auth_modes: HashMap::default(),
            manager_type: IBFabricManagerType::default(),
            max_partition_per_tenant: config::IBFabricConfig::default_max_partition_per_tenant(),
            mtu: IBMtu::default(),
            rate_limit: IBRateLimit::default(),
            service_level: IBServiceLevel::default(),
            fabric_manager_run_interval:
                config::IBFabricConfig::default_fabric_monitor_run_interval(),
        }
    }
}

pub fn create_ib_fabric_manager(
    credential_reader: Arc<dyn CredentialReader>,
    config: IBFabricManagerConfig,
) -> Result<IBFabricManagerImpl, eyre::Report> {
    for (fabric_id, endpoints) in config.endpoints.iter() {
        if endpoints.len() != 1 {
            return Err(eyre::eyre!(
                "exactly 1 endpoint can be specified for each IB fabric. fabric \"{fabric_id}\" specifies endpoints: {}",
                endpoints.clone().join(",")
            ));
        }

        for ep in endpoints.iter() {
            if ep.parse::<http::Uri>().is_err() {
                return Err(eyre::eyre!(
                    "endpoint \"{ep}\" for fabric \"{fabric_id}\" is not a valid HTTP(S) URI. expected format is https://1.2.3.4:443 ?"
                ));
            }
        }
    }

    #[cfg(feature = "test-support")]
    let mock_fabric = Arc::new(mock::MockIBFabric::new());

    let disable_fabric = Arc::new(disable::DisableIBFabric {});

    Ok(IBFabricManagerImpl {
        credential_reader,
        config,
        #[cfg(feature = "test-support")]
        mock_fabric,
        disable_fabric,
        rest_clients: ClientCache::new(log_tls_material_event),
        spiffe_cert_dir: PathBuf::from(rest::SPIFFE_CERT_DIR),
    })
}

#[async_trait]
impl IBFabricManager for IBFabricManagerImpl {
    fn get_config(&self) -> IBFabricManagerConfig {
        self.config.clone()
    }

    /// Returns the client for `fabric_name`, building one only when needed.
    ///
    /// The `Rest` client lives for the life of the process: every call still
    /// reads the fabric's credentials from the secret manager -- except for
    /// explicit client-certificate mode -- so token rotations are picked up
    /// promptly. The client is rebuilt only when the resolved authentication
    /// input or endpoint differs from the cached client. When `auth` is not set,
    /// an empty credential selects certificate authentication, whose TLS
    /// material is the SPIFFE files on disk rather than part of the credential,
    /// so a rebuild also happens once any file is newer than the cached client
    /// (certificate rotation), unless another is unreadable, such as one
    /// missing mid-rotation.
    /// [`ClientCache::get_or_build`] owns that lifecycle. Between rebuilds all
    /// callers share one client, whose HTTP pool keeps connections warm across
    /// monitor passes. (`Disable` and `Mock` hand out process-wide shared
    /// instances anyway.)
    ///
    /// Callers race here: partition state handlers run concurrently, alongside
    /// the fabric monitor and API handlers. Concurrent misses for the same
    /// fabric may each build a client, last insert wins -- a benign duplicate
    /// build that opens no connection, since the client connects lazily on
    /// first use -- so per-fabric in-flight serialization is not worth adding.
    async fn new_client(&self, fabric_name: &str) -> Result<Arc<dyn IBFabric>, IbError> {
        match self.config.manager_type {
            IBFabricManagerType::Disable => Ok(self.disable_fabric.clone()),
            #[cfg(feature = "test-support")]
            IBFabricManagerType::Mock => Ok(self.mock_fabric.clone()),
            IBFabricManagerType::Rest => {
                let endpoint = self
                    .config
                    .endpoints
                    .get(fabric_name)
                    .and_then(|fabric_endpoints| fabric_endpoints.first())
                    .ok_or_else(|| IbError::NotFoundError {
                        kind: "ib_fabric_endpoint",
                        id: fabric_name.to_string(),
                    })?;

                // Resolve the auth method once and build from that resolution,
                // so the client loads exactly the on-disk material (if any)
                // this call watches for rotation. Authentication selection
                // and the fingerprint come from the same policy decision.
                let (auth_method, fingerprint_auth) = self.resolve_auth_method(fabric_name).await?;
                let fingerprint = fabric_client_fingerprint(endpoint, &fingerprint_auth);
                let tls_material: Vec<&Path> = match &auth_method {
                    rest::AuthMethod::Cert(cert) => vec![
                        Path::new(&cert.ca_crt),
                        Path::new(&cert.tls_key),
                        Path::new(&cert.tls_crt),
                    ],
                    rest::AuthMethod::Token(_) => Vec::new(),
                };

                self.rest_clients
                    .get_or_build(fabric_name, fingerprint, &tls_material, || async {
                        rest::new_client(endpoint, auth_method.clone())
                    })
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::SystemTime;

    use async_trait::async_trait;
    use carbide_secrets::SecretsError;
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases_async, scenarios};
    use opentelemetry::global;
    use ufm_mock::{UfmAuthToken, UfmMock, UfmMockConfig};

    use super::*;

    struct NoopCredentialReader;

    #[async_trait]
    impl CredentialReader for NoopCredentialReader {
        async fn get_credentials(
            &self,
            _key: &CredentialKey,
        ) -> Result<Option<Credentials>, SecretsError> {
            Ok(None)
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum ManagerCase {
        ValidDisabled,
        EmptyEndpointList,
        MultipleEndpoints,
        InvalidEndpoint,
    }

    #[derive(Debug, PartialEq)]
    struct ManagerConfigSummary {
        endpoint_count: usize,
        manager_type: &'static str,
        max_partition_per_tenant: i32,
        allow_insecure_fabric_configuration: bool,
        fabric_manager_run_interval_secs: u64,
    }

    fn credential_reader() -> Arc<dyn CredentialReader> {
        Arc::new(NoopCredentialReader)
    }

    fn manager_type_name(manager_type: IBFabricManagerType) -> &'static str {
        match manager_type {
            IBFabricManagerType::Disable => "disable",
            #[cfg(feature = "test-support")]
            IBFabricManagerType::Mock => "mock",
            IBFabricManagerType::Rest => "rest",
        }
    }

    fn summarize_manager_config(config: IBFabricManagerConfig) -> ManagerConfigSummary {
        ManagerConfigSummary {
            endpoint_count: config.endpoints.len(),
            manager_type: manager_type_name(config.manager_type),
            max_partition_per_tenant: config.max_partition_per_tenant,
            allow_insecure_fabric_configuration: config.allow_insecure_fabric_configuration,
            fabric_manager_run_interval_secs: config.fabric_manager_run_interval.as_secs(),
        }
    }

    fn config_for_case(case: ManagerCase) -> IBFabricManagerConfig {
        let mut config = IBFabricManagerConfig::default();
        match case {
            ManagerCase::ValidDisabled => {
                config.endpoints.insert(
                    "fabric-a".to_string(),
                    vec!["https://127.0.0.1:443".to_string()],
                );
            }
            ManagerCase::EmptyEndpointList => {
                config.endpoints.insert("fabric-a".to_string(), vec![]);
            }
            ManagerCase::MultipleEndpoints => {
                config.endpoints.insert(
                    "fabric-a".to_string(),
                    vec![
                        "https://127.0.0.1:443".to_string(),
                        "https://127.0.0.2:443".to_string(),
                    ],
                );
            }
            ManagerCase::InvalidEndpoint => {
                config
                    .endpoints
                    .insert("fabric-a".to_string(), vec!["not a uri".to_string()]);
            }
        }
        config
    }

    fn create_manager(case: ManagerCase) -> Result<ManagerConfigSummary, &'static str> {
        create_ib_fabric_manager(credential_reader(), config_for_case(case))
            .map(|manager| summarize_manager_config(manager.get_config()))
            .map_err(manager_error_kind)
    }

    fn manager_error_kind(error: eyre::Report) -> &'static str {
        let error = error.to_string();
        if error.contains("exactly 1 endpoint") {
            "endpoint-count"
        } else if error.contains("not a valid HTTP(S) URI") {
            "invalid-uri"
        } else {
            "unknown"
        }
    }

    #[test]
    fn default_manager_config_uses_disabled_defaults() {
        assert_eq!(
            summarize_manager_config(IBFabricManagerConfig::default()),
            ManagerConfigSummary {
                endpoint_count: 0,
                manager_type: "disable",
                max_partition_per_tenant: config::IBFabricConfig::default_max_partition_per_tenant(
                ),
                allow_insecure_fabric_configuration: false,
                fabric_manager_run_interval_secs: 60,
            }
        );
    }

    #[test]
    fn validates_manager_endpoints() {
        scenarios!(create_manager:
            "valid config" {
                ManagerCase::ValidDisabled => Yields(ManagerConfigSummary {
                    endpoint_count: 1,
                    manager_type: "disable",
                    max_partition_per_tenant: config::IBFabricConfig::default_max_partition_per_tenant(),
                    allow_insecure_fabric_configuration: false,
                    fabric_manager_run_interval_secs: 60,
                }),
            }

            "invalid endpoints" {
                ManagerCase::EmptyEndpointList => FailsWith("endpoint-count"),
                ManagerCase::MultipleEndpoints => FailsWith("endpoint-count"),
                ManagerCase::InvalidEndpoint => FailsWith("invalid-uri"),
            }
        );
    }

    #[tokio::test]
    async fn disabled_manager_returns_disabled_client() {
        let manager =
            create_ib_fabric_manager(credential_reader(), IBFabricManagerConfig::default())
                .unwrap();
        let client = manager.new_client("fabric-a").await.unwrap();

        assert_eq!(
            client.get_fabric_config().await.unwrap_err().to_string(),
            "failed to call IBFabricManager: ib fabric is disabled"
        );
    }

    // ============================================================
    // Unit Tests for the application-lifetime REST client cache
    // ============================================================

    /// A credential reader whose returned credentials the test can swap,
    /// standing in for a secret-manager rotation between `new_client` calls.
    struct SwappableCredentialReader {
        credentials: Mutex<Credentials>,
    }

    impl SwappableCredentialReader {
        fn new(token: &str) -> Self {
            Self {
                credentials: Mutex::new(Self::credentials(token)),
            }
        }

        fn credentials(token: &str) -> Credentials {
            Credentials::UsernamePassword {
                username: "ufm-user".to_string(),
                password: token.to_string(),
            }
        }

        fn rotate(&self, token: &str) {
            *self.credentials.lock().unwrap() = Self::credentials(token);
        }
    }

    #[async_trait]
    impl CredentialReader for SwappableCredentialReader {
        async fn get_credentials(
            &self,
            _key: &CredentialKey,
        ) -> Result<Option<Credentials>, SecretsError> {
            Ok(Some(self.credentials.lock().unwrap().clone()))
        }
    }

    /// A `Rest` manager over two fabrics that share one endpoint, so only the
    /// fabric name tells their cache entries apart. Building a REST client
    /// makes no connection (it connects lazily on first use), so these tests
    /// exercise the real build + cache path without a UFM server.
    fn rest_manager(reader: Arc<dyn CredentialReader>) -> IBFabricManagerImpl {
        let config = IBFabricManagerConfig {
            manager_type: IBFabricManagerType::Rest,
            endpoints: HashMap::from([
                ("f1".to_string(), vec!["https://127.0.0.1:443".to_string()]),
                ("f2".to_string(), vec!["https://127.0.0.1:443".to_string()]),
            ]),
            ..Default::default()
        };
        create_ib_fabric_manager(reader, config).unwrap()
    }

    /// Counts credential lookups while returning one fixed optional password so explicit
    /// certificate mode can prove it bypasses the secret store.
    struct CountingCredentialReader {
        credentials: Option<Credentials>,
        calls: AtomicUsize,
    }

    impl CountingCredentialReader {
        /// Creates a reader whose optional password represents one credential-state table row.
        fn new(password: Option<&str>) -> Self {
            Self {
                credentials: password.map(|password| Credentials::new("", password)),
                calls: AtomicUsize::new(0),
            }
        }

        /// Returns the number of lookups so tests can distinguish bypass from a successful read.
        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl CredentialReader for CountingCredentialReader {
        async fn get_credentials(
            &self,
            _key: &CredentialKey,
        ) -> Result<Option<Credentials>, SecretsError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.credentials.clone())
        }
    }

    /// Describes the observable authentication result expected from one policy and credential.
    #[derive(Clone, Copy, Debug)]
    enum ExpectedAuth {
        Certificate,
        Token(&'static str),
        Error(&'static str),
    }

    /// Defines one auth-policy and credential-state combination for each distinct behavior.
    #[derive(Clone, Copy, Debug)]
    struct AuthResolutionCase {
        name: &'static str,
        mode: Option<UfmAuthMode>,
        password: Option<&'static str>,
        expected: ExpectedAuth,
        expected_reads: usize,
    }

    /// Verifies explicit auth policy selects certificate mode without a read and requires a
    /// nonblank credential for token mode.
    #[tokio::test]
    async fn resolves_configured_auth_against_every_credential_state() {
        const TOKEN_REQUIRED: &str = "token is required because auth is \"token\": ufm/fabric/auth";
        let cases = [
            // Certificate policy never reads the credential, so the result is also unchanged when
            // nothing is stored.
            AuthResolutionCase {
                name: "certificate with ordinary token",
                mode: Some(UfmAuthMode::ClientCertificate),
                password: Some("ordinary-token"),
                expected: ExpectedAuth::Certificate,
                expected_reads: 0,
            },
            // Explicit token mode must report a missing credential rather than falling back to certificates.
            AuthResolutionCase {
                name: "token without credential",
                mode: Some(UfmAuthMode::Token),
                password: None,
                expected: ExpectedAuth::Error(TOKEN_REQUIRED),
                expected_reads: 1,
            },
            // An exactly empty password must fail the explicit token contract after a successful read.
            AuthResolutionCase {
                name: "token with empty credential",
                mode: Some(UfmAuthMode::Token),
                password: Some(""),
                expected: ExpectedAuth::Error(TOKEN_REQUIRED),
                expected_reads: 1,
            },
            // Whitespace-only input must fail token validation just like an exactly empty password.
            AuthResolutionCase {
                name: "token with whitespace credential",
                mode: Some(UfmAuthMode::Token),
                password: Some(" \t"),
                expected: ExpectedAuth::Error(TOKEN_REQUIRED),
                expected_reads: 1,
            },
            // A nonblank credential must select token authentication in explicit token mode.
            AuthResolutionCase {
                name: "token with ordinary token",
                mode: Some(UfmAuthMode::Token),
                password: Some("ordinary-token"),
                expected: ExpectedAuth::Token("ordinary-token"),
                expected_reads: 1,
            },
        ];

        for case in cases {
            // Build one REST manager with only the case's explicit mode and credential state.
            let reader = Arc::new(CountingCredentialReader::new(case.password));
            let mut config = IBFabricManagerConfig {
                manager_type: IBFabricManagerType::Rest,
                endpoints: HashMap::from([(
                    "fabric".to_string(),
                    vec!["https://ufm.example".to_string()],
                )]),
                ..Default::default()
            };
            if let Some(mode) = case.mode {
                config.auth_modes.insert("fabric".to_string(), mode);
            }
            let manager = create_ib_fabric_manager(reader.clone(), config)
                .unwrap_or_else(|error| panic!("{}: create manager: {error}", case.name));

            // Resolve without building a network client so the table observes policy directly.
            let result = manager.resolve_auth_method("fabric").await;

            // Check the selected method or stable error and prove whether the reader was consulted.
            match (case.expected, result) {
                (ExpectedAuth::Certificate, Ok((rest::AuthMethod::Cert(_), _))) => {}
                (ExpectedAuth::Token(expected), Ok((rest::AuthMethod::Token(actual), _))) => {
                    assert_eq!(actual, expected, "{}", case.name);
                }
                (ExpectedAuth::Error(expected), Err(error)) => {
                    assert!(
                        error.to_string().contains(expected),
                        "{}: {error}",
                        case.name
                    );
                }
                (_, result) => panic!("{}: unexpected result: {}", case.name, result.is_ok()),
            }
            assert_eq!(reader.call_count(), case.expected_reads, "{}", case.name);
        }
    }

    /// Verifies manager-built token clients authenticate real mock requests,
    /// including directory-shaped tokens and rejected stale credentials.
    #[tokio::test]
    async fn manager_token_authentication_works_with_the_ufm_mock() {
        #[derive(Debug, PartialEq)]
        enum RequestOutcome {
            Accepted,
            AuthenticationFailed,
        }

        let existing_dir = tempfile::tempdir().expect("temp dir");
        let existing_path = existing_dir.path().to_string_lossy().into_owned();
        check_cases_async(
            [
                // An ordinary token accepted by UFM must authenticate the request.
                Case {
                    scenario: "accepted ordinary token",
                    input: ("accepted-token".to_string(), "accepted-token".to_string()),
                    expect: Yields(RequestOutcome::Accepted),
                },
                // An existing directory path must authenticate successfully as a token.
                Case {
                    scenario: "accepted existing directory path token",
                    input: (existing_path.clone(), existing_path),
                    expect: Yields(RequestOutcome::Accepted),
                },
                // A stale directory credential must surface UFM's authentication rejection.
                Case {
                    scenario: "rejected stale credential",
                    input: ("stale-credential".to_string(), "accepted-token".to_string()),
                    expect: Yields(RequestOutcome::AuthenticationFailed),
                },
            ],
            |(credential, accepted_token)| async move {
                let auth_token =
                    UfmAuthToken::new(accepted_token).map_err(|error| error.to_string())?;
                let ufm_mock = UfmMock::new(
                    &UfmMockConfig::default(),
                    &auth_token,
                    &global::meter("ib-fabric-manager-token-authentication-test"),
                )
                .map_err(|error| error.to_string())?;
                let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                    .await
                    .map_err(|error| format!("bind UFM mock: {error}"))?;
                let address = listener
                    .local_addr()
                    .map_err(|error| format!("read UFM mock address: {error}"))?;
                let server =
                    tokio::spawn(async move { axum::serve(listener, ufm_mock.router()).await });
                let config = IBFabricManagerConfig {
                    manager_type: IBFabricManagerType::Rest,
                    endpoints: HashMap::from([(
                        "f1".to_string(),
                        vec![format!("http://{address}")],
                    )]),
                    ..Default::default()
                };
                let manager = create_ib_fabric_manager(
                    Arc::new(SwappableCredentialReader::new(&credential)),
                    config,
                )
                .map_err(|error| format!("create manager: {error}"))?;
                let client = manager
                    .new_client("f1")
                    .await
                    .map_err(|error| format!("create client: {error}"))?;

                let result = client.versions().await;
                server.abort();
                match result {
                    Ok(_) => Ok(RequestOutcome::Accepted),
                    Err(error) if error.to_string().contains("HTTP error code 401") => {
                        Ok(RequestOutcome::AuthenticationFailed)
                    }
                    Err(error) => Err(format!("unexpected request failure: {error}")),
                }
            },
        )
        .await;
    }

    /// Verifies a production manager resolves an empty credential to the fixed
    /// SPIFFE files, because default authentication must not depend on disk state.
    #[test]
    fn production_manager_resolves_the_default_spiffe_material() {
        let config = IBFabricManagerConfig {
            manager_type: IBFabricManagerType::Rest,
            ..Default::default()
        };
        let manager =
            create_ib_fabric_manager(Arc::new(SwappableCredentialReader::new("")), config)
                .expect("create manager");

        assert_eq!(
            manager.spiffe_cert_dir,
            PathBuf::from(rest::SPIFFE_CERT_DIR)
        );
        let rest::AuthMethod::Cert(cert) = rest::auth_method("", &manager.spiffe_cert_dir) else {
            panic!("empty credential must select certificate authentication");
        };
        assert_eq!(
            [cert.ca_crt, cert.tls_key, cert.tls_crt],
            [
                format!("{}/ca.crt", rest::SPIFFE_CERT_DIR),
                format!("{}/tls.key", rest::SPIFFE_CERT_DIR),
                format!("{}/tls.crt", rest::SPIFFE_CERT_DIR),
            ]
        );
    }

    struct BlockedUfmCredentialReader;

    #[async_trait]
    impl CredentialReader for BlockedUfmCredentialReader {
        async fn get_credentials(
            &self,
            key: &CredentialKey,
        ) -> Result<Option<Credentials>, SecretsError> {
            let CredentialKey::UfmAuth { fabric } = key else {
                return Ok(None);
            };
            Err(SecretsError::UfmCredentialReadBlocked {
                fabric: fabric.clone(),
            })
        }
    }

    struct FailingCredentialReader;

    #[async_trait]
    impl CredentialReader for FailingCredentialReader {
        async fn get_credentials(
            &self,
            _key: &CredentialKey,
        ) -> Result<Option<Credentials>, SecretsError> {
            Err(SecretsError::GenericError(eyre::eyre!(
                "simulated credential read failure"
            )))
        }
    }

    #[tokio::test]
    async fn blocked_ufm_credential_is_a_configuration_error() {
        let manager = rest_manager(Arc::new(BlockedUfmCredentialReader));

        let error = manager
            .new_client("f1")
            .await
            .err()
            .expect("authoritative local credential miss must fail client creation");

        assert!(matches!(error, IbError::CredentialConfigurationError(_)));
        assert!(error.to_string().contains("credentials.ufm_source"));
    }

    /// Verifies each remaining REST setup failure keeps its error variant and
    /// useful context, so callers can distinguish endpoint and credential failures.
    #[tokio::test]
    async fn rest_client_setup_failures_preserve_their_context() {
        #[derive(Clone, Copy, Debug)]
        enum FailureCase {
            MissingEndpoint,
            CredentialReadFailure,
            MissingCredential,
        }

        check_cases_async(
            [
                // An unknown fabric must report that its endpoint is missing.
                Case {
                    scenario: "fabric endpoint is not configured",
                    input: (FailureCase::MissingEndpoint, "ib_fabric_endpoint not found"),
                    expect: Yields(("not-found", true)),
                },
                // A general secret-manager failure must remain an internal client-creation error.
                Case {
                    scenario: "credential reader fails",
                    input: (
                        FailureCase::CredentialReadFailure,
                        "Cannot create UFM client: secret manager error",
                    ),
                    expect: Yields(("internal", true)),
                },
                // An absent credential must explain that the key or token was not set.
                Case {
                    scenario: "credential is absent",
                    input: (
                        FailureCase::MissingCredential,
                        "vault key not found or token is not set",
                    ),
                    expect: Yields(("internal", true)),
                },
            ],
            |(case, stable_message)| async move {
                let (manager, fabric_name) = match case {
                    FailureCase::MissingEndpoint => (
                        rest_manager(Arc::new(SwappableCredentialReader::new("token"))),
                        "missing-fabric",
                    ),
                    FailureCase::CredentialReadFailure => {
                        (rest_manager(Arc::new(FailingCredentialReader)), "f1")
                    }
                    FailureCase::MissingCredential => {
                        (rest_manager(Arc::new(NoopCredentialReader)), "f1")
                    }
                };
                let error = manager
                    .new_client(fabric_name)
                    .await
                    .err()
                    .ok_or_else(|| format!("{case:?} unexpectedly built a client"))?;
                let (variant, message) = match error {
                    IbError::NotFoundError { kind, id } => {
                        ("not-found", format!("{kind} not found: {id}"))
                    }
                    IbError::Internal { message } => ("internal", message),
                    error => return Err(format!("unexpected error variant: {error}")),
                };

                Ok::<_, String>((variant, message.contains(stable_message)))
            },
        )
        .await;
    }

    #[tokio::test]
    async fn unchanged_credentials_reuse_the_cached_rest_client() {
        let manager = rest_manager(Arc::new(SwappableCredentialReader::new("token-a")));

        let first = manager.new_client("f1").await.unwrap();
        let second = manager.new_client("f1").await.unwrap();

        assert!(
            Arc::ptr_eq(&first, &second),
            "unchanged credentials must return the cached client, not a rebuild"
        );
    }

    #[tokio::test]
    async fn rotated_credentials_rebuild_the_rest_client() {
        let reader = Arc::new(SwappableCredentialReader::new("token-a"));
        let manager = rest_manager(reader.clone());

        let before = manager.new_client("f1").await.unwrap();
        reader.rotate("token-b");
        let after = manager.new_client("f1").await.unwrap();
        let after_again = manager.new_client("f1").await.unwrap();

        assert!(
            !Arc::ptr_eq(&before, &after),
            "rotated credentials must rebuild the client"
        );
        assert!(
            Arc::ptr_eq(&after, &after_again),
            "the rebuilt client replaces the cache entry"
        );
    }

    /// Verifies switching between token and certificate authentication rebuilds
    /// the client, because the credential change must replace the cached method.
    #[tokio::test]
    async fn changed_auth_method_rebuilds_the_rest_client() {
        check_cases_async(
            [
                // With auth omitted, switching from a token to an empty password selects
                // certificate authentication.
                Case {
                    scenario: "token to certificate authentication",
                    input: ("token-a", ""),
                    expect: Yields((true, true, true)),
                },
                // With auth omitted, switching from an empty password to a token selects token
                // authentication.
                Case {
                    scenario: "certificate to token authentication",
                    input: ("", "token-a"),
                    expect: Yields((true, true, true)),
                },
            ],
            |(initial_credential, changed_credential)| async move {
                let dir = cert_material_dir();
                let reader = Arc::new(SwappableCredentialReader::new(initial_credential));
                let mut manager = rest_manager(reader.clone());
                manager.spiffe_cert_dir = dir.path().to_path_buf();

                let before = manager
                    .new_client("f1")
                    .await
                    .map_err(|error| format!("first build: {error}"))?;
                let before_again = manager
                    .new_client("f1")
                    .await
                    .map_err(|error| format!("first reuse: {error}"))?;
                let reused_before_change = Arc::ptr_eq(&before, &before_again);

                reader.rotate(changed_credential);
                let after = manager
                    .new_client("f1")
                    .await
                    .map_err(|error| format!("rebuild: {error}"))?;
                let rebuilt = !Arc::ptr_eq(&before, &after);
                let after_again = manager
                    .new_client("f1")
                    .await
                    .map_err(|error| format!("rebuilt reuse: {error}"))?;
                let reused_after_change = Arc::ptr_eq(&after, &after_again);

                Ok::<_, String>((reused_before_change, rebuilt, reused_after_change))
            },
        )
        .await;
    }

    /// Verifies endpoint changes remain part of the cache identity for the same auth input.
    #[test]
    fn fingerprint_covers_the_endpoint() {
        let auth = FabricClientAuth::Credentials(SwappableCredentialReader::credentials("token-a"));

        assert_ne!(
            fabric_client_fingerprint("https://127.0.0.1:443", &auth),
            fabric_client_fingerprint("https://127.0.0.2:443", &auth),
            "the same credentials at another endpoint need another client"
        );
    }

    /// Verifies explicit certificate and token policies produce distinct cache inputs so an auth
    /// mode change rebuilds the client.
    #[test]
    fn fingerprint_distinguishes_certificate_and_token_auth() {
        let endpoint = "https://ufm.example";
        let certificate = FabricClientAuth::ClientCertificate;
        let token =
            FabricClientAuth::Credentials(SwappableCredentialReader::credentials("token-a"));

        // Explicit certificate policy must never reuse a token-authenticated client.
        assert_ne!(
            fabric_client_fingerprint(endpoint, &certificate),
            fabric_client_fingerprint(endpoint, &token),
        );
    }

    #[tokio::test]
    async fn distinct_fabrics_cache_distinct_rest_clients() {
        let manager = rest_manager(Arc::new(SwappableCredentialReader::new("token-a")));

        let f1 = manager.new_client("f1").await.unwrap();
        let f2 = manager.new_client("f2").await.unwrap();

        assert!(!Arc::ptr_eq(&f1, &f2), "each fabric gets its own client");
        assert!(
            Arc::ptr_eq(&f1, &manager.new_client("f1").await.unwrap()),
            "reusing one fabric leaves its entry in place"
        );
        assert!(
            Arc::ptr_eq(&f2, &manager.new_client("f2").await.unwrap()),
            "and does not disturb the other fabric's entry"
        );
    }

    /// Certificate authentication loads a directory of material files. The
    /// staleness check only reads mtimes, but building the client opens all
    /// three files and insists the key parses as a PEM private key, so give it
    /// minimal PEM armor (the DER payload is never validated: the empty cert
    /// list routes the build to its no-client-auth config).
    fn cert_material_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("ca.crt"), "").expect("write ca.crt");
        std::fs::write(dir.path().join("tls.crt"), "").expect("write tls.crt");
        std::fs::write(
            dir.path().join("tls.key"),
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
        )
        .expect("write tls.key");
        dir
    }

    /// Builds a certificate-auth manager against temporary SPIFFE material so
    /// rotation can be tested without using the process-wide default files.
    fn cert_manager(dir: &tempfile::TempDir) -> IBFabricManagerImpl {
        let mut manager = rest_manager(Arc::new(SwappableCredentialReader::new("")));
        manager.spiffe_cert_dir = dir.path().to_path_buf();
        manager
    }

    /// Verifies missing SPIFFE material names the absent file and fails client
    /// creation, so certificate authentication cannot silently become token authentication.
    #[tokio::test]
    async fn missing_cert_material_fails_with_the_file_name() {
        check_cases_async(
            [
                // A missing trust root must identify the CA certificate.
                Case {
                    scenario: "missing CA certificate",
                    input: "ca.crt",
                    expect: Yields(()),
                },
                // A missing client certificate must identify the identity certificate.
                Case {
                    scenario: "missing client certificate",
                    input: "tls.crt",
                    expect: Yields(()),
                },
                // A missing private key must identify the client key.
                Case {
                    scenario: "missing client key",
                    input: "tls.key",
                    expect: Yields(()),
                },
            ],
            |missing| async move {
                let dir = cert_material_dir();
                std::fs::remove_file(dir.path().join(missing))
                    .map_err(|error| format!("remove {missing}: {error}"))?;
                let manager = cert_manager(&dir);

                let error = manager
                    .new_client("f1")
                    .await
                    .err()
                    .ok_or_else(|| format!("missing {missing} unexpectedly built a client"))?;
                let message = error.to_string();
                if !message.contains(missing) {
                    return Err(format!("error does not name {missing}: {message}"));
                }
                Ok::<_, String>(())
            },
        )
        .await;
    }

    /// Each certificate file the credentials select is watched: rotating any
    /// one of them rebuilds the cached client.
    #[tokio::test]
    async fn newer_cert_material_rebuilds_the_client() {
        check_cases_async(
            [
                // Rotating the trust root must rebuild the TLS configuration.
                Case {
                    scenario: "rotated CA certificate",
                    input: "ca.crt",
                    expect: Yields((true, true)),
                },
                // Rotating the private key must rebuild the client identity.
                Case {
                    scenario: "rotated client key",
                    input: "tls.key",
                    expect: Yields((true, true)),
                },
                // Rotating the client certificate must rebuild the client identity.
                Case {
                    scenario: "rotated client certificate",
                    input: "tls.crt",
                    expect: Yields((true, true)),
                },
            ],
            // Whether the client is reused before `rotated` changes, and
            // rebuilt after.
            |rotated| async move {
                let dir = cert_material_dir();
                let manager = cert_manager(&dir);

                let before = manager
                    .new_client("f1")
                    .await
                    .map_err(|error| format!("first build: {error}"))?;
                let again = manager
                    .new_client("f1")
                    .await
                    .map_err(|error| format!("reuse: {error}"))?;
                let reused = Arc::ptr_eq(&before, &again);

                // Give the file a modification time strictly after the cached
                // client's creation. An explicit future timestamp avoids
                // depending on the filesystem's mtime resolution.
                std::fs::File::options()
                    .write(true)
                    .open(dir.path().join(rotated))
                    .expect("open TLS material")
                    .set_modified(SystemTime::now() + std::time::Duration::from_secs(60))
                    .expect("set TLS material mtime");

                let after = manager
                    .new_client("f1")
                    .await
                    .map_err(|error| format!("rebuild: {error}"))?;
                let rebuilt = !Arc::ptr_eq(&before, &after);
                Ok::<_, String>((reused, rebuilt))
            },
        )
        .await;
    }
}
