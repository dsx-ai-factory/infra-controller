// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Core artifact requests use the admitted job's runtime identity configuration.
//! The journal stores delivery intent and digests, never this client configuration.

use std::path::{Path, PathBuf};
use std::time::Duration;

use forge_tls::client_config::ClientCert;
use http_body_util::BodyExt;
use rpc::forge_tls_client::{
    ConfigurationError, ForgeClientConfig, ForgeTlsClient, ForgeTlsClientError,
};
use rpc::protos::firmware_supervisor::CoreFirmwareDownloadConfig as WireConfig;
use tokio::time::{Instant, timeout_at};
use tonic::codegen::http::uri::PathAndQuery;
use tonic::codegen::http::{StatusCode, header};

use super::artifact_writer::{ArtifactWriter, MAX_ARTIFACT_BYTES};
use super::config::FirmwareArtifactDigests;
use super::error::{CoreTransferFailure, FirmwareError, FirmwareResult};
use super::source::SOURCE_TRANSFER_TIMEOUT;

const MAX_TRANSFER_RETRIES: u32 = 3;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Resolved certificate-based Core transport settings supplied by Scout.
///
/// The accepted worker owns this snapshot after the submitting RPC or Scout
/// exits. The local request carries certificate paths and transport settings,
/// without PEM bytes or node-auth tokens. `ForgeTlsClient` rereads certificates
/// and roots and mints JWTs locally. Keep this configuration out of the journal
/// and diagnostics because proxy settings can contain secrets.
#[derive(Clone)]
pub struct CoreFirmwareDownloadConfig {
    /// Stable HTTPS Core origin, without user information, query or fragment.
    pub core_url: String,
    /// Absolute UTF-8 root-CA path, at most 4096 bytes; loaded during preparation.
    pub root_ca_path: String,
    /// Absolute UTF-8 client-certificate path, at most 4096 bytes.
    pub client_cert_path: String,
    /// Absolute UTF-8 private-key path, at most 4096 bytes.
    pub client_key_path: String,
    /// Resolved TLS enforcement; SiteCache rejects `false`.
    pub enforce_tls: bool,
    /// Whether missing client certificate material must fail transport setup.
    pub require_client_cert: bool,
    /// Whether DNS and connections use the management VRF.
    pub use_mgmt_vrf: bool,
    /// Positive header deadline that fits an `Instant`; omission uses the shared
    /// client's 300-second default. The cumulative artifact deadline still applies.
    pub request_timeout: Option<Duration>,
    /// Shared connector's SOCKS5 proxy setting; omission uses no proxy.
    /// It can contain credentials and is retained only for this worker.
    pub socks_proxy: Option<String>,
    /// Connector retries per request; omission or zero disables them. Scout's
    /// existing factory supplies three, within the artifact's cumulative deadline.
    pub connect_retries_max: Option<u32>,
    /// Connector retry delay that fits an `Instant`; omission or zero adds no
    /// delay. Scout's existing factory supplies 20 seconds. Artifact-level retries
    /// separately wait 1, 2 and 4 seconds within the same cumulative deadline.
    pub connect_retries_interval: Option<Duration>,
}

impl CoreFirmwareDownloadConfig {
    /// Validates a configuration without opening files or making a request.
    /// Relative paths are rejected rather than resolved against a helper that
    /// may have been started by another caller in a different directory.
    pub fn validate(&self) -> FirmwareResult<()> {
        let origin =
            url::Url::parse(&self.core_url).map_err(|_| invalid_config("invalid core origin"))?;
        if origin.scheme() != "https"
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
            || !self.enforce_tls
        {
            return Err(invalid_config(
                "core delivery requires a verified HTTPS origin",
            ));
        }
        for path in [
            &self.root_ca_path,
            &self.client_cert_path,
            &self.client_key_path,
        ] {
            if !Path::new(path).is_absolute() || path.len() > 4096 {
                return Err(invalid_config(
                    "core identity paths must be absolute and at most 4096 bytes",
                ));
            }
        }
        for duration in [self.request_timeout, self.connect_retries_interval]
            .into_iter()
            .flatten()
        {
            if Instant::now().checked_add(duration).is_none() {
                return Err(invalid_config("core transport duration is out of range"));
            }
        }
        if self
            .request_timeout
            .is_some_and(|duration| duration.is_zero())
        {
            return Err(invalid_config("core request timeout must be positive"));
        }
        Ok(())
    }

    /// Reconstruct Scout's resolved settings without consulting the helper environment.
    fn client_config(&self) -> ForgeClientConfig {
        // Build from resolved fields, without rereading the environment of a
        // daemon that might predate the Scout supplying this operation.
        ForgeClientConfig {
            root_ca_path: self.root_ca_path.clone(),
            client_cert: Some(ClientCert {
                cert_path: self.client_cert_path.clone(),
                key_path: self.client_key_path.clone(),
            }),
            enforce_tls: self.enforce_tls,
            require_client_cert: self.require_client_cert,
            use_mgmt_vrf: self.use_mgmt_vrf,
            request_timeout: self.request_timeout,
            socks_proxy: self.socks_proxy.clone(),
            connect_retries_max: self.connect_retries_max,
            connect_retries_interval: self.connect_retries_interval,
            ..Default::default()
        }
        .with_node_jwt()
    }
}

