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
use std::env;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use carbide_instrument::{Event, LabelValue, MetricFamily, emit};
use eyre::{ContextCompat, WrapErr, eyre};
use opentelemetry::StringValue;
use rand::RngExt;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::sleep;
use vaultrs::api::kv2::requests::SetSecretRequestOptions;
use vaultrs::api::pki::requests::GenerateCertificateRequest;
use vaultrs::client::{
    VaultClient, VaultClientSettings, VaultClientSettingsBuilder, VaultClientSettingsBuilderError,
};
use vaultrs::error::ClientError;
use vaultrs::{kv2, pki};

use crate::SecretsError;
use crate::certificates::{Certificate, CertificateProvider};
use crate::credentials::{
    CredentialKey, CredentialManager, CredentialPrefix, CredentialReader, CredentialWriter,
    Credentials,
};

const DEFAULT_VAULT_CA_PATH: &str = "/var/run/secrets/forge-roots/ca.crt";
const VAULT_CACERT_ENV_VAR: &str = "VAULT_CACERT";
const VAULT_NAMESPACE_ENV_VAR: &str = "VAULT_NAMESPACE";
const DEFAULT_SPIFFE_TRUST_DOMAIN: &str = "nico.local";
const DEFAULT_SPIFFE_MACHINE_BASE_PATH: &str = "/forge-system/machine/";
const VAULT_SPIFFE_TRUST_DOMAIN_ENV_VAR: &str = "VAULT_SPIFFE_TRUST_DOMAIN";
const VAULT_SPIFFE_MACHINE_BASE_PATH_ENV_VAR: &str = "VAULT_SPIFFE_MACHINE_BASE_PATH";

/// Where `vault_token_refresh` writes its token-validation probe on every
/// authentication (first login and every renewal), so it exists on every
/// site from startup on. It is not a `Credentials`-shaped value -- reading it
/// as one fails deserialization -- and bulk enumeration must never try:
/// this path, not a real credential path, is excluded unconditionally rather
/// than through the caller-supplied `CredentialPrefix` exclusions.
const TOKEN_REFRESH_PROBE_PATH: &str = "machines/token_refresh/current_token";

#[derive(Clone, Debug)]
enum ForgeVaultAuthenticationType {
    Root(String),
    ServiceAccount(PathBuf),
}

#[derive(Clone, Debug)]
struct ForgeVaultAuthentication {
    expiry: Instant,
}

enum ForgeVaultAuthenticationStatus {
    Authenticated(ForgeVaultAuthentication, Arc<VaultClient>),
    Initialized,
}

#[derive(Debug, Clone)]
struct ForgeVaultClientConfig {
    pub auth_type: ForgeVaultAuthenticationType,
    pub vault_address: String,
    pub kv_mount_location: String,
    pub pki_mount_location: String,
    pub pki_role_name: String,
    namespace: Option<String>,
    spiffe_trust_domain: String,
    spiffe_machine_base_path: String,
    vault_root_ca_path: String,
}

// Resolve Vault CA path from a specified path first, then
// from `VAULT_CACERT` for local dev flows such as `vault server -dev-tls`.
fn resolve_vault_root_ca_path(configured_path: &str) -> Result<String, eyre::Report> {
    if Path::new(configured_path).exists() {
        return Ok(configured_path.to_string());
    }

    match env::var(VAULT_CACERT_ENV_VAR) {
        Ok(env_path) if Path::new(&env_path).exists() => Ok(env_path),
        Ok(env_path) => {
            tracing::error!(
                %env_path,
                "VAULT_CACERT does not exist. Refusing to connect without TLS verification.",
            );
            Err(eyre!("vault root CA not found"))
        }
        Err(_) => {
            tracing::error!(
                configured_path,
                "Vault root CA not found. Refusing to connect without TLS verification.",
            );
            Err(eyre!("vault root CA not found"))
        }
    }
}

impl ForgeVaultClientConfig {
    fn vault_root_ca_path(&self) -> Result<String, eyre::Report> {
        resolve_vault_root_ca_path(&self.vault_root_ca_path)
    }
}

/// Get the kubernetes ServiceAccount name from a ServiceAccount token.
///
/// The token itself is a JWT, and the ServiceAccount name is in the
/// `["kubernetes.io"]["serviceaccount"]["name"]` key path within the JWT's payload.
///
/// Documentation on the payload is here:
/// https://kubernetes.io/docs/tasks/configure-pod-container/configure-service-account/#serviceaccount-token-volume-projection
fn service_account_role_name_from_jwt(jwt: &str) -> Result<String, eyre::Report> {
    let payload = jwt
        .split('.')
        .nth(1)
        .context("service account jwt missing payload")?;
    let decoded_payload = URL_SAFE_NO_PAD
        .decode(payload)
        .wrap_err("failed to decode service account jwt payload")?;
    let json_value = serde_json::from_slice::<serde_json::Value>(&decoded_payload)
        .wrap_err("failed to parse service account jwt payload")?;
    json_value["kubernetes.io"]["serviceaccount"]["name"]
        .as_str()
        .wrap_err("JWT payload does not contain /kubernetes.io/serviceaccount/name")
        .map(str::to_string)
}

/// Builds a machine SPIFFE URI SAN matching site `[auth.trust]` path layout.
///
/// `machine_base_path` is the path segment after the trust domain, e.g. `/forge-system/machine/`.
pub(crate) fn machine_spiffe_uri(
    trust_domain: &str,
    machine_base_path: &str,
    machine_id: &str,
) -> String {
    let base = machine_base_path.trim().trim_matches('/');
    if base.is_empty() {
        format!("spiffe://{trust_domain}/{machine_id}")
    } else {
        format!("spiffe://{trust_domain}/{base}/{machine_id}")
    }
}

/// The Vault request kind, as the bounded `request_type` label carried by the
/// attempted / succeeded / failed counters and the duration histogram. Each
/// variant renders to its exact snake_case metric label, so the variant names
/// are the label contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum VaultRequestType {
    ServiceAccountLogin,
    ValidateToken,
    GetCredentials,
    SetCredentials,
    DeleteCredentials,
    ListSecrets,
    GetSecrets,
    GetCertificate,
}

/// The HTTP status code of a failed Vault request, as the bounded
/// `http_response_status_code` label on the failure counter: the status code
/// rendered as a string, or the empty string when the client error carried no
/// HTTP response. HTTP status codes are a closed set, so this is a bounded
/// label value; the hand-written `LabelValue` impl is the reviewed escape hatch
/// for a bounded-but-not-enum value, and reproduces the previous
/// `code.to_string()`-or-empty rendering byte for byte.
#[derive(Clone, Copy)]
struct VaultFailureStatusCode(Option<u16>);

impl LabelValue for VaultFailureStatusCode {
    fn label_value(&self) -> StringValue {
        StringValue::from(self.0.map(|code| code.to_string()).unwrap_or_default())
    }
}

/// How long the current Vault token has before it must be refreshed, sampled
/// each time the refresher loop checks. Metric-only (`log = off`): the value
/// matters as a series, not as a line per check.
#[derive(Event)]
#[event(
    event_name = "vault_token_refresh_window_observed",
    metric_name = "carbide_api_vault_token_time_until_refresh_seconds",
    metric_name_unchecked,
    component = "nico-api",
    log = off,
    metric = gauge,
    unit = "s",
    describe = "The amount of time, in seconds, until the Vault token is required to be refreshed"
)]
struct VaultTokenRefreshWindowObserved {
    #[observation]
    time_until_refresh: Duration,
}

/// A Vault request was attempted. Metric-only (`log = off`): counted, never
/// logged.
#[derive(Event)]
#[event(
    event_name = "vault_request_attempted",
    metric_name = "carbide_api_vault_requests_attempted_total",
    component = "nico-api",
    log = off,
    metric = counter,
    describe = "Number of attempted Vault requests"
)]
struct VaultRequestAttempted {
    #[label]
    request_type: VaultRequestType,
}

/// A Vault request succeeded. Metric-only (`log = off`): counted, never logged.
#[derive(Event)]
#[event(
    event_name = "vault_request_succeeded",
    metric_name = "carbide_api_vault_requests_succeeded_total",
    component = "nico-api",
    log = off,
    metric = counter,
    describe = "Number of successful Vault requests"
)]
struct VaultRequestSucceeded {
    #[label]
    request_type: VaultRequestType,
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_api_vault_requests_failed_total",
    kind = counter,
    component = "nico-api",
    describe = "Number of failed Vault requests"
)]
struct ApiVaultRequestsFailed {
    request_type: VaultRequestType,
    http_response_status_code: VaultFailureStatusCode,
}

/// Counts a failed request when this layer does not own a diagnostic record.
/// Callers either handle the response as an expected absence or propagate the
/// error to the layer that owns its log.
#[derive(Event)]
#[event(
    event_name = "vault_request_failed",
    metric_family = ApiVaultRequestsFailed,
    log = off
)]
struct VaultRequestFailed {
    #[label]
    request_type: VaultRequestType,
    #[label]
    http_response_status_code: VaultFailureStatusCode,
}

// Credential Events preserve each operation's log schema:
// reads carry `credential_key`, while writes and deletes carry only `error`.
// Separate types let all four paths share the failure counter without
// changing those operator-facing fields.

/// `VaultCredentialsNotFound` treats HTTP `404` as an expected absence: it
/// moves the failure counter, retains the `DEBUG` record, and lets the caller
/// return `None`.
#[derive(Event)]
#[event(
    event_name = "vault_credentials_not_found",
    metric_family = ApiVaultRequestsFailed,
    log = debug,
    message = "Credentials not found"
)]
struct VaultCredentialsNotFound {
    #[label]
    request_type: VaultRequestType,
    #[label]
    http_response_status_code: VaultFailureStatusCode,
    #[context]
    credential_key: String,
}

/// `VaultCredentialsGetFailed` pairs a non-`404` read failure with its
/// existing `ERROR` record before the error returns to the caller.
#[derive(Event)]
#[event(
    event_name = "vault_credentials_get_failed",
    metric_family = ApiVaultRequestsFailed,
    log = error,
    message = "Error getting credentials"
)]
struct VaultCredentialsGetFailed {
    #[label]
    request_type: VaultRequestType,
    #[label]
    http_response_status_code: VaultFailureStatusCode,
    #[context]
    credential_key: String,
    #[context]
    error: String,
}

#[derive(Event)]
#[event(
    event_name = "vault_credentials_set_failed",
    metric_family = ApiVaultRequestsFailed,
    log = error,
    message = "Error setting credentials"
)]
struct VaultCredentialsSetFailed {
    #[label]
    request_type: VaultRequestType,
    #[label]
    http_response_status_code: VaultFailureStatusCode,
    #[context]
    error: String,
}

#[derive(Event)]
#[event(
    event_name = "vault_credentials_delete_failed",
    metric_family = ApiVaultRequestsFailed,
    log = error,
    message = "Error deleting credentials"
)]
struct VaultCredentialsDeleteFailed {
    #[label]
    request_type: VaultRequestType,
    #[label]
    http_response_status_code: VaultFailureStatusCode,
    #[context]
    error: String,
}

/// A token validation request failed and another attempt will follow.
#[derive(Event)]
#[event(
    event_name = "vault_token_validation_retrying",
    metric_family = ApiVaultRequestsFailed,
    log = error,
    message = "Vault token renewal check: error reading kv mount location config, waiting for token to be good"
)]
struct VaultTokenValidationRetrying {
    #[label]
    request_type: VaultRequestType,
    #[label]
    http_response_status_code: VaultFailureStatusCode,
}

/// The final token validation request failed, exhausting the retry budget.
#[derive(Event)]
#[event(
    event_name = "vault_token_validation_failed",
    metric_family = ApiVaultRequestsFailed,
    log = error,
    message = "Vault token renewal check: error reading kv mount location config, giving up after max attempts"
)]
struct VaultTokenValidationFailed {
    #[label]
    request_type: VaultRequestType,
    #[label]
    http_response_status_code: VaultFailureStatusCode,
}

/// A best-effort catalogue walk could not list one Vault path.
#[derive(Event)]
#[event(
    event_name = "vault_secret_path_list_failed",
    metric_family = ApiVaultRequestsFailed,
    log = warn,
    message = "failed to list vault path"
)]
struct VaultSecretPathListFailed {
    #[label]
    request_type: VaultRequestType,
    #[label]
    http_response_status_code: VaultFailureStatusCode,
    #[context]
    prefix: String,
    #[context]
    error: String,
}

/// A secret disappeared between the catalogue list and the corresponding read.
#[derive(Event)]
#[event(
    event_name = "vault_secret_not_found",
    metric_family = ApiVaultRequestsFailed,
    log = debug,
    message = "vault secret not found"
)]
struct VaultSecretNotFound {
    #[label]
    request_type: VaultRequestType,
    #[label]
    http_response_status_code: VaultFailureStatusCode,
    #[context]
    path: String,
}

