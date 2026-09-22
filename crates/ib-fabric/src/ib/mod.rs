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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use async_trait::async_trait;
use carbide_secrets::SecretsError;
use carbide_secrets::credentials::{CredentialKey, CredentialReader, Credentials};
pub use iface::{
    Filter, GetPartitionOptions, IBFabric, IBFabricConfig, IBFabricManager, IBFabricRawResponse,
    IBFabricVersions,
};
pub use model::ib::{IBMtu, IBRateLimit, IBServiceLevel};

use crate::config;
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
    /// Application-lifetime cache of built REST clients, one per fabric.
    /// [`IBFabricManager::new_client`] reuses an entry until the fabric's
    /// credentials, endpoint, or on-disk TLS material change; see there for
    /// the full policy.
    rest_clients: Mutex<HashMap<String, CachedFabricClient>>,
}

/// A built REST client together with the fingerprint of the endpoint and
/// credentials it was built from, and when it was built. The client is reused
/// only while a fresh credential read still matches that fingerprint and, for
/// certificate-authenticated fabrics, no TLS material file on disk is newer
/// than `created`.
struct CachedFabricClient {
    fingerprint: u64,
    created: SystemTime,
    client: Arc<dyn IBFabric>,
}

/// Fingerprint of everything a REST client is built from by value: the
/// resolved endpoint and the fabric's credentials. A changed endpoint (config
/// reload) or changed credentials (secret rotation) produce a new fingerprint
/// and therefore a rebuilt client. Certificate material selected by the
/// credentials lives on disk rather than in them, so its rotation is tracked
/// separately via [`cert_material_newer_than`].
fn fabric_client_fingerprint(endpoint: &str, credentials: &Credentials) -> u64 {
    let mut hasher = DefaultHasher::new();
    endpoint.hash(&mut hasher);
    credentials.hash(&mut hasher);
    hasher.finish()
}

/// True when any of the client-certificate files in `cert` was modified after
/// `created`, i.e. a client built at that time no longer reflects the material
/// a fresh build would load -- the same rotation policy the NMX-C channel
/// cache applies to its TLS material. `None` (token authentication) has no
/// on-disk material and never goes stale this way. A file whose modification
/// time cannot be read keeps the cached client in use: rebuilding on that
/// alone would tear down a working client mid-rotation.
async fn cert_material_newer_than(cert: Option<&ufmclient::UFMCert>, created: SystemTime) -> bool {
    let Some(cert) = cert else {
        return false;
    };
    for path in [&cert.ca_crt, &cert.tls_key, &cert.tls_crt] {
        let modified = tokio::fs::metadata(path).await.and_then(|m| m.modified());
        let mtime = match modified {
            Ok(mtime) => mtime,
            Err(e) => {
                tracing::debug!(
                    path = %path,
                    error = %e,
                    "could not read UFM TLS material mtime; keeping the cached client"
                );
                continue;
            }
        };
        if mtime > created {
            tracing::info!(
                path = %path,
                "UFM TLS material changed on disk; rebuilding the fabric client to pick it up"
            );
            return true;
        }
    }
    false
}

impl IBFabricManagerImpl {
    /// Gets the mocked fabric manager that is used within tests
    #[cfg(feature = "test-support")]
    pub fn get_mock_manager(&self) -> Arc<mock::MockIBFabric> {
        self.mock_fabric.clone()
    }

    /// Locks the REST-client cache. The critical sections only look up and
    /// insert map entries, so a poisoned lock leaves the map usable; recover
    /// it instead of propagating the panic into fabric-protocol paths.
    fn lock_rest_clients(&self) -> MutexGuard<'_, HashMap<String, CachedFabricClient>> {
        self.rest_clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Clone)]
pub struct IBFabricManagerConfig {
    /// List of endpoint per fabric
    pub endpoints: HashMap<String, Vec<String>>,
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
        rest_clients: Mutex::new(HashMap::new()),
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
    /// reads the fabric's credentials from the secret manager -- so rotations
    /// are picked up promptly -- but the client is rebuilt only when that read
    /// returns different credentials, or the fabric's endpoint resolves
    /// differently, than the cached client was built from. Credentials that
    /// select certificate authentication name TLS material on disk rather
    /// than containing it, so a rebuild also happens once any of those files
    /// is newer than the cached client (certificate rotation). Between rebuilds
    /// all callers share one client, whose HTTP pool keeps connections warm
    /// across monitor passes. (`Disable` and `Mock` hand out process-wide
    /// shared instances anyway.)
    ///
    /// Concurrent misses for the same fabric may each build a client, last
    /// insert wins -- a benign duplicate build that opens no connection, since
    /// the client connects lazily on first use. The steady-state callers are
    /// serial reconcile/monitor loops -- API handlers can also race in here,
    /// at worst repeating that lazy build -- so per-fabric in-flight
    /// serialization would be machinery without a workload.
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