impl TryFrom<WireConfig> for CoreFirmwareDownloadConfig {
    type Error = FirmwareError;

    fn try_from(value: WireConfig) -> FirmwareResult<Self> {
        let config = Self {
            core_url: value.core_url,
            root_ca_path: value.root_ca_path,
            client_cert_path: value.client_cert_path,
            client_key_path: value.client_key_path,
            enforce_tls: value.enforce_tls,
            require_client_cert: value.require_client_cert,
            use_mgmt_vrf: value.use_mgmt_vrf,
            request_timeout: value
                .request_timeout
                .map(TryInto::try_into)
                .transpose()
                .map_err(|_| invalid_config("invalid core request timeout"))?,
            socks_proxy: value.socks_proxy,
            connect_retries_max: value.connect_retries_max,
            connect_retries_interval: value
                .connect_retries_interval
                .map(TryInto::try_into)
                .transpose()
                .map_err(|_| invalid_config("invalid core connection retry interval"))?,
        };
        config.validate()?;
        Ok(config)
    }
}

impl TryFrom<CoreFirmwareDownloadConfig> for WireConfig {
    type Error = FirmwareError;

    fn try_from(value: CoreFirmwareDownloadConfig) -> FirmwareResult<Self> {
        value.validate()?;
        Ok(Self {
            core_url: value.core_url,
            root_ca_path: value.root_ca_path,
            client_cert_path: value.client_cert_path,
            client_key_path: value.client_key_path,
            enforce_tls: value.enforce_tls,
            require_client_cert: value.require_client_cert,
            use_mgmt_vrf: value.use_mgmt_vrf,
            request_timeout: value
                .request_timeout
                .map(TryInto::try_into)
                .transpose()
                .map_err(|_| invalid_config("core request timeout cannot be represented"))?,
            socks_proxy: value.socks_proxy,
            connect_retries_max: value.connect_retries_max,
            connect_retries_interval: value
                .connect_retries_interval
                .map(TryInto::try_into)
                .transpose()
                .map_err(|_| invalid_config("core retry interval cannot be represented"))?,
        })
    }
}

/// Keep validation diagnostics free of runtime configuration values.
fn invalid_config(message: &str) -> FirmwareError {
    FirmwareError::ConfigError(message.to_string())
}

/// Classify local setup without saving paths, proxy settings or source text.
fn client_setup_error(error: ForgeTlsClientError) -> FirmwareError {
    let failure = match &error {
        ForgeTlsClientError::Configuration(ConfigurationError::CouldNotReadRootCa {
            error,
            ..
        }) => CoreTransferFailure::RootCertificate(error.kind()),
        ForgeTlsClientError::Configuration(
            ConfigurationError::ClientCertNotLoaded | ConfigurationError::InvalidClientCert(_),
        ) => CoreTransferFailure::ClientCertificate,
        ForgeTlsClientError::Configuration(ConfigurationError::Resolver(_)) => {
            CoreTransferFailure::Resolver
        }
        _ => CoreTransferFailure::Configuration,
    };
    FirmwareError::CoreTransfer {
        failure,
        source: Box::new(error),
    }
}

/// Artifact membership in the frozen operation, matching Core route roles.
#[derive(Clone, Copy)]
pub(super) enum CoreArtifactRole {
    /// Image bytes written by the burn phase.
    Image,
    /// Optional configuration bytes applied before burn.
    Configuration,
}

impl CoreArtifactRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Configuration => "configuration",
        }
    }
}

/// Per-artifact network time and staged-byte bounds.
struct DownloadLimits {
    timeout: Duration,
    max_bytes: u64,
    retry_delay: Duration,
}

/// Fetch one saved operation's artifacts using transient Core credentials.
pub(super) struct CoreFirmwareDelivery {
    config: CoreFirmwareDownloadConfig,
    operation_id: String,
    digests: FirmwareArtifactDigests,
    limits: DownloadLimits,
}

impl CoreFirmwareDelivery {
    /// Bind runtime credentials to one admitted identity and approved artifact set.
    pub(super) fn new(
        config: CoreFirmwareDownloadConfig,
        operation_id: String,
        digests: FirmwareArtifactDigests,
    ) -> FirmwareResult<Self> {
        config.validate()?;
        Ok(Self {
            config,
            operation_id,
            digests,
            limits: DownloadLimits {
                timeout: SOURCE_TRANSFER_TIMEOUT,
                max_bytes: MAX_ARTIFACT_BYTES,
                retry_delay: Duration::from_secs(1),
            },
        })
    }