/// A best-effort catalogue read could not retrieve one Vault secret.
#[derive(Event)]
#[event(
    event_name = "vault_secret_read_failed",
    metric_family = ApiVaultRequestsFailed,
    log = warn,
    message = "failed to read vault secret"
)]
struct VaultSecretReadFailed {
    #[label]
    request_type: VaultRequestType,
    #[label]
    http_response_status_code: VaultFailureStatusCode,
    #[context]
    path: String,
    #[context]
    error: String,
}

/// The wall-clock duration of an outbound Vault request, in whole
/// milliseconds. Metric-only (`log = off`).
#[derive(Event)]
#[event(
    event_name = "vault_request_duration",
    metric_name = "carbide_api_vault_request_duration_milliseconds",
    component = "nico-api",
    log = off,
    metric = histogram,
    describe = "Duration of outbound Vault requests, in milliseconds"
)]
struct VaultRequestDuration {
    #[label]
    request_type: VaultRequestType,
    #[observation]
    duration_ms: u64,
}

/// The one Vault metric that stays a hand-rolled OpenTelemetry instrument: a
struct RefresherMessage {
    response_tx: tokio::sync::oneshot::Sender<Result<Arc<VaultClient>, eyre::Report>>,
}

pub struct ForgeVaultClient {
    vault_client_config: ForgeVaultClientConfig,
    vault_refresher_tx: Sender<RefresherMessage>,
}

fn create_vault_client_settings<S>(
    token: S,
    vault_client_config: &ForgeVaultClientConfig,
) -> Result<VaultClientSettings, eyre::ErrReport>
where
    S: Into<String>,
{
    let mut vault_client_settings_builder = VaultClientSettingsBuilder::default();
    let vault_client_settings_builder = vault_client_settings_builder
        .token(token)
        .address(vault_client_config.vault_address.clone())
        .timeout(Some(Duration::from_secs(60)));

    let ca_path = vault_client_config.vault_root_ca_path()?;

    let vault_client_settings_builder = vault_client_settings_builder
        .ca_certs(vec![ca_path])
        .verify(true);

    let vault_client_settings_builder = if let Some(namespace) = &vault_client_config.namespace {
        vault_client_settings_builder.set_namespace(namespace.clone())
    } else {
        vault_client_settings_builder
    };

    Ok(vault_client_settings_builder.build()?)
}

async fn validate_vault_token_attempt(
    vault_client: &VaultClient,
    kv_mount_location: &str,
    data: &HashMap<&str, String>,
    will_retry: bool,
) -> bool {
    let request_type = VaultRequestType::ValidateToken;
    emit(VaultRequestAttempted { request_type });

    let started = Instant::now();
    let response = kv2::set(
        vault_client,
        kv_mount_location,
        TOKEN_REFRESH_PROBE_PATH,
        data,
    )
    .await;
    emit(VaultRequestDuration {
        request_type,
        duration_ms: started.elapsed().as_millis() as u64,
    });

    match response {
        Ok(_) => {
            emit(VaultRequestSucceeded { request_type });
            true
        }
        Err(error) => {
            record_vault_token_validation_error(&error, will_retry);
            false
        }
    }
}

async fn vault_token_refresh(
    vault_client_config: &ForgeVaultClientConfig,
) -> Result<(ForgeVaultAuthentication, Arc<VaultClient>), eyre::ErrReport> {
    let (vault_token, vault_token_expiry_secs) = match vault_client_config.auth_type {
        ForgeVaultAuthenticationType::Root(ref root_token) => {
            (
                root_token.clone(),
                60 * 60 * 24 * 365 * 10, /*root token never expires just use ten years*/
            )
        }
        ForgeVaultAuthenticationType::ServiceAccount(ref service_account_token_path) => {
            let jwt = std::fs::read_to_string(service_account_token_path)
                .wrap_err("service_account_token_file_read")?
                .trim()
                .to_string();

            // Multiple services use this crate (carbide-secrets), so figure out what service account
            // to use to auth to vault. The token JWT contains the service account name in the decoded
            // JSON, so we can just read that.
            let role_name =
                service_account_role_name_from_jwt(&jwt).wrap_err("service_account_role_name")?;

            let vault_client_settings = create_vault_client_settings(
                "silly vaultrs bugs make me sad",
                vault_client_config,
            )?;
            let vault_client = VaultClient::new(vault_client_settings)?;
            emit(VaultRequestAttempted {
                request_type: VaultRequestType::ServiceAccountLogin,
            });
            let time_started_vault_request = Instant::now();
            let vault_response = vaultrs::auth::kubernetes::login(
                &vault_client,
                "kubernetes",
                role_name.as_str(),
                jwt.as_str(),
            )
            .await;
            let elapsed_request_duration = time_started_vault_request.elapsed().as_millis() as u64;
            emit(VaultRequestDuration {
                request_type: VaultRequestType::ServiceAccountLogin,
                duration_ms: elapsed_request_duration,
            });
            let auth_info = vault_response
                .inspect_err(|err| {
                    record_vault_service_account_error(err);
                })
                .wrap_err("failed to execute kubernetes service account login request")?;

            emit(VaultRequestSucceeded {
                request_type: VaultRequestType::ServiceAccountLogin,
            });
            // start refreshing before it expires
            let lease_expiry_secs = (0.9 * auth_info.lease_duration as f64) as u64;
            (auth_info.client_token, lease_expiry_secs)
        }
    };

    tracing::info!(
        vault_token_expiry_seconds = vault_token_expiry_secs,
        "successfully refreshed vault token"
    );

    let vault_client_settings = create_vault_client_settings(vault_token, vault_client_config)?;
    let vault_client = VaultClient::new(vault_client_settings)?;

    // validate that we can actually _use_ the token before we give it back
    let mut attempts = 3;

    let now = SystemTime::now();
    let timestamp_secs = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

    let kv_mount_location = vault_client_config.kv_mount_location.as_str();
    let data = HashMap::from([("timestamp_seconds", timestamp_secs.to_string())]);
    while !validate_vault_token_attempt(&vault_client, kv_mount_location, &data, attempts > 1).await
    {
        attempts -= 1;
        if attempts <= 0 {
            break;
        }
        sleep(Duration::from_secs(2)).await;
    }

    Ok((
        ForgeVaultAuthentication {
            expiry: Instant::now() + Duration::from_secs(vault_token_expiry_secs),
        },
        Arc::new(vault_client),
    ))
}

async fn maybe_refresh_vault_client(
    vault_client_config: &ForgeVaultClientConfig,
    vault_auth_status: ForgeVaultAuthenticationStatus,
) -> Result<(ForgeVaultAuthentication, Arc<VaultClient>), eyre::ErrReport> {
    let refresh_fut = vault_token_refresh(vault_client_config);
    match vault_auth_status {
        ForgeVaultAuthenticationStatus::Initialized => refresh_fut.await,
        ForgeVaultAuthenticationStatus::Authenticated(authentication, client) => {
            let time_remaining_until_refresh = authentication
                .expiry
                .saturating_duration_since(Instant::now());

            emit(VaultTokenRefreshWindowObserved {
                time_until_refresh: time_remaining_until_refresh,
            });

            if Instant::now() >= authentication.expiry {
                refresh_fut.await
            } else {
                Ok((authentication, client))
            }
        }
    }
}

async fn vault_refresher_loop(
    mut vault_refresher_rx: Receiver<RefresherMessage>,
    vault_client_config: ForgeVaultClientConfig,
) {
    let mut auth_status = ForgeVaultAuthenticationStatus::Initialized;
    while let Some(message) = vault_refresher_rx.recv().await {
        match maybe_refresh_vault_client(&vault_client_config, auth_status).await {
            Ok((auth, client)) => {
                message.response_tx.send(Ok(client.clone())).ok();
                auth_status = ForgeVaultAuthenticationStatus::Authenticated(auth, client);
            }
            Err(error) => {
                message.response_tx.send(Err(error)).ok();
                auth_status = ForgeVaultAuthenticationStatus::Initialized; // force a refresh until it works
            }
        }
    }
}

impl From<ClientError> for SecretsError {
    fn from(value: ClientError) -> Self {
        SecretsError::GenericError(value.into())
    }
}

impl From<VaultClientSettingsBuilderError> for SecretsError {
    fn from(value: VaultClientSettingsBuilderError) -> Self {
        SecretsError::GenericError(value.into())
    }
}

impl ForgeVaultClient {
    fn new(vault_client_config: ForgeVaultClientConfig) -> Self {
        let (vault_refresher_tx, vault_refresher_rx) = tokio::sync::mpsc::channel(1);
        let vault_client_config_clone = vault_client_config.clone();
        tokio::spawn(async move {
            vault_refresher_loop(vault_refresher_rx, vault_client_config_clone).await;
        });
        Self {
            vault_client_config,
            vault_refresher_tx,
        }
    }

    async fn vault_client(&self) -> Result<Arc<VaultClient>, eyre::Report> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let message = RefresherMessage { response_tx: tx };

        self.vault_refresher_tx
            .send(message)
            .await
            .map_err(|err| eyre!(err))
            .wrap_err("sender error from background vault refresher loop")?;

        rx.await
            .map_err(|err| eyre!(err))
            .wrap_err("receiver error from background vault refresher loop")?
    }
}

#[async_trait]
trait VaultTask<T> {
    async fn execute(&self, vault_client: Arc<VaultClient>) -> Result<T, SecretsError>;
}

struct GetCredentialsHelper<'key, 'location> {
    pub kv_mount_location: &'location String,
    pub key: &'key CredentialKey,
}

#[async_trait]
impl VaultTask<Option<Credentials>> for GetCredentialsHelper<'_, '_> {
    async fn execute(
        &self,
        vault_client: Arc<VaultClient>,
    ) -> Result<Option<Credentials>, SecretsError> {
        emit(VaultRequestAttempted {
            request_type: VaultRequestType::GetCredentials,
        });

        let time_started_vault_request = Instant::now();
        let vault_response = kv2::read(
            vault_client.deref(),
            self.kv_mount_location,
            self.key.to_key_str().as_ref(),
        )
        .await;
        let elapsed_request_duration = time_started_vault_request.elapsed().as_millis() as u64;
        emit(VaultRequestDuration {
            request_type: VaultRequestType::GetCredentials,
            duration_ms: elapsed_request_duration,
        });

        match vault_response {
            Ok(creds) => {
                emit(VaultRequestSucceeded {
                    request_type: VaultRequestType::GetCredentials,
                });
                Ok(Some(creds))
            }
            Err(ce) => {
                let status_code = record_vault_credentials_get_error(&ce, self.key);
                match status_code {
                    Some(404) => {
                        // Not found errors are common and of no concern
                        Ok(None)
                    }
                    _ => Err(SecretsError::GenericError(ce.into())),
                }
            }
        }
    }
}

/// Tracks client errors if an invocation to a Vault server failed
///
/// Returns the status code of the HTTP request if available
fn record_vault_metric_only_error(
    err: &ClientError,
    request_type: VaultRequestType,
) -> Option<u16> {
    let status_code = vault_client_error_status(err);
    emit(VaultRequestFailed {
        request_type,
        http_response_status_code: VaultFailureStatusCode(status_code),
    });
    status_code
}

fn record_vault_token_validation_error(err: &ClientError, will_retry: bool) -> Option<u16> {
    let status_code = vault_client_error_status(err);
    if will_retry {
        emit(VaultTokenValidationRetrying {
            request_type: VaultRequestType::ValidateToken,
            http_response_status_code: VaultFailureStatusCode(status_code),
        });
    } else {
        emit(VaultTokenValidationFailed {
            request_type: VaultRequestType::ValidateToken,
            http_response_status_code: VaultFailureStatusCode(status_code),
        });
    }
    status_code
}

fn record_vault_secret_path_list_error(err: &ClientError, prefix: &str) -> Option<u16> {
    let status_code = vault_client_error_status(err);
    emit(VaultSecretPathListFailed {
        request_type: VaultRequestType::ListSecrets,
        http_response_status_code: VaultFailureStatusCode(status_code),
        prefix: prefix.to_string(),
        error: err.to_string(),
    });
    status_code
}

fn record_vault_secret_not_found(err: &ClientError, path: &str) -> Option<u16> {
    let status_code = vault_client_error_status(err);
    emit(VaultSecretNotFound {
        request_type: VaultRequestType::GetSecrets,
        http_response_status_code: VaultFailureStatusCode(status_code),
        path: path.to_string(),
    });
    status_code
}

fn record_vault_secret_read_error(err: &ClientError, path: &str) -> Option<u16> {
    let status_code = vault_client_error_status(err);
    emit(VaultSecretReadFailed {
        request_type: VaultRequestType::GetSecrets,
        http_response_status_code: VaultFailureStatusCode(status_code),
        path: path.to_string(),
        error: err.to_string(),
    });
    status_code
}