                let key = &CredentialKey::UfmAuth {
                    fabric: fabric_name.to_string(),
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
                        IbError::internal(format!(
                            "Cannot create UFM client: vault key not found or token is not set: {}",
                            key.to_key_str()
                        ))
                    })?;

                let fingerprint = fabric_client_fingerprint(endpoint, &credentials);
                let (_deprecated_address, token) = match credentials {
                    Credentials::UsernamePassword { username, password } => (username, password),
                };
                // The auth method these credentials select, and so the on-disk
                // material (if any) a client built from them loads.
                let (_, cert) = rest::auth_method(&token);

                let cached = self
                    .lock_rest_clients()
                    .get(fabric_name)
                    .filter(|cached| cached.fingerprint == fingerprint)
                    .map(|cached| (cached.client.clone(), cached.created));
                if let Some((client, created)) = cached
                    && !cert_material_newer_than(cert.as_ref(), created).await
                {
                    return Ok(client);
                }

                // Timestamp before building: the build reads TLS material from
                // disk, so material rewritten while we build still counts as
                // newer than `created` and triggers one more (benign) rebuild.
                let created = SystemTime::now();
                // Built outside the lock; see the method doc for the benign
                // concurrent-build race this allows.
                let client = rest::new_client(endpoint, &token)?;
                self.lock_rest_clients().insert(
                    fabric_name.to_string(),
                    CachedFabricClient {
                        fingerprint,
                        created,
                        client: client.clone(),
                    },
                );
                Ok(client)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use carbide_secrets::SecretsError;
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

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

    /// A `Rest` manager over two fabrics. Building a REST client makes no
    /// connection (it connects lazily on first use), so these tests exercise
    /// the real build + cache path without a UFM server.
    fn rest_manager(reader: Arc<dyn CredentialReader>) -> IBFabricManagerImpl {
        let config = IBFabricManagerConfig {
            manager_type: IBFabricManagerType::Rest,
            endpoints: HashMap::from([
                ("f1".to_string(), vec!["https://127.0.0.1:443".to_string()]),
                ("f2".to_string(), vec!["https://127.0.0.2:443".to_string()]),
            ]),
            ..Default::default()
        };
        create_ib_fabric_manager(reader, config).unwrap()
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

    /// Certificate-auth credentials name a directory of material files. The
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

    fn cert_manager(dir: &tempfile::TempDir) -> IBFabricManagerImpl {
        rest_manager(Arc::new(SwappableCredentialReader::new(
            dir.path().to_str().expect("utf-8 temp path"),
        )))
    }

    #[tokio::test]
    async fn newer_cert_material_rebuilds_the_client() {
        let dir = cert_material_dir();
        let manager = cert_manager(&dir);

        let before = manager.new_client("f1").await.unwrap();
        assert!(
            Arc::ptr_eq(&before, &manager.new_client("f1").await.unwrap()),
            "unchanged material must return the cached client"
        );

        // Rotate the key: give it a modification time strictly after the
        // cached client's creation. An explicit future timestamp avoids
        // depending on the filesystem's mtime resolution.
        let rotated = SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(dir.path().join("tls.key"))
            .expect("open tls.key")
            .set_modified(rotated)
            .expect("set tls.key mtime");

        assert!(
            !Arc::ptr_eq(&before, &manager.new_client("f1").await.unwrap()),
            "newer TLS material on disk must rebuild the client"
        );
    }

    #[tokio::test]
    async fn unreadable_cert_material_keeps_the_cached_client() {
        let dir = cert_material_dir();
        let manager = cert_manager(&dir);

        let before = manager.new_client("f1").await.unwrap();
        std::fs::remove_file(dir.path().join("tls.key")).expect("remove tls.key");

        assert!(
            Arc::ptr_eq(&before, &manager.new_client("f1").await.unwrap()),
            "unreadable material must not tear down a working client mid-rotation"
        );
    }
}