    /// Poll the Core cache and stage only a 200 body, never a pending response.
    /// One deadline covers requests, retry waits and response frames. Local
    /// writes settle before returning a network failure; outer preparation
    /// cancellation retains the directory if filesystem work can still run.
    pub(super) async fn resolve(
        &self,
        original_url: &str,
        directory: &Path,
        role: CoreArtifactRole,
    ) -> FirmwareResult<PathBuf> {
        let digest = match role {
            CoreArtifactRole::Image => &self.digests.image_sha256,
            CoreArtifactRole::Configuration => self
                .digests
                .configuration_sha256
                .as_ref()
                .ok_or_else(|| invalid_config("core configuration download has no digest"))?,
        };
        let mut address = url::Url::parse(&self.config.core_url)
            .map_err(|_| invalid_config("invalid core origin"))?;
        address.set_path(&format!(
            "/firmware/nic/{}/{}",
            hex::encode(digest),
            role.as_str()
        ));
        address
            .query_pairs_mut()
            .append_pair("operation_id", &self.operation_id);
        let query = address
            .query()
            .ok_or_else(|| invalid_config("core artifact query is missing"))?;
        let path: PathAndQuery = format!("{}?{query}", address.path())
            .parse()
            .map_err(|_| invalid_config("invalid core artifact path"))?;
        let original = url::Url::parse(original_url)
            .map_err(|_| invalid_config("invalid original firmware source"))?;
        let filename = original
            .path_segments()
            .and_then(|mut parts| parts.next_back())
            .filter(|name| !name.is_empty())
            .unwrap_or("firmware.bin");
        let destination = directory.join(filename);
        let deadline = Instant::now() + self.limits.timeout;
        let mut failures = 0;
        loop {
            if Instant::now() >= deadline {
                return Err(self.timed_out());
            }
            // Rebuild from paths on each attempt, preserving the existing
            // certificate/root refresh boundary and authenticated HTTP2 stack.
            let config = self.config.client_config();
            let response = timeout_at(deadline, async {
                let mut client = ForgeTlsClient::new(&config)
                    .build_http(&self.config.core_url)
                    .await
                    .map_err(client_setup_error)?;
                client
                    .get(path.clone())
                    .await
                    .map_err(|error| FirmwareError::CoreTransfer {
                        failure: CoreTransferFailure::Request,
                        source: error.into(),
                    })
            })
            .await
            .map_err(|_| self.timed_out())?;
            let mut response = match response {
                Ok(response) => response,
                Err(
                    error @ FirmwareError::CoreTransfer {
                        failure: CoreTransferFailure::Request,
                        ..
                    },
                ) => {
                    self.retry_transfer(error, &mut failures, deadline).await?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            match response.status() {
                StatusCode::ACCEPTED
                | StatusCode::SERVICE_UNAVAILABLE
                | StatusCode::BAD_GATEWAY => {
                    let seconds = response
                        .headers()
                        .get(header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(1);
                    let delay =
                        Duration::from_secs(seconds).clamp(Duration::from_secs(1), MAX_RETRY_DELAY);
                    drop(response);
                    timeout_at(deadline, tokio::time::sleep(delay))
                        .await
                        .map_err(|_| self.timed_out())?;
                    continue;
                }
                StatusCode::OK => {}
                status => {
                    return Err(FirmwareError::HttpError(format!(
                        "HTTP {} from core artifact route",
                        status.as_u16()
                    )));
                }
            }
            let advertised_length = response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            let mut writer =
                ArtifactWriter::create(&destination, self.limits.max_bytes, advertised_length)
                    .await?;
            let transfer = async {
                loop {
                    // A ready frame can win timeout's poll after slow local I/O.
                    // Observe the absolute budget before accepting more bytes.
                    if Instant::now() >= deadline {
                        return Err(self.timed_out());
                    }
                    let Some(frame) = timeout_at(deadline, response.body_mut().frame())
                        .await
                        .map_err(|_| self.timed_out())?
                    else {
                        break;
                    };
                    let frame = frame.map_err(|error| FirmwareError::CoreTransfer {
                        failure: CoreTransferFailure::Body,
                        source: error.into(),
                    })?;
                    if let Ok(data) = frame.into_data() {
                        writer.write_chunk(&data).await?;
                    }
                }
                FirmwareResult::Ok(())
            }
            .await;
            // Tokio may still own a buffered filesystem write when the next
            // network frame fails. Settle it before truncating or removing inputs.
            writer.finish().await?;
            match transfer {
                Ok(()) => return Ok(destination),
                Err(error @ FirmwareError::CoreTransfer { .. }) => {
                    self.retry_transfer(error, &mut failures, deadline).await?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Retry transport failures within the shared artifact deadline.
    async fn retry_transfer(
        &self,
        error: FirmwareError,
        failures: &mut u32,
        deadline: Instant,
    ) -> FirmwareResult<()> {
        if *failures >= MAX_TRANSFER_RETRIES {
            return Err(error);
        }
        let delay = self.limits.retry_delay * (1_u32 << *failures);
        *failures += 1;
        timeout_at(deadline, tokio::time::sleep(delay))
            .await
            .map_err(|_| self.timed_out())?;
        Ok(())
    }

    /// Report the cumulative network budget without exposing transport settings.
    fn timed_out(&self) -> FirmwareError {
        FirmwareError::SourceTransferTimeout {
            timeout: self.limits.timeout,
        }
    }
}

#[cfg(test)]
mod tests;