fn record_vault_service_account_error(err: &ClientError) -> Option<u16> {
    record_vault_metric_only_error(err, VaultRequestType::ServiceAccountLogin)
}

fn record_vault_certificate_error(err: &ClientError) -> Option<u16> {
    record_vault_metric_only_error(err, VaultRequestType::GetCertificate)
}

fn vault_client_error_status(err: &ClientError) -> Option<u16> {
    match err {
        ClientError::APIError { code, errors: _ } => Some(*code),
        _ => None,
    }
}

fn record_vault_credentials_get_error(
    err: &ClientError,
    credential_key: &CredentialKey,
) -> Option<u16> {
    let status_code = vault_client_error_status(err);
    let credential_key = credential_key.to_key_str().into_owned();
    if status_code == Some(404) {
        emit(VaultCredentialsNotFound {
            request_type: VaultRequestType::GetCredentials,
            http_response_status_code: VaultFailureStatusCode(status_code),
            credential_key,
        });
    } else {
        emit(VaultCredentialsGetFailed {
            request_type: VaultRequestType::GetCredentials,
            http_response_status_code: VaultFailureStatusCode(status_code),
            credential_key,
            error: format!("{err:?}"),
        });
    }
    status_code
}

fn record_vault_credentials_set_error(err: &ClientError) -> Option<u16> {
    let status_code = vault_client_error_status(err);
    emit(VaultCredentialsSetFailed {
        request_type: VaultRequestType::SetCredentials,
        http_response_status_code: VaultFailureStatusCode(status_code),
        error: format!("{err:?}"),
    });
    status_code
}

fn record_vault_credentials_delete_error(err: &ClientError) -> Option<u16> {
    let status_code = vault_client_error_status(err);
    emit(VaultCredentialsDeleteFailed {
        request_type: VaultRequestType::DeleteCredentials,
        http_response_status_code: VaultFailureStatusCode(status_code),
        error: format!("{err:?}"),
    });
    status_code
}

struct SetCredentialsHelper<'key, 'location> {
    pub kv_mount_location: &'location String,
    pub key: &'key CredentialKey,
    pub credentials: &'key Credentials,
    pub allow_overwrite: bool,
}

#[async_trait]
impl VaultTask<()> for SetCredentialsHelper<'_, '_> {
    async fn execute(&self, vault_client: Arc<VaultClient>) -> Result<(), SecretsError> {
        emit(VaultRequestAttempted {
            request_type: VaultRequestType::SetCredentials,
        });

        let time_started_vault_request = Instant::now();

        let vault_response = if self.allow_overwrite {
            kv2::set(
                vault_client.deref(),
                self.kv_mount_location,
                self.key.to_key_str().as_ref(),
                &self.credentials,
            )
            .await
        } else {
            // Setting the cas key to 0 is the officially documented way of create-only writes. Per
            // vault docs:
            // > If set to 0 a write will only be allowed if the key doesn't exist as unset keys do
            // > not have any version information.
            let options = SetSecretRequestOptions { cas: 0 };

            kv2::set_with_options(
                vault_client.deref(),
                self.kv_mount_location,
                self.key.to_key_str().as_ref(),
                &self.credentials,
                options,
            )
            .await
        };

        let elapsed_request_duration = time_started_vault_request.elapsed().as_millis() as u64;
        emit(VaultRequestDuration {
            request_type: VaultRequestType::SetCredentials,
            duration_ms: elapsed_request_duration,
        });

        let _secret_version_metadata = vault_response.inspect_err(|err| {
            record_vault_credentials_set_error(err);
        })?;

        emit(VaultRequestSucceeded {
            request_type: VaultRequestType::SetCredentials,
        });
        Ok(())
    }
}

struct DeleteCredentialsHelper<'key, 'location> {
    pub kv_mount_location: &'location String,
    pub key: &'key CredentialKey,
}

#[async_trait]
impl VaultTask<()> for DeleteCredentialsHelper<'_, '_> {
    async fn execute(&self, vault_client: Arc<VaultClient>) -> Result<(), SecretsError> {
        emit(VaultRequestAttempted {
            request_type: VaultRequestType::DeleteCredentials,
        });

        let time_started_vault_request = Instant::now();
        let vault_response = kv2::delete_metadata(
            vault_client.deref(),
            self.kv_mount_location,
            self.key.to_key_str().as_ref(),
        )
        .await;

        let elapsed_request_duration = time_started_vault_request.elapsed().as_millis() as u64;
        emit(VaultRequestDuration {
            request_type: VaultRequestType::DeleteCredentials,
            duration_ms: elapsed_request_duration,
        });

        let _secret_version_metadata = vault_response.inspect_err(|err| {
            record_vault_credentials_delete_error(err);
        })?;

        emit(VaultRequestSucceeded {
            request_type: VaultRequestType::DeleteCredentials,
        });
        Ok(())
    }
}

#[async_trait]
impl CredentialReader for ForgeVaultClient {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        let kv_mount_location = &self.vault_client_config.kv_mount_location;
        let get_credentials_helper = GetCredentialsHelper {
            kv_mount_location,
            key,
        };
        let vault_client = self.vault_client().await?;
        get_credentials_helper.execute(vault_client).await
    }
}

#[async_trait]
impl CredentialWriter for ForgeVaultClient {
    async fn get_credentials_from_writer(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        CredentialReader::get_credentials(self, key).await
    }

    async fn set_credentials(
        &self,
        key: &CredentialKey,
        credentials: &Credentials,
    ) -> Result<(), SecretsError> {
        let kv_mount_location = &self.vault_client_config.kv_mount_location;
        let set_credentials_helper = SetCredentialsHelper {
            key,
            credentials,
            kv_mount_location,
            allow_overwrite: true,
        };
        let vault_client = self.vault_client().await?;
        set_credentials_helper.execute(vault_client).await
    }

    async fn create_credentials(
        &self,
        key: &CredentialKey,
        credentials: &Credentials,
    ) -> Result<(), SecretsError> {
        let kv_mount_location = &self.vault_client_config.kv_mount_location;
        let set_credentials_helper = SetCredentialsHelper {
            key,
            credentials,
            kv_mount_location,
            allow_overwrite: false,
        };
        let vault_client = self.vault_client().await?;
        set_credentials_helper.execute(vault_client).await
    }

    async fn delete_credentials(&self, key: &CredentialKey) -> Result<(), SecretsError> {
        let kv_mount_location = &self.vault_client_config.kv_mount_location;
        let delete_credentials_helper = DeleteCredentialsHelper {
            key,
            kv_mount_location,
        };
        let vault_client = self.vault_client().await?;
        delete_credentials_helper.execute(vault_client).await
    }
}

impl CredentialManager for ForgeVaultClient {}

struct GetCertificateHelper {
    /// Used to form URI-type SANs for this certificate
    unique_identifier: String,
    pki_mount_location: String,
    pki_role_name: String,
    spiffe_trust_domain: String,
    spiffe_machine_base_path: String,
    /// Alternative requested DNS-type SANs for this certificate
    alt_names: Option<String>,
    /// Requested expiration date of this certificate
    /// Duration format: https://developer.hashicorp.com/vault/docs/concepts/duration-format
    /// Accept numeric value with suffix such as  s-seconds, m-minutes, h-hours, d-days
    ttl: Option<String>,
}

#[async_trait]
impl VaultTask<Certificate> for GetCertificateHelper {
    async fn execute(&self, vault_client: Arc<VaultClient>) -> Result<Certificate, SecretsError> {
        emit(VaultRequestAttempted {
            request_type: VaultRequestType::GetCertificate,
        });

        let spiffe_id = machine_spiffe_uri(
            &self.spiffe_trust_domain,
            &self.spiffe_machine_base_path,
            &self.unique_identifier,
        );

        let ttl = if let Some(ttl) = self.ttl.clone() {
            ttl
        } else {
            // this is to setup a baseline skew of between 60 - 100% of 30 days,
            // so that not all boxes will renew (or expire) at the same time.
            let max_hours = 720; // 24 * 30
            let min_hours = 432; // 24 * 30 * 0.6
            let mut rng = rand::rng();
            format!("{}h", rng.random_range(min_hours..max_hours))
        };

        let mut certificate_request_builder = GenerateCertificateRequest::builder();
        certificate_request_builder
            .mount(self.pki_mount_location.clone())
            .role(self.pki_role_name.clone())
            .uri_sans(spiffe_id)
            .alt_names(self.alt_names.clone().unwrap_or_default())
            .ttl(ttl);

        let time_started_vault_request = Instant::now();
        let vault_response = pki::cert::generate(
            vault_client.deref(),
            self.pki_mount_location.as_str(),
            self.pki_role_name.as_str(),
            Some(&mut certificate_request_builder),
        )
        .await;
        let elapsed_request_duration = time_started_vault_request.elapsed().as_millis() as u64;
        emit(VaultRequestDuration {
            request_type: VaultRequestType::GetCertificate,
            duration_ms: elapsed_request_duration,
        });

        let generate_certificate_response = vault_response.inspect_err(|err| {
            record_vault_certificate_error(err);
        })?;

        emit(VaultRequestSucceeded {
            request_type: VaultRequestType::GetCertificate,
        });

        Ok(Certificate {
            issuing_ca: generate_certificate_response.issuing_ca.into_bytes(),
            public_key: generate_certificate_response.certificate.into_bytes(),
            private_key: generate_certificate_response.private_key.into_bytes(),
        })
    }
}

#[async_trait]
impl CertificateProvider for ForgeVaultClient {
    async fn get_certificate(
        &self,
        unique_identifier: &str,
        alt_names: Option<String>,
        ttl: Option<String>,
    ) -> Result<Certificate, SecretsError> {
        let get_certificate_helper = GetCertificateHelper {
            unique_identifier: unique_identifier.to_string(),
            pki_mount_location: self.vault_client_config.pki_mount_location.clone(),
            pki_role_name: self.vault_client_config.pki_role_name.clone(),
            spiffe_trust_domain: self.vault_client_config.spiffe_trust_domain.clone(),
            spiffe_machine_base_path: self.vault_client_config.spiffe_machine_base_path.clone(),
            alt_names,
            ttl,
        };
        let vault_client = self.vault_client().await?;
        get_certificate_helper.execute(vault_client).await
    }
}

/// `EnumerationMode` decides whether bulk enumeration keeps going after Vault
/// errors other than HTTP `404`. Callers treat `404` as an expected absence,
/// but request metrics still record it as an unsuccessful HTTP request.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EnumerationMode {
    /// Warn and keep going. Fine for diagnostics, where a partial answer
    /// beats none.
    BestEffort,
    /// Fail the whole enumeration. Required when the caller will act on
    /// the result as if it were complete -- the one-time import writes a
    /// permanent completion marker, so a silently dropped subtree would
    /// become silently lost credentials.
    Strict,
}

/// Whether an enumerated path is skipped, and if so, whether the skip
/// should count toward "something eligible was intentionally skipped"
/// (the caller's `excluded_prefix_found`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathExclusion {
    /// Not excluded; read and imported normally.
    Included,
    /// Skipped, and reported to the caller as an exclusion. Used for
    /// caller-requested prefixes such as `CredentialPrefix::UfmAuth`.
    ExcludedAndCounted,
    /// Skipped, but never reported as an exclusion. The token-refresh
    /// probe is not a credential, but it exists on every site that has
    /// ever authenticated to Vault, so counting it would let a genuinely
    /// empty or misconfigured import source pass the empty-vault guard in
    /// `validate_vault_import_selection`.
    ExcludedAndUncounted,
}

fn vault_path_exclusion(path: &str, excluded_prefixes: &[CredentialPrefix]) -> PathExclusion {
    if path == TOKEN_REFRESH_PROBE_PATH {
        return PathExclusion::ExcludedAndUncounted;
    }
    if excluded_prefixes
        .iter()
        .any(|prefix| path.starts_with(prefix.as_str()))
    {
        return PathExclusion::ExcludedAndCounted;
    }
    PathExclusion::Included
}

async fn list_vault_path(
    vault_client: &VaultClient,
    mount: &str,
    prefix: &str,
    mode: EnumerationMode,
) -> Result<Option<Vec<String>>, SecretsError> {
    let request_type = VaultRequestType::ListSecrets;
    emit(VaultRequestAttempted { request_type });

    let started = Instant::now();
    let response = kv2::list(vault_client, mount, prefix).await;
    emit(VaultRequestDuration {
        request_type,
        duration_ms: started.elapsed().as_millis() as u64,
    });

    match response {
        Ok(entries) => {
            emit(VaultRequestSucceeded { request_type });
            Ok(Some(entries))
        }
        Err(error) if vault_client_error_status(&error) == Some(404) => {
            record_vault_metric_only_error(&error, request_type);
            Ok(None)
        }
        Err(error) if mode == EnumerationMode::Strict => {
            record_vault_metric_only_error(&error, request_type);
            Err(SecretsError::GenericError(eyre!(
                "failed to list vault path {prefix:?}: {error}"
            )))
        }
        Err(error) => {
            record_vault_secret_path_list_error(&error, prefix);
            Ok(None)
        }
    }
}

async fn read_vault_secret(
    vault_client: &VaultClient,
    mount: &str,
    path: &str,
    mode: EnumerationMode,
) -> Result<Option<Credentials>, SecretsError> {
    let request_type = VaultRequestType::GetSecrets;
    emit(VaultRequestAttempted { request_type });

    let started = Instant::now();
    let response = kv2::read::<Credentials>(vault_client, mount, path).await;
    emit(VaultRequestDuration {
        request_type,
        duration_ms: started.elapsed().as_millis() as u64,
    });

    match response {
        Ok(credentials) => {
            emit(VaultRequestSucceeded { request_type });
            Ok(Some(credentials))
        }
        Err(error) if vault_client_error_status(&error) == Some(404) => {
            record_vault_secret_not_found(&error, path);
            Ok(None)
        }
        Err(error) if mode == EnumerationMode::Strict => {
            record_vault_metric_only_error(&error, request_type);
            Err(SecretsError::GenericError(eyre!(
                "failed to read vault secret {path:?}: {error}"
            )))
        }
        Err(error) => {
            record_vault_secret_read_error(&error, path);
            Ok(None)
        }
    }
}

impl ForgeVaultClient {
    /// list_secrets returns all secret paths in the
    /// KV mount.
    pub async fn list_secrets(&self) -> Result<Vec<String>, SecretsError> {
        let paths = self
            .list_secrets_for_path("", EnumerationMode::BestEffort)
            .await?;
        tracing::info!(
            secret_path_count = paths.len(),
            "listed all vault secret paths"
        );
        Ok(paths)
    }

    /// list_secrets_for_prefix returns all secret
    /// paths under the given CredentialPrefix.
    pub async fn list_secrets_for_prefix(
        &self,
        prefix: &crate::credentials::CredentialPrefix,
    ) -> Result<Vec<String>, SecretsError> {
        let paths = self
            .list_secrets_for_path(prefix.as_str(), EnumerationMode::BestEffort)
            .await?;
        tracing::info!(
            prefix = prefix.as_str(),
            secret_path_count = paths.len(),
            "listed vault secret paths for prefix"
        );
        Ok(paths)
    }

    /// list_secrets_for_path recursively lists all secret paths under the
    /// given path prefix in the KV mount.
    async fn list_secrets_for_path(
        &self,
        path_prefix: &str,
        mode: EnumerationMode,
    ) -> Result<Vec<String>, SecretsError> {
        let (paths, _) = self
            .list_secrets_for_path_excluding(path_prefix, mode, &[])
            .await?;
        Ok(paths)
    }

    async fn list_secrets_for_path_excluding(
        &self,
        path_prefix: &str,
        mode: EnumerationMode,
        excluded_prefixes: &[CredentialPrefix],
    ) -> Result<(Vec<String>, bool), SecretsError> {
        let vault_client = self.vault_client().await?;
        let mount = &self.vault_client_config.kv_mount_location;

        let mut paths = Vec::new();
        let mut stack = vec![path_prefix.to_string()];
        let mut excluded_prefix_found = false;

        while let Some(dir) = stack.pop() {
            let Some(entries) = list_vault_path(vault_client.deref(), mount, &dir, mode).await?
            else {
                continue;
            };

            for entry in entries {
                let is_directory = entry.ends_with('/');
                let full = if dir.is_empty() {
                    entry
                } else {
                    format!("{dir}{entry}")
                };

                match vault_path_exclusion(&full, excluded_prefixes) {
                    PathExclusion::ExcludedAndCounted => {
                        excluded_prefix_found = true;
                        continue;
                    }
                    PathExclusion::ExcludedAndUncounted => continue,
                    PathExclusion::Included => {}
                }

                if is_directory {
                    stack.push(full);
                } else {
                    paths.push(full);
                }
            }
        }

        Ok((paths, excluded_prefix_found))
    }

    /// get_secrets returns all secrets in the KV mount (paths plus
    /// credentials), skipping unreadable entries with a warning.
    pub async fn get_secrets(&self) -> Result<Vec<(String, Credentials)>, SecretsError> {
        let paths = self
            .list_secrets_for_path("", EnumerationMode::BestEffort)
            .await?;
        self.read_secrets(&paths, EnumerationMode::BestEffort).await
    }

    /// get_secrets_strict returns all secrets in the KV mount, failing on
    /// the first list or read error instead of skipping. The one-time
    /// Postgres import uses this so a vault hiccup aborts the import --
    /// and leaves the completion marker unwritten -- rather than quietly
    /// importing a subset.
    pub async fn get_secrets_strict(&self) -> Result<Vec<(String, Credentials)>, SecretsError> {
        let (secrets, _) = self.get_secrets_strict_excluding_prefixes(&[]).await?;
        Ok(secrets)
    }

    /// Returns all secrets outside `excluded_prefixes`, failing on the first
    /// list or read error. Excluded directories are not traversed and excluded
    /// credentials are not read. The boolean reports whether an excluded
    /// prefix was found during enumeration.
    pub async fn get_secrets_strict_excluding_prefixes(
        &self,
        excluded_prefixes: &[CredentialPrefix],
    ) -> Result<(Vec<(String, Credentials)>, bool), SecretsError> {
        let (paths, excluded_prefix_found) = self
            .list_secrets_for_path_excluding("", EnumerationMode::Strict, excluded_prefixes)
            .await?;
        let secrets = self.read_secrets(&paths, EnumerationMode::Strict).await?;
        Ok((secrets, excluded_prefix_found))
    }

    /// get_secrets_for_prefix returns all secrets
    /// under the given CredentialPrefix.
    pub async fn get_secrets_for_prefix(
        &self,
        prefix: &crate::credentials::CredentialPrefix,
    ) -> Result<Vec<(String, Credentials)>, SecretsError> {
        let paths = self
            .list_secrets_for_path(prefix.as_str(), EnumerationMode::BestEffort)
            .await?;
        self.read_secrets(&paths, EnumerationMode::BestEffort).await
    }

    /// get_secrets_for_path returns all secrets under
    /// the given path prefix.
    pub async fn get_secrets_for_path(
        &self,
        path_prefix: &str,
    ) -> Result<Vec<(String, Credentials)>, SecretsError> {
        let paths = self
            .list_secrets_for_path(path_prefix, EnumerationMode::BestEffort)
            .await?;
        self.read_secrets(&paths, EnumerationMode::BestEffort).await
    }

    /// read_secrets reads credentials from vault for each path. 404s are
    /// always skipped (deleted between list and read); other errors follow
    /// the enumeration mode.
    async fn read_secrets(
        &self,
        paths: &[String],
        mode: EnumerationMode,
    ) -> Result<Vec<(String, Credentials)>, SecretsError> {
        let vault_client = self.vault_client().await?;
        let mount = &self.vault_client_config.kv_mount_location;

        let mut secrets = Vec::with_capacity(paths.len());
        for path in paths {
            if let Some(credentials) =
                read_vault_secret(vault_client.deref(), mount, path, mode).await?
            {
                secrets.push((path.clone(), credentials));
            }
        }

        Ok(secrets)
    }
}

#[derive(Default, Debug, Clone)]
pub struct VaultConfig {
    pub address: Option<String>,
    pub kv_mount_location: Option<String>,
    pub pki_mount_location: Option<String>,
    pub pki_role_name: Option<String>,
    pub token: Option<String>,
    pub vault_cacert: Option<String>,
    /// HashiCorp Vault Enterprise or HCP Vault Dedicated namespace.
    ///
    /// When configured, this takes precedence over `VAULT_NAMESPACE` and is
    /// sent as `X-Vault-Namespace` on every Vault API request.
    pub namespace: Option<String>,
    /// SPIFFE trust domain for machine PKI URI SANs. Defaults to `nico.local`.
    pub spiffe_trust_domain: Option<String>,
    /// Path prefix after the trust domain, e.g. `/forge-system/machine/`.
    pub spiffe_machine_base_path: Option<String>,
}

impl VaultConfig {
    pub fn address(&self) -> eyre::Result<String> {
        self.address
            .clone()
            .or(env::var("VAULT_ADDR").ok())
            .context("VAULT_ADDR")
    }

    pub fn kv_mount_location(&self) -> eyre::Result<String> {
        self.kv_mount_location
            .clone()
            .or(env::var("VAULT_KV_MOUNT_LOCATION").ok())
            .context("VAULT_KV_MOUNT_LOCATION")
    }

    pub fn pki_mount_location(&self) -> eyre::Result<String> {
        self.pki_mount_location
            .clone()
            .or(env::var("VAULT_PKI_MOUNT_LOCATION").ok())
            .context("VAULT_PKI_MOUNT_LOCATION")
    }

    pub fn pki_role_name(&self) -> eyre::Result<String> {
        self.pki_role_name
            .clone()
            .or(env::var("VAULT_PKI_ROLE_NAME").ok())
            .context("VAULT_PKI_ROLE_NAME")
    }

    pub fn token(&self) -> eyre::Result<String> {
        self.token
            .clone()
            .or(env::var("VAULT_TOKEN").ok())
            .context("VAULT_TOKEN")
    }

    pub fn vault_cacert(&self) -> eyre::Result<String> {
        self.vault_cacert
            .clone()
            .or(env::var(VAULT_CACERT_ENV_VAR).ok())
            .context("VAULT_CACERT")
    }

    /// Resolves the Vault namespace from configuration, then `VAULT_NAMESPACE`.
    ///
    /// An unset namespace preserves Vault OSS and root-namespace behavior.
    pub fn namespace(&self) -> Option<String> {
        normalize_vault_namespace(self.namespace.clone())
            .or_else(|| normalize_vault_namespace(env::var(VAULT_NAMESPACE_ENV_VAR).ok()))
    }

    pub fn spiffe_trust_domain(&self) -> String {
        self.spiffe_trust_domain
            .clone()
            .or_else(|| env::var(VAULT_SPIFFE_TRUST_DOMAIN_ENV_VAR).ok())
            .unwrap_or_else(|| DEFAULT_SPIFFE_TRUST_DOMAIN.to_string())
    }

    pub fn spiffe_machine_base_path(&self) -> String {
        self.spiffe_machine_base_path
            .clone()
            .or_else(|| env::var(VAULT_SPIFFE_MACHINE_BASE_PATH_ENV_VAR).ok())
            .unwrap_or_else(|| DEFAULT_SPIFFE_MACHINE_BASE_PATH.to_string())
    }
}

/// Trims a Vault namespace and treats an empty value as unset.
fn normalize_vault_namespace(namespace: Option<String>) -> Option<String> {
    namespace.and_then(|namespace| {
        let namespace = namespace.trim();
        (!namespace.is_empty()).then(|| namespace.to_string())
    })
}

pub fn create_vault_client(vault_config: &VaultConfig) -> eyre::Result<Arc<ForgeVaultClient>> {
    let configured_ca_path = vault_config
        .vault_cacert()
        .unwrap_or_else(|_| DEFAULT_VAULT_CA_PATH.to_string());

    let vault_root_ca_path = resolve_vault_root_ca_path(configured_ca_path.as_str())?;

    let service_account_token_path =
        Path::new("/var/run/secrets/kubernetes.io/serviceaccount/token");
    let auth_type = if service_account_token_path.exists() {
        ForgeVaultAuthenticationType::ServiceAccount(service_account_token_path.to_owned())
    } else {
        ForgeVaultAuthenticationType::Root(vault_config.token()?)
    };

    let vault_client_config = ForgeVaultClientConfig {
        auth_type,
        vault_address: vault_config.address()?,
        kv_mount_location: vault_config.kv_mount_location()?,
        pki_mount_location: vault_config.pki_mount_location()?,
        pki_role_name: vault_config.pki_role_name()?,
        namespace: vault_config.namespace(),
        spiffe_trust_domain: vault_config.spiffe_trust_domain(),
        spiffe_machine_base_path: vault_config.spiffe_machine_base_path(),
        vault_root_ca_path,
    };

    let forge_vault_client = ForgeVaultClient::new(vault_client_config);
    Ok(Arc::new(forge_vault_client))
}

/// Site-wide SPIFFE identity namespace used when minting machine certificates.
///
/// Certificates are issued under the same identity namespace regardless of
/// which Vault signs them, so this is resolved once from the site's
/// `[auth.trust]` config and shared across cert backends.
#[derive(Debug, Clone)]
pub struct SpiffeIdentity {
    pub trust_domain: String,
    pub machine_base_path: String,
}

/// Connection settings for a Vault used *only* to vend certificates, kept
/// separate from the credential store's Vault.
///
/// The connection-identifying fields are required (non-optional), so a value
/// of this type cannot be constructed without naming the target Vault, its PKI
/// mount, and its role. Those fields do not fall back to process-global
/// `VAULT_*` environment variables, so a half-configured cert Vault cannot be
/// silently re-pointed to the credential Vault. The namespace is the exception:
/// it inherits `VAULT_NAMESPACE` so the default deployment uses one Vault
/// namespace consistently for credentials and certificates.
#[derive(Clone)]
pub struct DedicatedVaultConfig {
    /// Vault address, e.g. `https://vault.example:8200`. Required.
    pub address: String,
    /// PKI secrets-engine mount path on the target Vault. Required.
    pub pki_mount_location: String,
    /// PKI role used to sign leaf certificates. Required.
    pub pki_role_name: String,
    /// Token for root-token auth. Required only when the pod has no Kubernetes
    /// service-account token (the preferred auth path); ignored when SA auth
    /// is available.
    pub token: Option<String>,
    /// Path to the CA bundle that signs the target Vault's TLS certificate.
    /// Defaults to the standard site root (`/var/run/secrets/forge-roots/ca.crt`,
    /// or `VAULT_CACERT`) — this is TLS trust material, not a Vault selector.
    pub vault_cacert: Option<String>,
    /// Optional Vault Enterprise or HCP Vault Dedicated namespace for this
    /// certificate Vault. Takes precedence over `VAULT_NAMESPACE`.
    pub namespace: Option<String>,
}

impl DedicatedVaultConfig {
    /// Resolves the dedicated Vault namespace from configuration, then the
    /// shared `VAULT_NAMESPACE` environment setting.
    fn namespace(&self) -> Option<String> {
        normalize_vault_namespace(self.namespace.clone())
            .or_else(|| normalize_vault_namespace(env::var(VAULT_NAMESPACE_ENV_VAR).ok()))
    }
}

// Hand-rolled so the root `token` is never printed verbatim in logs or errors;
// only its presence is shown.
impl std::fmt::Debug for DedicatedVaultConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DedicatedVaultConfig")
            .field("address", &self.address)
            .field("pki_mount_location", &self.pki_mount_location)
            .field("pki_role_name", &self.pki_role_name)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("vault_cacert", &self.vault_cacert)
            .field("namespace", &self.namespace)
            .finish()
    }
}

/// Build a Vault client dedicated to certificate vending from fully explicit
/// settings, with NO environment-variable fallback for the connection fields.
/// A missing required setting fails here, at startup, rather than silently
/// inheriting the credential Vault's configuration.
pub fn create_dedicated_vault_client(
    config: &DedicatedVaultConfig,
    spiffe: SpiffeIdentity,
) -> eyre::Result<Arc<ForgeVaultClient>> {
    // Required fields are non-`Option`, but an empty string would still slip
    // through serde and build a client that fails confusingly on first use.
    for (field, value) in [
        ("address", &config.address),
        ("pki_mount_location", &config.pki_mount_location),
        ("pki_role_name", &config.pki_role_name),
    ] {
        if value.trim().is_empty() {
            return Err(eyre!(
                "dedicated certificate vault requires a non-empty `{field}`"
            ));
        }
    }

    let configured_ca_path = config
        .vault_cacert
        .clone()
        .unwrap_or_else(|| DEFAULT_VAULT_CA_PATH.to_string());
    let vault_root_ca_path = resolve_vault_root_ca_path(configured_ca_path.as_str())?;

    let service_account_token_path =
        Path::new("/var/run/secrets/kubernetes.io/serviceaccount/token");
    let auth_type = if service_account_token_path.exists() {
        ForgeVaultAuthenticationType::ServiceAccount(service_account_token_path.to_owned())
    } else {
        let token = config
            .token
            .as_ref()
            .filter(|token| !token.trim().is_empty())
            .cloned()
            .ok_or_else(|| {
                eyre!(
                    "dedicated certificate vault requires a non-empty explicit `token` when no kubernetes service-account token is present"
                )
            })?;
        ForgeVaultAuthenticationType::Root(token)
    };

    let vault_client_config = ForgeVaultClientConfig {
        auth_type,
        vault_address: config.address.clone(),
        // Certificate vending never touches the KV engine.
        kv_mount_location: String::new(),
        pki_mount_location: config.pki_mount_location.clone(),
        pki_role_name: config.pki_role_name.clone(),
        namespace: config.namespace(),
        spiffe_trust_domain: spiffe.trust_domain,
        spiffe_machine_base_path: spiffe.machine_base_path,
        vault_root_ca_path,
    };

    Ok(Arc::new(ForgeVaultClient::new(vault_client_config)))
}

/// Build raw vaultrs client settings for a separate vault consumer (the
/// Transit KMS provider), with the same address, CA trust, and timeout that
/// `ForgeVaultClient` itself connects with. Without the CA wiring, a
/// vaultrs client only trusts public roots and fails TLS against a
/// site-CA-signed vault.
///
/// Authentication is NOT at parity with `ForgeVaultClient`: this requires a
/// static vault token in the config and does not support the Kubernetes
/// service-account login flow. Deployments using SA auth cannot configure a
/// transit KMS provider until that lands.
pub fn create_raw_vault_client_settings(
    vault_config: &VaultConfig,
) -> eyre::Result<VaultClientSettings> {
    let configured_ca_path = vault_config
        .vault_cacert()
        .unwrap_or_else(|_| DEFAULT_VAULT_CA_PATH.to_string());
    let ca_path = resolve_vault_root_ca_path(configured_ca_path.as_str())?;

    let mut builder = VaultClientSettingsBuilder::default();
    builder
        .token(vault_config.token()?)
        .address(vault_config.address()?)
        .timeout(Some(Duration::from_secs(60)))
        .ca_certs(vec![ca_path])
        .verify(true);
    if let Some(namespace) = vault_config.namespace() {
        builder.set_namespace(namespace);
    }
    builder
        .build()
        .map_err(|e| eyre!("vault client settings: {e}"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

    use base64::Engine;
    use serde_json::json;
    use serial_test::serial;
    use vaultrs::client::VaultClient;
    use vaultrs::{kv2, pki};

    use super::{
        DedicatedVaultConfig, ForgeVaultAuthenticationType, ForgeVaultClientConfig, PathExclusion,
        SpiffeIdentity, VaultConfig, VaultTokenRefreshWindowObserved,
        create_dedicated_vault_client, create_vault_client_settings, machine_spiffe_uri,
        service_account_role_name_from_jwt, vault_path_exclusion,
    };
    use crate::credentials::CredentialPrefix;

    #[test]
    fn excluded_vault_prefix_covers_the_directory_and_its_credentials() {
        for (path, expected) in [
            ("ufm/", PathExclusion::ExcludedAndCounted),
            ("ufm/default/auth", PathExclusion::ExcludedAndCounted),
            ("machines/bmc/site/root", PathExclusion::Included),
        ] {
            assert_eq!(
                vault_path_exclusion(path, &[CredentialPrefix::UfmAuth]),
                expected,
                "{path}"
            );
        }
    }

    // Regression test for nvbugs 6786805: the token-refresh probe is not a
    // `Credentials`-shaped value, so strict enumeration must always skip it,
    // even when the caller passes no exclusions at all (the default case for
    // a site with `ufm_source` unset to `"local"`). It must also never be
    // counted as a caller-requested exclusion (unlike `UfmAuth`), or an
    // otherwise-empty Vault would pass the empty-source guard in
    // `validate_vault_import_selection` and permanently record nothing.
    #[test]
    fn token_refresh_probe_is_always_excluded_but_never_counted() {
        for (path, excluded_prefixes, expected) in [
            (
                super::TOKEN_REFRESH_PROBE_PATH,
                &[][..],
                PathExclusion::ExcludedAndUncounted,
            ),
            (
                super::TOKEN_REFRESH_PROBE_PATH,
                &[CredentialPrefix::UfmAuth][..],
                PathExclusion::ExcludedAndUncounted,
            ),
            ("machines/bmc/site/root", &[][..], PathExclusion::Included),
        ] {
            assert_eq!(
                vault_path_exclusion(path, excluded_prefixes),
                expected,
                "{path}"
            );
        }
    }

    /// Restores a process environment variable when a test finishes or panics.
    struct EnvironmentVariableGuard {
        name: &'static str,
        previous: Option<OsString>,
    }

    impl EnvironmentVariableGuard {
        /// Replaces an environment variable and records its prior value.
        fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(name);
            // SAFETY: callers mark tests `#[serial]` to avoid concurrent reads
            // or mutations of this process-wide state.
            unsafe {
                std::env::set_var(name, value);
            }
            Self { name, previous }
        }
    }

    impl Drop for EnvironmentVariableGuard {
        fn drop(&mut self) {
            // SAFETY: this restores the prior state recorded by `set` in a
            // `#[serial]` test, including while unwinding from a panic.
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.name, value),
                    None => std::env::remove_var(self.name),
                }
            }
        }
    }

    /// Builds a minimal HTTP Vault client configuration for header tests.
    fn vault_client_config(address: String, namespace: Option<&str>) -> ForgeVaultClientConfig {
        ForgeVaultClientConfig {
            auth_type: ForgeVaultAuthenticationType::Root("test-token".to_string()),
            vault_address: address,
            kv_mount_location: "secret".to_string(),
            pki_mount_location: "pki".to_string(),
            pki_role_name: "nico".to_string(),
            namespace: namespace.map(str::to_string),
            spiffe_trust_domain: "nico.local".to_string(),
            spiffe_machine_base_path: "/forge-system/machine/".to_string(),
            // The settings builder checks that a CA path is configured, but an
            // HTTP address does not read it.
            vault_root_ca_path: format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR")),
        }
    }

    /// Starts a three-request HTTP server and returns its address, requests, and thread.
    fn vault_header_server() -> (
        String,
        mpsc::Receiver<Vec<String>>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind Vault test server");
        let address = listener
            .local_addr()
            .expect("get Vault test server address");
        let (sender, receiver) = mpsc::channel();

        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().expect("accept Vault request");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("set Vault request read timeout");

                let mut request = Vec::new();
                let mut buf = [0; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let bytes_read = stream.read(&mut buf).expect("read Vault request");
                    if bytes_read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..bytes_read]);
                }
                requests.push(String::from_utf8(request).expect("Vault request is UTF-8"));

                stream
                    .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .expect("respond to Vault request");
            }
            sender.send(requests).expect("send Vault requests");
        });

        (format!("http://{address}"), receiver, server)
    }

    /// Asserts that auth, KV, and PKI calls consistently carry the namespace header.
    async fn assert_vault_namespace_headers(namespace: Option<&str>) {
        let (address, requests, server) = vault_header_server();
        let config = vault_client_config(address, namespace);
        let settings = create_vault_client_settings("test-token", &config)
            .expect("build Vault client settings");
        let client = VaultClient::new(settings).expect("create Vault client");

        let _ = vaultrs::auth::kubernetes::login(&client, "kubernetes", "nico", "jwt").await;
        let data = HashMap::from([("key", "value")]);
        let _ = kv2::set(&client, "secret", "machines/test", &data).await;
        let _ = pki::cert::generate(&client, "pki", "nico", None).await;

        let requests = requests
            .recv_timeout(Duration::from_secs(5))
            .expect("receive Vault requests");
        server.join().expect("Vault test server panicked");
        assert_eq!(requests.len(), 3);
        for request in requests {
            let request = request.to_ascii_lowercase();
            match namespace {
                Some(namespace) => assert!(request.contains(&format!(
                    "x-vault-namespace: {}",
                    namespace.to_ascii_lowercase()
                ))),
                None => assert!(!request.contains("x-vault-namespace:")),
            }
        }
    }

    fn dedicated_config() -> DedicatedVaultConfig {
        DedicatedVaultConfig {
            address: "https://vault-certs.example:8200".to_string(),
            pki_mount_location: "pki".to_string(),
            pki_role_name: "machine".to_string(),
            token: None,
            vault_cacert: None,
            namespace: None,
        }
    }

    fn test_spiffe() -> SpiffeIdentity {
        SpiffeIdentity {
            trust_domain: "nico.local".to_string(),
            machine_base_path: "/forge-system/machine/".to_string(),
        }
    }

    /// The token-refresh gauge is queried by the API performance dashboards by
    /// its exported name, which the exporter derives from the instrument name
    /// and unit. Moving it onto the framework must not move that name.
    #[test]
    fn token_refresh_gauge_keeps_its_exported_name() {
        use carbide_instrument::testing::MetricsCapture;

        assert_eq!(
            <VaultTokenRefreshWindowObserved as carbide_instrument::Event>::METRIC,
            carbide_instrument::MetricKind::Gauge { unit: "s" }
        );

        let metrics = MetricsCapture::start();
        carbide_instrument::emit(VaultTokenRefreshWindowObserved {
            time_until_refresh: Duration::from_secs(42),
        });
        assert_eq!(
            metrics.gauge_value("carbide_api_vault_token_time_until_refresh_seconds", &[]),
            42.0,
            "the dashboards query this exact name:\n{}",
            metrics.render()
        );
    }

    #[test]
    fn dedicated_vault_rejects_empty_required_fields() {
        for mutate in [
            |c: &mut DedicatedVaultConfig| c.address = "  ".to_string(),
            |c: &mut DedicatedVaultConfig| c.pki_mount_location = String::new(),
            |c: &mut DedicatedVaultConfig| c.pki_role_name = String::new(),
        ] {
            let mut config = dedicated_config();
            mutate(&mut config);
            let err = match create_dedicated_vault_client(&config, test_spiffe()) {
                Ok(_) => panic!("empty required field must be rejected"),
                Err(err) => err,
            };
            assert!(
                err.to_string().contains("non-empty"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    #[serial]
    fn dedicated_vault_namespace_inherits_environment_with_config_precedence() {
        let _namespace = EnvironmentVariableGuard::set("VAULT_NAMESPACE", "from-environment");

        assert_eq!(
            dedicated_config().namespace().as_deref(),
            Some("from-environment")
        );

        let configured = DedicatedVaultConfig {
            namespace: Some("from-config".to_string()),
            ..dedicated_config()
        };
        assert_eq!(configured.namespace().as_deref(), Some("from-config"));
    }

    #[test]
    fn machine_spiffe_uri_uses_trust_domain_and_base_path() {
        assert_eq!(
            machine_spiffe_uri("forge.local", "/forge-system/machine/", "abc-123"),
            "spiffe://forge.local/forge-system/machine/abc-123"
        );
        assert_eq!(
            machine_spiffe_uri("nico.local", "/forge-system/machine/", "abc-123"),
            "spiffe://nico.local/forge-system/machine/abc-123"
        );
        assert_eq!(
            machine_spiffe_uri("forge.local", "forge-system/machine", "abc-123"),
            "spiffe://forge.local/forge-system/machine/abc-123"
        );
    }

    #[test]
    fn vault_config_spiffe_trust_domain_defaults_to_nico_local() {
        let config = VaultConfig::default();
        assert_eq!(config.spiffe_trust_domain(), "nico.local");
    }

    #[test]
    #[serial]
    fn vault_namespace_from_config_has_precedence() {
        let _namespace = EnvironmentVariableGuard::set("VAULT_NAMESPACE", "from-environment");
        let config = VaultConfig {
            namespace: Some("admin/platform".to_string()),
            ..Default::default()
        };

        assert_eq!(config.namespace().as_deref(), Some("admin/platform"));
    }

    #[test]
    fn vault_namespace_normalization_trims_and_ignores_blank_values() {
        assert_eq!(
            super::normalize_vault_namespace(Some("  admin/platform  ".to_string())).as_deref(),
            Some("admin/platform")
        );
        assert_eq!(
            super::normalize_vault_namespace(Some(" \t ".to_string())),
            None
        );
    }

    #[test]
    #[serial]
    fn vault_namespace_ignores_blank_environment_value() {
        let _namespace = EnvironmentVariableGuard::set("VAULT_NAMESPACE", " \t ");

        assert_eq!(VaultConfig::default().namespace(), None);
    }

    #[test]
    fn vault_client_settings_apply_nested_namespace() {
        let config = vault_client_config(
            "http://127.0.0.1:8200".to_string(),
            Some("admin/platform/nico"),
        );

        let settings = create_vault_client_settings("test-token", &config)
            .expect("build Vault client settings");

        assert_eq!(settings.namespace.as_deref(), Some("admin/platform/nico"));
    }

    #[tokio::test]
    async fn vault_client_sends_namespace_on_auth_kv_and_pki_requests() {
        assert_vault_namespace_headers(Some("admin/platform/nico")).await;
    }

    #[tokio::test]
    async fn vault_client_omits_namespace_when_not_configured() {
        assert_vault_namespace_headers(None).await;
    }

    fn jwt_from_payload(payload_value: serde_json::Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"none","typ":"JWT"}"#);
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload_value.to_string());
        format!("{header}.{payload}.")
    }

    fn jwt_with_account(account: serde_json::Value) -> String {
        jwt_from_payload(json!({
          "aud": [
            "https://kubernetes.default.svc"
          ],
          "exp": 1731613413,
          "iat": 1700077413,
          "iss": "https://kubernetes.default.svc",
          "jti": "ea28ed49-2e11-4280-9ec5-bc3d1d84661a",
          "kubernetes.io": {
            "namespace": "kube-system",
            "node": {
              "name": "127.0.0.1",
              "uid": "58456cb0-dd00-45ed-b797-5578fdceaced"
            },
            "pod": {
              "name": "coredns-69cbfb9798-jv9gn",
              "uid": "778a530c-b3f4-47c0-9cd5-ab018fb64f33"
            },
            "serviceaccount": {
              "name": account,
              "uid": "a087d5a0-e1dd-43ec-93ac-f13d89cd13af"
            },
            "warnafter": 1700081020
          },
          "nbf": 1700077413,
          // The service account is also in the `sub` field. We don't read it, but let's mock it faithfully.
          "sub": format!("system:serviceaccount:kube-system:{account}"),
        }))
    }

    #[test]
    fn extracts_service_account_name_from_kubernetes_jwt_subject() {
        let jwt = jwt_with_account("carbide-bmc-proxy".into());
        let role_name = service_account_role_name_from_jwt(&jwt).unwrap();
        assert_eq!(role_name, "carbide-bmc-proxy");
    }

    #[test]
    fn rejects_unexpected_jwt_subject_format() {
        let jwt = jwt_with_account(serde_json::Value::Null);
        assert!(service_account_role_name_from_jwt(&jwt).is_err());
    }

    #[test]
    fn rejects_random_json() {
        let jwt = jwt_from_payload(json!({"foo": ["bar"]}));
        assert!(service_account_role_name_from_jwt(&jwt).is_err());
    }

    /// The `request_type` label values are the metric's contract: each variant
    /// renders to its exact snake_case string in the Vault counters and
    /// histogram.
    #[test]
    fn vault_request_type_renders_expected_label_values() {
        use carbide_instrument::LabelValue;
        use carbide_test_support::{Check, check_values};

        use super::VaultRequestType;

        check_values(
            [
                Check {
                    scenario: "service account login",
                    input: VaultRequestType::ServiceAccountLogin,
                    expect: "service_account_login".to_string(),
                },
                Check {
                    scenario: "validate token",
                    input: VaultRequestType::ValidateToken,
                    expect: "validate_token".to_string(),
                },
                Check {
                    scenario: "get credentials",
                    input: VaultRequestType::GetCredentials,
                    expect: "get_credentials".to_string(),
                },
                Check {
                    scenario: "set credentials",
                    input: VaultRequestType::SetCredentials,
                    expect: "set_credentials".to_string(),
                },
                Check {
                    scenario: "delete credentials",
                    input: VaultRequestType::DeleteCredentials,
                    expect: "delete_credentials".to_string(),
                },
                Check {
                    scenario: "list secrets",
                    input: VaultRequestType::ListSecrets,
                    expect: "list_secrets".to_string(),
                },
                Check {
                    scenario: "get secrets",
                    input: VaultRequestType::GetSecrets,
                    expect: "get_secrets".to_string(),
                },
                Check {
                    scenario: "get certificate",
                    input: VaultRequestType::GetCertificate,
                    expect: "get_certificate".to_string(),
                },
            ],
            |request_type| request_type.label_value().to_string(),
        );
    }

    /// The failure counter's `http_response_status_code` label: an HTTP status
    /// rendered as a string, or the empty string when the client error carried
    /// no HTTP response. Pins both the code strings and the empty case.
    #[test]
    fn vault_failure_status_code_renders_codes_and_empty() {
        use carbide_instrument::LabelValue;
        use carbide_test_support::{Check, check_values};

        use super::VaultFailureStatusCode;

        check_values(
            [
                Check {
                    scenario: "no http response renders empty",
                    input: VaultFailureStatusCode(None),
                    expect: String::new(),
                },
                Check {
                    scenario: "not found",
                    input: VaultFailureStatusCode(Some(404)),
                    expect: "404".to_string(),
                },
                Check {
                    scenario: "forbidden",
                    input: VaultFailureStatusCode(Some(403)),
                    expect: "403".to_string(),
                },
                Check {
                    scenario: "server error",
                    input: VaultFailureStatusCode(Some(500)),
                    expect: "500".to_string(),
                },
            ],
            |status| status.label_value().to_string(),
        );
    }

    /// Every Vault client failure moves the existing counter. This table pins
    /// the operation split: credential helpers retain their log records, while
    /// service-account and certificate failures remain metric-only here.
    #[test]
    fn vault_request_failures_count_and_log_by_request_type() {
        use carbide_instrument::testing::{MetricsCapture, capture_logs};
        use carbide_test_support::{Check, check_values};
        use vaultrs::error::ClientError;

        use super::{
            VaultRequestType, record_vault_certificate_error,
            record_vault_credentials_delete_error, record_vault_credentials_get_error,
            record_vault_credentials_set_error, record_vault_service_account_error,
        };
        use crate::credentials::CredentialKey;

        #[derive(Clone, Copy)]
        struct FailureInput {
            request_type: VaultRequestType,
            request_type_label: &'static str,
            status_code: u16,
            error: &'static str,
        }

        #[derive(Debug, PartialEq)]
        struct ObservedLog {
            level: tracing::Level,
            metadata_name: String,
            message: String,
            event_name: Option<String>,
            metric_name: Option<String>,
            request_type: Option<String>,
            http_response_status_code: Option<String>,
            credential_key: Option<String>,
            error: Option<String>,
        }

        #[derive(Debug, PartialEq)]
        struct FailureObservation {
            status_code: Option<u16>,
            metric_delta: f64,
            logs: Vec<ObservedLog>,
        }

        fn client_error_text(status_code: u16, error: &str) -> String {
            format!(
                "{:?}",
                ClientError::APIError {
                    code: status_code,
                    errors: vec![error.to_string()],
                }
            )
        }

        fn expected_log(
            level: tracing::Level,
            metadata_name: &str,
            message: &str,
            request_type: &str,
            status_code: u16,
            credential_key: Option<String>,
            error: Option<String>,
        ) -> Vec<ObservedLog> {
            vec![ObservedLog {
                level,
                metadata_name: metadata_name.to_string(),
                message: message.to_string(),
                event_name: Some(metadata_name.to_string()),
                metric_name: Some("carbide_api_vault_requests_failed_total".to_string()),
                request_type: Some(request_type.to_string()),
                http_response_status_code: Some(status_code.to_string()),
                credential_key,
                error,
            }]
        }

        let credential_key = CredentialKey::UfmAuth {
            fabric: "vault-failure-test".to_string(),
        };
        let credential_key_string = credential_key.to_key_str().into_owned();

        check_values(
            [
                Check {
                    scenario: "credential not found",
                    input: FailureInput {
                        request_type: VaultRequestType::GetCredentials,
                        request_type_label: "get_credentials",
                        status_code: 404,
                        error: "credential not found",
                    },
                    expect: FailureObservation {
                        status_code: Some(404),
                        metric_delta: 1.0,
                        logs: expected_log(
                            tracing::Level::DEBUG,
                            "vault_credentials_not_found",
                            "Credentials not found",
                            "get_credentials",
                            404,
                            Some(credential_key_string.clone()),
                            None,
                        ),
                    },
                },
                Check {
                    scenario: "credential read failed",
                    input: FailureInput {
                        request_type: VaultRequestType::GetCredentials,
                        request_type_label: "get_credentials",
                        status_code: 403,
                        error: "credential read failed",
                    },
                    expect: FailureObservation {
                        status_code: Some(403),
                        metric_delta: 1.0,
                        logs: expected_log(
                            tracing::Level::ERROR,
                            "vault_credentials_get_failed",
                            "Error getting credentials",
                            "get_credentials",
                            403,
                            Some(credential_key_string),
                            Some(client_error_text(403, "credential read failed")),
                        ),
                    },
                },
                Check {
                    scenario: "credential write failed",
                    input: FailureInput {
                        request_type: VaultRequestType::SetCredentials,
                        request_type_label: "set_credentials",
                        status_code: 500,
                        error: "credential write failed",
                    },
                    expect: FailureObservation {
                        status_code: Some(500),
                        metric_delta: 1.0,
                        logs: expected_log(
                            tracing::Level::ERROR,
                            "vault_credentials_set_failed",
                            "Error setting credentials",
                            "set_credentials",
                            500,
                            None,
                            Some(client_error_text(500, "credential write failed")),
                        ),
                    },
                },
                Check {
                    scenario: "credential delete failed",
                    input: FailureInput {
                        request_type: VaultRequestType::DeleteCredentials,
                        request_type_label: "delete_credentials",
                        status_code: 503,
                        error: "credential delete failed",
                    },
                    expect: FailureObservation {
                        status_code: Some(503),
                        metric_delta: 1.0,
                        logs: expected_log(
                            tracing::Level::ERROR,
                            "vault_credentials_delete_failed",
                            "Error deleting credentials",
                            "delete_credentials",
                            503,
                            None,
                            Some(client_error_text(503, "credential delete failed")),
                        ),
                    },
                },
                Check {
                    scenario: "service account login caller owns its log",
                    input: FailureInput {
                        request_type: VaultRequestType::ServiceAccountLogin,
                        request_type_label: "service_account_login",
                        status_code: 401,
                        error: "service account login failed",
                    },
                    expect: FailureObservation {
                        status_code: Some(401),
                        metric_delta: 1.0,
                        logs: Vec::new(),
                    },
                },
                Check {
                    scenario: "certificate caller owns its log",
                    input: FailureInput {
                        request_type: VaultRequestType::GetCertificate,
                        request_type_label: "get_certificate",
                        status_code: 502,
                        error: "certificate request failed",
                    },
                    expect: FailureObservation {
                        status_code: Some(502),
                        metric_delta: 1.0,
                        logs: Vec::new(),
                    },
                },
            ],
            |input| {
                let error = ClientError::APIError {
                    code: input.status_code,
                    errors: vec![input.error.to_string()],
                };
                let metrics = MetricsCapture::start();
                let mut status_code = None;
                let logs = capture_logs(|| {
                    status_code = match input.request_type {
                        VaultRequestType::ServiceAccountLogin => {
                            record_vault_service_account_error(&error)
                        }
                        VaultRequestType::GetCertificate => record_vault_certificate_error(&error),
                        VaultRequestType::GetCredentials => {
                            record_vault_credentials_get_error(&error, &credential_key)
                        }
                        VaultRequestType::SetCredentials => {
                            record_vault_credentials_set_error(&error)
                        }
                        VaultRequestType::DeleteCredentials => {
                            record_vault_credentials_delete_error(&error)
                        }
                        VaultRequestType::ValidateToken
                        | VaultRequestType::ListSecrets
                        | VaultRequestType::GetSecrets => {
                            unreachable!("remaining request failures have their own event table")
                        }
                    };
                });
                let status_label = input.status_code.to_string();

                FailureObservation {
                    status_code,
                    metric_delta: metrics.counter_delta(
                        "carbide_api_vault_requests_failed_total",
                        &[
                            ("request_type", input.request_type_label),
                            ("http_response_status_code", status_label.as_str()),
                        ],
                    ),
                    logs: logs
                        .iter()
                        .map(|log| ObservedLog {
                            level: log.level,
                            metadata_name: log.metadata_name.clone(),
                            message: log.message.clone(),
                            event_name: log.field("event_name").map(str::to_string),
                            metric_name: log.field("metric_name").map(str::to_string),
                            request_type: log.field("request_type").map(str::to_string),
                            http_response_status_code: log
                                .field("http_response_status_code")
                                .map(str::to_string),
                            credential_key: log.field("credential_key").map(str::to_string),
                            error: log.field("error").map(str::to_string),
                        })
                        .collect(),
                }
            },
        );
    }

    /// Catalogue and token-validation failures keep their existing log records
    /// while moving the shared Vault failure counter exactly once.
    #[test]
    fn vault_catalogue_failures_count_and_preserve_logs() {
        use carbide_instrument::testing::{MetricsCapture, capture_logs};
        use carbide_test_support::{Check, check_values};
        use vaultrs::error::ClientError;

        use super::{
            VaultRequestType, record_vault_metric_only_error, record_vault_secret_not_found,
            record_vault_secret_path_list_error, record_vault_secret_read_error,
            record_vault_token_validation_error,
        };

        #[derive(Clone, Copy)]
        enum FailureKind {
            TokenValidation { will_retry: bool },
            SecretPathList,
            SecretNotFound,
            SecretRead,
            MetricOnly(VaultRequestType),
        }

        #[derive(Clone, Copy)]
        struct FailureInput {
            kind: FailureKind,
            request_type_label: &'static str,
            status_code: u16,
            error: &'static str,
        }

        #[derive(Debug, PartialEq)]
        struct ObservedLog {
            level: tracing::Level,
            metadata_name: String,
            message: String,
            event_name: Option<String>,
            metric_name: Option<String>,
            request_type: Option<String>,
            http_response_status_code: Option<String>,
            prefix: Option<String>,
            path: Option<String>,
            error: Option<String>,
        }

        #[derive(Debug, PartialEq)]
        struct FailureObservation {
            status_code: Option<u16>,
            metric_delta: f64,
            logs: Vec<ObservedLog>,
        }

        struct ExpectedContext<'a> {
            prefix: Option<&'a str>,
            path: Option<&'a str>,
            error: Option<String>,
        }

        fn expected_log(
            level: tracing::Level,
            metadata_name: &str,
            message: &str,
            request_type: &str,
            status_code: u16,
            context: ExpectedContext<'_>,
        ) -> Vec<ObservedLog> {
            vec![ObservedLog {
                level,
                metadata_name: metadata_name.to_string(),
                message: message.to_string(),
                event_name: Some(metadata_name.to_string()),
                metric_name: Some("carbide_api_vault_requests_failed_total".to_string()),
                request_type: Some(request_type.to_string()),
                http_response_status_code: Some(status_code.to_string()),
                prefix: context.prefix.map(str::to_string),
                path: context.path.map(str::to_string),
                error: context.error,
            }]
        }

        fn client_error_display(status_code: u16, error: &str) -> String {
            ClientError::APIError {
                code: status_code,
                errors: vec![error.to_string()],
            }
            .to_string()
        }

        check_values(
            [
                Check {
                    scenario: "token validation will retry",
                    input: FailureInput {
                        kind: FailureKind::TokenValidation { will_retry: true },
                        request_type_label: "validate_token",
                        status_code: 503,
                        error: "vault unavailable",
                    },
                    expect: FailureObservation {
                        status_code: Some(503),
                        metric_delta: 1.0,
                        logs: expected_log(
                            tracing::Level::ERROR,
                            "vault_token_validation_retrying",
                            "Vault token renewal check: error reading kv mount location config, waiting for token to be good",
                            "validate_token",
                            503,
                            ExpectedContext {
                                prefix: None,
                                path: None,
                                error: None,
                            },
                        ),
                    },
                },
                Check {
                    scenario: "token validation exhausted retries",
                    input: FailureInput {
                        kind: FailureKind::TokenValidation { will_retry: false },
                        request_type_label: "validate_token",
                        status_code: 503,
                        error: "vault unavailable",
                    },
                    expect: FailureObservation {
                        status_code: Some(503),
                        metric_delta: 1.0,
                        logs: expected_log(
                            tracing::Level::ERROR,
                            "vault_token_validation_failed",
                            "Vault token renewal check: error reading kv mount location config, giving up after max attempts",
                            "validate_token",
                            503,
                            ExpectedContext {
                                prefix: None,
                                path: None,
                                error: None,
                            },
                        ),
                    },
                },
                Check {
                    scenario: "best-effort path list",
                    input: FailureInput {
                        kind: FailureKind::SecretPathList,
                        request_type_label: "list_secrets",
                        status_code: 500,
                        error: "list failed",
                    },
                    expect: FailureObservation {
                        status_code: Some(500),
                        metric_delta: 1.0,
                        logs: expected_log(
                            tracing::Level::WARN,
                            "vault_secret_path_list_failed",
                            "failed to list vault path",
                            "list_secrets",
                            500,
                            ExpectedContext {
                                prefix: Some("machines/"),
                                path: None,
                                error: Some(client_error_display(500, "list failed")),
                            },
                        ),
                    },
                },
                Check {
                    scenario: "strict path list caller owns its log",
                    input: FailureInput {
                        kind: FailureKind::MetricOnly(VaultRequestType::ListSecrets),
                        request_type_label: "list_secrets",
                        status_code: 403,
                        error: "list forbidden",
                    },
                    expect: FailureObservation {
                        status_code: Some(403),
                        metric_delta: 1.0,
                        logs: Vec::new(),
                    },
                },
                Check {
                    scenario: "secret disappeared after list",
                    input: FailureInput {
                        kind: FailureKind::SecretNotFound,
                        request_type_label: "get_secrets",
                        status_code: 404,
                        error: "not found",
                    },
                    expect: FailureObservation {
                        status_code: Some(404),
                        metric_delta: 1.0,
                        logs: expected_log(
                            tracing::Level::DEBUG,
                            "vault_secret_not_found",
                            "vault secret not found",
                            "get_secrets",
                            404,
                            ExpectedContext {
                                prefix: None,
                                path: Some("machines/node"),
                                error: None,
                            },
                        ),
                    },
                },
                Check {
                    scenario: "best-effort secret read",
                    input: FailureInput {
                        kind: FailureKind::SecretRead,
                        request_type_label: "get_secrets",
                        status_code: 500,
                        error: "read failed",
                    },
                    expect: FailureObservation {
                        status_code: Some(500),
                        metric_delta: 1.0,
                        logs: expected_log(
                            tracing::Level::WARN,
                            "vault_secret_read_failed",
                            "failed to read vault secret",
                            "get_secrets",
                            500,
                            ExpectedContext {
                                prefix: None,
                                path: Some("machines/node"),
                                error: Some(client_error_display(500, "read failed")),
                            },
                        ),
                    },
                },
                Check {
                    scenario: "strict secret read caller owns its log",
                    input: FailureInput {
                        kind: FailureKind::MetricOnly(VaultRequestType::GetSecrets),
                        request_type_label: "get_secrets",
                        status_code: 403,
                        error: "read forbidden",
                    },
                    expect: FailureObservation {
                        status_code: Some(403),
                        metric_delta: 1.0,
                        logs: Vec::new(),
                    },
                },
            ],
            |input| {
                let error = ClientError::APIError {
                    code: input.status_code,
                    errors: vec![input.error.to_string()],
                };
                let metrics = MetricsCapture::start();
                let mut status_code = None;
                let logs = capture_logs(|| {
                    status_code = match input.kind {
                        FailureKind::TokenValidation { will_retry } => {
                            record_vault_token_validation_error(&error, will_retry)
                        }
                        FailureKind::SecretPathList => {
                            record_vault_secret_path_list_error(&error, "machines/")
                        }
                        FailureKind::SecretNotFound => {
                            record_vault_secret_not_found(&error, "machines/node")
                        }
                        FailureKind::SecretRead => {
                            record_vault_secret_read_error(&error, "machines/node")
                        }
                        FailureKind::MetricOnly(request_type) => {
                            record_vault_metric_only_error(&error, request_type)
                        }
                    };
                });
                let status_label = input.status_code.to_string();

                FailureObservation {
                    status_code,
                    metric_delta: metrics.counter_delta(
                        "carbide_api_vault_requests_failed_total",
                        &[
                            ("request_type", input.request_type_label),
                            ("http_response_status_code", status_label.as_str()),
                        ],
                    ),
                    logs: logs
                        .iter()
                        .map(|log| ObservedLog {
                            level: log.level,
                            metadata_name: log.metadata_name.clone(),
                            message: log.message.clone(),
                            event_name: log.field("event_name").map(str::to_string),
                            metric_name: log.field("metric_name").map(str::to_string),
                            request_type: log.field("request_type").map(str::to_string),
                            http_response_status_code: log
                                .field("http_response_status_code")
                                .map(str::to_string),
                            prefix: log.field("prefix").map(str::to_string),
                            path: log.field("path").map(str::to_string),
                            error: log.field("error").map(str::to_string),
                        })
                        .collect(),
                }
            },
        );
    }

    /// Builds a `VaultClient` pointed at a plaintext `mockito` server, so the
    /// get-credentials helper's real `kv2::read` round-trips through a response
    /// we control. An `http://` address skips TLS, so no CA wiring is needed.
    fn mock_backed_vault_client(
        server: &mockito::ServerGuard,
    ) -> std::sync::Arc<vaultrs::client::VaultClient> {
        use vaultrs::client::{VaultClient, VaultClientSettingsBuilder};

        let settings = VaultClientSettingsBuilder::default()
            .address(server.url())
            .token("test-token")
            .verify(false)
            .build()
            .expect("vault client settings for mock server");
        std::sync::Arc::new(VaultClient::new(settings).expect("vault client for mock server"))
    }

    /// Each remaining outbound request records one attempt, one duration, and
    /// exactly one success or failure, including failures handled locally.
    #[tokio::test]
    async fn catalogue_requests_record_one_red_lifecycle() {
        use carbide_instrument::testing::MetricsCapture;
        use carbide_test_support::Outcome::Yields;
        use carbide_test_support::{Case, check_cases_async};

        use super::{
            EnumerationMode, list_vault_path, read_vault_secret, validate_vault_token_attempt,
        };

        #[derive(Clone, Copy)]
        enum Request {
            ValidateToken { will_retry: bool },
            ListSecrets { mode: EnumerationMode },
            GetSecrets { mode: EnumerationMode },
        }

        #[derive(Clone, Copy)]
        struct RequestInput {
            request: Request,
            request_type: &'static str,
            method: &'static str,
            status_code: u16,
            body: &'static str,
        }

        #[derive(Debug, PartialEq)]
        enum RequestResult {
            Succeeded,
            HandledFailure,
            ReturnedFailure,
        }

        #[derive(Debug, PartialEq)]
        struct RequestObservation {
            result: RequestResult,
            attempted: f64,
            succeeded: f64,
            failed: f64,
            duration_count: u64,
        }

        const SET_SUCCESS: &str = r#"{
            "request_id":"test",
            "lease_id":"",
            "renewable":false,
            "lease_duration":0,
            "data":{
                "created_time":"2024-01-01T00:00:00Z",
                "deletion_time":"",
                "custom_metadata":null,
                "destroyed":false,
                "version":1
            }
        }"#;
        const LIST_SUCCESS: &str = r#"{
            "request_id":"test",
            "lease_id":"",
            "renewable":false,
            "lease_duration":0,
            "data":{"keys":["node"]}
        }"#;
        const READ_SUCCESS: &str = r#"{
            "request_id":"test",
            "lease_id":"",
            "lease_duration":0,
            "renewable":false,
            "data":{
                "data":{"UsernamePassword":{"username":"u","password":"p"}},
                "metadata":{
                    "created_time":"2024-01-01T00:00:00Z",
                    "deletion_time":"",
                    "custom_metadata":null,
                    "destroyed":false,
                    "version":1
                }
            }
        }"#;
        const FORBIDDEN: &str = r#"{"errors":["permission denied"]}"#;
        const UNAVAILABLE: &str = r#"{"errors":["vault unavailable"]}"#;
        const NOT_FOUND: &str = r#"{"errors":["not found"]}"#;

        check_cases_async(
            [
                Case {
                    scenario: "token validation succeeds",
                    input: RequestInput {
                        request: Request::ValidateToken { will_retry: false },
                        request_type: "validate_token",
                        method: "POST",
                        status_code: 200,
                        body: SET_SUCCESS,
                    },
                    expect: Yields(RequestObservation {
                        result: RequestResult::Succeeded,
                        attempted: 1.0,
                        succeeded: 1.0,
                        failed: 0.0,
                        duration_count: 1,
                    }),
                },
                Case {
                    scenario: "token validation failure stays retryable",
                    input: RequestInput {
                        request: Request::ValidateToken { will_retry: true },
                        request_type: "validate_token",
                        method: "POST",
                        status_code: 503,
                        body: UNAVAILABLE,
                    },
                    expect: Yields(RequestObservation {
                        result: RequestResult::HandledFailure,
                        attempted: 1.0,
                        succeeded: 0.0,
                        failed: 1.0,
                        duration_count: 1,
                    }),
                },
                Case {
                    scenario: "path list succeeds",
                    input: RequestInput {
                        request: Request::ListSecrets {
                            mode: EnumerationMode::BestEffort,
                        },
                        request_type: "list_secrets",
                        method: "LIST",
                        status_code: 200,
                        body: LIST_SUCCESS,
                    },
                    expect: Yields(RequestObservation {
                        result: RequestResult::Succeeded,
                        attempted: 1.0,
                        succeeded: 1.0,
                        failed: 0.0,
                        duration_count: 1,
                    }),
                },
                Case {
                    scenario: "best-effort path list handles failure",
                    input: RequestInput {
                        request: Request::ListSecrets {
                            mode: EnumerationMode::BestEffort,
                        },
                        request_type: "list_secrets",
                        method: "LIST",
                        status_code: 503,
                        body: UNAVAILABLE,
                    },
                    expect: Yields(RequestObservation {
                        result: RequestResult::HandledFailure,
                        attempted: 1.0,
                        succeeded: 0.0,
                        failed: 1.0,
                        duration_count: 1,
                    }),
                },
                Case {
                    scenario: "strict path list returns failure",
                    input: RequestInput {
                        request: Request::ListSecrets {
                            mode: EnumerationMode::Strict,
                        },
                        request_type: "list_secrets",
                        method: "LIST",
                        status_code: 403,
                        body: FORBIDDEN,
                    },
                    expect: Yields(RequestObservation {
                        result: RequestResult::ReturnedFailure,
                        attempted: 1.0,
                        succeeded: 0.0,
                        failed: 1.0,
                        duration_count: 1,
                    }),
                },
                Case {
                    scenario: "strict path list handles absence",
                    input: RequestInput {
                        request: Request::ListSecrets {
                            mode: EnumerationMode::Strict,
                        },
                        request_type: "list_secrets",
                        method: "LIST",
                        status_code: 404,
                        body: NOT_FOUND,
                    },
                    expect: Yields(RequestObservation {
                        result: RequestResult::HandledFailure,
                        attempted: 1.0,
                        succeeded: 0.0,
                        failed: 1.0,
                        duration_count: 1,
                    }),
                },
                Case {
                    scenario: "bulk secret read succeeds",
                    input: RequestInput {
                        request: Request::GetSecrets {
                            mode: EnumerationMode::BestEffort,
                        },
                        request_type: "get_secrets",
                        method: "GET",
                        status_code: 200,
                        body: READ_SUCCESS,
                    },
                    expect: Yields(RequestObservation {
                        result: RequestResult::Succeeded,
                        attempted: 1.0,
                        succeeded: 1.0,
                        failed: 0.0,
                        duration_count: 1,
                    }),
                },
                Case {
                    scenario: "strict bulk secret read handles disappearance",
                    input: RequestInput {
                        request: Request::GetSecrets {
                            mode: EnumerationMode::Strict,
                        },
                        request_type: "get_secrets",
                        method: "GET",
                        status_code: 404,
                        body: NOT_FOUND,
                    },
                    expect: Yields(RequestObservation {
                        result: RequestResult::HandledFailure,
                        attempted: 1.0,
                        succeeded: 0.0,
                        failed: 1.0,
                        duration_count: 1,
                    }),
                },
                Case {
                    scenario: "strict bulk secret read returns failure",
                    input: RequestInput {
                        request: Request::GetSecrets {
                            mode: EnumerationMode::Strict,
                        },
                        request_type: "get_secrets",
                        method: "GET",
                        status_code: 403,
                        body: FORBIDDEN,
                    },
                    expect: Yields(RequestObservation {
                        result: RequestResult::ReturnedFailure,
                        attempted: 1.0,
                        succeeded: 0.0,
                        failed: 1.0,
                        duration_count: 1,
                    }),
                },
            ],
            |input| async move {
                let mut server = mockito::Server::new_async().await;
                let request = server
                    .mock(input.method, mockito::Matcher::Any)
                    .with_status(input.status_code as usize)
                    .with_header("content-type", "application/json")
                    .with_body(input.body)
                    .expect(1)
                    .create_async()
                    .await;
                let client = mock_backed_vault_client(&server);
                let metrics = MetricsCapture::start();

                let result = match input.request {
                    Request::ValidateToken { will_retry } => {
                        let data = std::collections::HashMap::from([(
                            "timestamp_seconds",
                            "1".to_string(),
                        )]);
                        if validate_vault_token_attempt(
                            client.as_ref(),
                            "secret",
                            &data,
                            will_retry,
                        )
                        .await
                        {
                            RequestResult::Succeeded
                        } else {
                            RequestResult::HandledFailure
                        }
                    }
                    Request::ListSecrets { mode } => {
                        match list_vault_path(client.as_ref(), "secret", "machines/", mode).await {
                            Ok(Some(_)) => RequestResult::Succeeded,
                            Ok(None) => RequestResult::HandledFailure,
                            Err(_) => RequestResult::ReturnedFailure,
                        }
                    }
                    Request::GetSecrets { mode } => {
                        match read_vault_secret(client.as_ref(), "secret", "machines/node", mode)
                            .await
                        {
                            Ok(Some(_)) => RequestResult::Succeeded,
                            Ok(None) => RequestResult::HandledFailure,
                            Err(_) => RequestResult::ReturnedFailure,
                        }
                    }
                };
                request.assert_async().await;

                let request_labels = &[("request_type", input.request_type)];
                let status_code = input.status_code.to_string();
                let failure_labels = &[
                    ("request_type", input.request_type),
                    ("http_response_status_code", status_code.as_str()),
                ];

                Ok::<_, ()>(RequestObservation {
                    result,
                    attempted: metrics.counter_delta(
                        "carbide_api_vault_requests_attempted_total",
                        request_labels,
                    ),
                    succeeded: metrics.counter_delta(
                        "carbide_api_vault_requests_succeeded_total",
                        request_labels,
                    ),
                    failed: metrics
                        .counter_delta("carbide_api_vault_requests_failed_total", failure_labels),
                    duration_count: metrics.histogram_count_delta(
                        "carbide_api_vault_request_duration_milliseconds",
                        request_labels,
                    ),
                })
            },
        )
        .await;
    }

    /// A failed `get_credentials` read counts the attempt, times it once, and
    /// moves ONLY the failed counter (carrying the HTTP status code) -- never
    /// the succeeded counter -- while a successful read moves the succeeded
    /// counter and leaves the failed one alone. Regression: the helper used to
    /// emit `VaultRequestSucceeded` unconditionally after the response match, so
    /// a failed read double-counted as both failed and succeeded, corrupting the
    /// success/error split for `request_type="get_credentials"`.
    #[tokio::test]
    async fn get_credentials_failed_read_counts_failed_not_succeeded() {
        use carbide_instrument::testing::MetricsCapture;

        use super::{GetCredentialsHelper, VaultTask};
        use crate::credentials::CredentialKey;

        let mount = "secret".to_string();
        let key = CredentialKey::UfmAuth {
            fabric: "regression".to_string(),
        };
        let get = &[("request_type", "get_credentials")][..];
        let failed_403 = &[
            ("request_type", "get_credentials"),
            ("http_response_status_code", "403"),
        ][..];

        // A non-404 error (here 403) must surface as an error and move the
        // failed counter with its status code -- and must NOT move succeeded.
        {
            let mut server = mockito::Server::new_async().await;
            let _mock = server
                .mock("GET", mockito::Matcher::Any)
                .with_status(403)
                .with_header("content-type", "application/json")
                .with_body(r#"{"errors":["permission denied"]}"#)
                .create_async()
                .await;

            let helper = GetCredentialsHelper {
                kv_mount_location: &mount,
                key: &key,
            };

            let metrics = MetricsCapture::start();
            let result = helper.execute(mock_backed_vault_client(&server)).await;

            assert!(result.is_err(), "a 403 read must surface as an error");
            assert_eq!(
                metrics.counter_delta("carbide_api_vault_requests_failed_total", failed_403),
                1.0,
                "a failed read must move the failed counter once with its status code; exposition:\n{}",
                metrics.render()
            );
            assert_eq!(
                metrics.counter_delta("carbide_api_vault_requests_succeeded_total", get),
                0.0,
                "a failed read must not move the succeeded counter",
            );
            assert_eq!(
                metrics.counter_delta("carbide_api_vault_requests_attempted_total", get),
                1.0,
                "every read counts exactly one attempt",
            );
            assert_eq!(
                metrics
                    .histogram_count_delta("carbide_api_vault_request_duration_milliseconds", get),
                1,
                "every read records exactly one duration observation",
            );
        }

        // A successful read moves the succeeded counter and leaves the failed
        // series untouched.
        {
            let mut server = mockito::Server::new_async().await;
            let _mock = server
                .mock("GET", mockito::Matcher::Any)
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(
                    r#"{"request_id":"test","lease_id":"","lease_duration":0,"renewable":false,"data":{"data":{"UsernamePassword":{"username":"u","password":"p"}},"metadata":{"created_time":"2024-01-01T00:00:00Z","deletion_time":"","custom_metadata":null,"destroyed":false,"version":1}}}"#,
                )
                .create_async()
                .await;

            let helper = GetCredentialsHelper {
                kv_mount_location: &mount,
                key: &key,
            };

            let metrics = MetricsCapture::start();
            let result = helper.execute(mock_backed_vault_client(&server)).await;

            assert!(
                matches!(&result, Ok(Some(_))),
                "a 200 read with a valid body must succeed, got {result:?}"
            );
            assert_eq!(
                metrics.counter_delta("carbide_api_vault_requests_succeeded_total", get),
                1.0,
                "a successful read must move the succeeded counter once; exposition:\n{}",
                metrics.render()
            );
            assert_eq!(
                metrics.counter_delta("carbide_api_vault_requests_failed_total", failed_403),
                0.0,
                "a successful read must not move the failed counter",
            );
            assert_eq!(
                metrics.counter_delta("carbide_api_vault_requests_attempted_total", get),
                1.0,
                "every read counts exactly one attempt",
            );
            assert_eq!(
                metrics
                    .histogram_count_delta("carbide_api_vault_request_duration_milliseconds", get),
                1,
                "every read records exactly one duration observation",
            );
        }
    }
}
