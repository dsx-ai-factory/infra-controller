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

use std::time::Duration;

use ::rpc::forge::{AttestQuoteRequest, MachineCertificate};
use ::rpc::forge_tls_client::{ForgeClientConfig, ForgeClientT, ForgeTlsClient};
use ::rpc::{MachineDiscoveryInfo, forge as rpc, machine_discovery as rpc_discovery};
use carbide_uuid::machine::MachineId;
use eyre::WrapErr;
use forge_tls::client_config::ClientCert;
use forge_tls::default as tls_default;
use tryhard::RetryFutureConfig;

#[derive(thiserror::Error, Debug)]
pub enum RegistrationError {
    #[error("transport error {0}")]
    TransportError(String),
    #[error("tonic status error {0}")]
    TonicStatusError(#[from] tonic::Status),
    #[error("missing machine id in API server response. should be impossible")]
    MissingMachineId,
    #[error("attestation failed")]
    AttestationFailed,
    #[error("failed to retrieve or write client certificate: {0}")]
    ClientCertificateError(eyre::Report),
    #[error("missing certificate in DiscoverMachine reply")]
    MissingCertificate,
}

/// Data that is retrieved from the Forge API server during registration
#[derive(Debug, Clone)]
pub struct RegistrationData {
    /// The machine ID under which this machine is known in Forge
    pub machine_id: MachineId,
}

#[derive(Clone)]
pub struct DiscoveryRetry {
    pub secs: u64,
    pub max: u32,
}
// RegistrationClient is a small wrapper client that handles
// doing async retries of machine discovery requests. Since
// everything here is async, retrying futures gets interesting,
// because values get moved into them (as in needing to clone or
// recreate the underlying forge_tls_client, gRPC message, etc.
//
// This could have also just gone inline with register_machine,
// but breaking the code out like this does help to make
// the register_machine flow a little bit cleaner. This also
// could have just been its own function, instead of a struct,
// but I sort of have high hopes for maybe eventually making
// this so generic that we can use it for other things.
struct RegistrationClient<'a, 'c> {
    // api is the Forge API URL.
    api_url: &'a str,

    // config is, quite obviously, a reference
    // to a ForgeClientConfig to use.
    config: &'c ForgeClientConfig,

    retry: DiscoveryRetry,
}

impl<'a, 'c> RegistrationClient<'a, 'c> {
    // new creates a new RegistrationClient, where the
    // only things needed here are references to the API
    // URL and the corresponding ForgeClientConfig.
    fn new(api_url: &'a str, config: &'c ForgeClientConfig, retry: DiscoveryRetry) -> Self {
        Self {
            api_url,
            config,
            retry,
        }
    }

    // connect will create a new ForgeTlsClient and return an underlying
    // ForgeClientT connection for callers to leverage, returning an error
    // if we were unable to create a client (e.g. if there was an issue
    // loading certificates), or establish a connection. All calls from
    // devices -> carbide-api generally follow this pattern, so you merely
    // need to just do something like:
    //
    // let request = tonic::Request::new(your_request);
    // let mut connection = self.connect("<what-youre-doing>").await?;
    // let response = connection.your_api_endpoint(request).await?;
    //
    async fn connect(&self, purpose: &str) -> Result<ForgeClientT, RegistrationError> {
        tracing::debug!(purpose, "creating tls client connection");
        let client = ForgeTlsClient::new(self.config);
        match client.build(self.api_url.to_string()).await {
            Ok(connection) => {
                tracing::debug!(purpose, ?connection, "created tls client connection");
                Ok(connection)
            }
            Err(e) => {
                tracing::error!(purpose, error = ?e, "could not create tls client");
                Err(RegistrationError::TransportError(e.to_string()))
            }
        }
    }

    // discover_machine_once is a single future attempt of
    // trying to send MachineDiscoveryInfo to the API, creating
    // a new connection + wrapped request for each iteration
    // of the retry.
    async fn discover_machine_once(
        &self,
        info: MachineDiscoveryInfo,
        attempt: u32,
    ) -> Result<rpc::MachineDiscoveryResult, RegistrationError> {
        tracing::info!(attempt, "Attempting to discover_machine");

        // Create a new connection off of the ForgeTlsClient.
        let mut connection = self.connect("discover_machine_once").await?;

        // Create a new request with the provided MachineDiscoveryInfo.
        let request = tonic::Request::new(info);
        tracing::debug!(?request, "discover_machine request");

        // And now attempt to send the request.
        Ok(connection
            .discover_machine(request)
            .await
            .inspect_err(|err| {
                tracing::error!(
                    attempt,
                    error = %err,
                    "Error attempting to discover_machine"
                );
            })?
            .into_inner())
    }

    // discover_machine is a retrying wrapper around making
    // discover_machine gRPC calls to the Carbide API.
    async fn discover_machine(
        &mut self,
        info: MachineDiscoveryInfo,
    ) -> Result<rpc::MachineDiscoveryResult, RegistrationError> {
        // The retry config is currently hard-coded in here to be
        // every minute for a week. Basically, keep trying every
        // minute for a while. This could probably become something
        // that is configurable.
        let config = RetryFutureConfig::new(self.retry.max)
            .fixed_backoff(Duration::from_secs(self.retry.secs));
        let mut attempt = 0;
        tryhard::retry_fn(|| {
            attempt += 1;
            self.discover_machine_once(info.clone(), attempt)
        })
        .with_config(config)
        .await
    }

    async fn attest_quote(
        &self,
        quote: &AttestQuoteRequest,
    ) -> Result<rpc::AttestQuoteResponse, RegistrationError> {
        // Create a new connection off of the ForgeTlsClient.
        let mut connection = self.connect("attest_quote").await?;

        // Create a new request with the provided AttestQuoteRequest.
        let request = tonic::Request::new(quote.clone());
        tracing::debug!(?request, "attest_quote request");

        // And now attempt to send the request.
        Ok(connection
            .attest_quote(request)
            .await
            .inspect_err(|err| tracing::error!(error = %err, "Error attempting to attest_quote"))?
            .into_inner())
    }
}

// create_client_config creates a new ForgeClientConfig. All
// calls in here follow the same pattern, so this was moved out
// into its own function to clean up tons of duplication.
fn create_client_config(
    purpose: &str,
    use_mgmt_vrf: bool,
    root_ca: String,
) -> Result<ForgeClientConfig, RegistrationError> {
    let forge_client_config = match use_mgmt_vrf {
        true => ForgeClientConfig::new(root_ca, None)
            .use_mgmt_vrf()
            .map_err(|e| RegistrationError::TransportError(e.to_string()))?,
        false => ForgeClientConfig::new(root_ca, None),
    };
    tracing::debug!(purpose, ?forge_client_config, "client_config");
    Ok(forge_client_config)
}

/// Registers a machine at the Forge API server for further interactions
///
/// Returns information about the machine that is known by the API server
#[allow(clippy::too_many_arguments)]
pub async fn register_machine(
    forge_api: &str,
    root_ca: String,
    machine_interface_id: Option<uuid::Uuid>,
    hardware_info: rpc_discovery::DiscoveryInfo,
    use_mgmt_vrf: bool,
    retry: DiscoveryRetry,
    create_machine: bool,
    require_client_certificates: bool,
    discovery_reporter: ::rpc::MachineDiscoveryReporter,
    reporter_version: Option<String>,
) -> Result<
    (
        RegistrationData,
        Option<rpc::AttestKeyBindChallenge>,
        Option<uuid::Uuid>,
    ),
    RegistrationError,
> {
    let info = rpc::MachineDiscoveryInfo {
        machine_interface_id: machine_interface_id.map(|mid| mid.into()),
        discovery_data: Some(::rpc::forge::machine_discovery_info::DiscoveryData::Info(
            hardware_info,
        )),
        create_machine,
        discovery_reporter: discovery_reporter as i32,
        discovery_reporter_version: reporter_version,
    };
    tracing::info!(machine_discovery_info = ?info, "register_machine discovery_info");

    let forge_client_config = create_client_config("register_machine", use_mgmt_vrf, root_ca)?;
    let response = RegistrationClient::new(forge_api, &forge_client_config, retry)
        .discover_machine(info)
        .await?;

    if response.machine_certificate.is_none() && require_client_certificates {
        return Err(RegistrationError::MissingCertificate);
    }

    if response.machine_certificate.is_some() {
        match write_certs(response.machine_certificate, None).await {
            Ok(()) => {}
            Err(e) => return Err(RegistrationError::ClientCertificateError(e)),
        }
    }

    let machine_id = response
        .machine_id
        .ok_or(RegistrationError::MissingMachineId)?;
    tracing::info!(%machine_id, "Registered");

    Ok((
        RegistrationData { machine_id },
        response.attest_key_challenge,
        response.machine_interface_id.map(uuid::Uuid::from),
    ))
}

pub async fn attest_quote(
    forge_api: &str,
    root_ca: String,
    use_mgmt_vrf: bool,
    retry: DiscoveryRetry,
    quote: &AttestQuoteRequest,
) -> Result<bool, RegistrationError> {
    tracing::info!("registration client sending attest_quote");

    let forge_client_config = create_client_config("attest_quote", use_mgmt_vrf, root_ca)?;
    let response = RegistrationClient::new(forge_api, &forge_client_config, retry)
        .attest_quote(quote)
        .await?;

    let _ = write_certs(response.machine_certificate, None).await;

    tracing::info!(success = response.success, "Attestation result");

    Ok(response.success)
}

pub async fn write_certs(
    machine_certificate: Option<MachineCertificate>,
    override_client_cert: Option<&ClientCert>,
) -> Result<(), eyre::Report> {
    if let Some(mut machine_certificate) = machine_certificate {
        let mut combined_cert = Vec::with_capacity(
            machine_certificate.public_key.len() + machine_certificate.issuing_ca.len() + 1,
        );
        combined_cert.append(&mut machine_certificate.public_key);
        combined_cert.append(&mut "\n".to_string().into_bytes());
        combined_cert.append(&mut machine_certificate.issuing_ca);
        combined_cert.append(&mut "\n".to_string().into_bytes());
        let (client_cert, client_key) = match override_client_cert {
            Some(c) => (c.cert_path.as_str(), c.key_path.as_str()),
            None => (tls_default::CLIENT_CERT, tls_default::CLIENT_KEY),
        };
        tokio::fs::write(client_cert, combined_cert)
            .await
            .wrap_err(format!(
                "failed to write new machine certificate PEM to {client_cert}"
            ))?;
        tracing::info!(%client_cert, "Wrote new machine certificate PEM");

        write_private_key(client_key, machine_certificate.private_key.as_slice())
            .await
            .wrap_err(format!(
                "failed to write new machine certificate key to: {client_key}"
            ))?;
    } else {
        return Err(eyre::eyre!("write_certs: machine_certificate is empty"));
    }

    Ok(())
}

/// Writes the machine private key owner-readable only (0600). The key is
/// created with the restrictive mode from the first byte — not chmod'ed after
/// the fact — so there is no window where it sits world-readable; a pre-existing
/// key file from an older agent (written 0644 via plain `fs::write`) is
/// tightened as well. Everything that legitimately reads the key (dpu-agent,
/// scout, fmds, otelcol) runs as root, so 0600 root-owned loses nobody access.
#[cfg(unix)]
async fn write_private_key(path: &str, key: &[u8]) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;

    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .await?;
    // `mode` only applies at creation; tighten files that already existed.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .await?;
    file.write_all(key).await?;
    // `sync_all`, not `flush`: flushing only pushes the buffer into the kernel.
    // The agent's very next act is to use this key, and a DPU losing power
    // between the write and the writeback would leave a truncated or empty key
    // behind a certificate that looks installed — a state the mismatch check in
    // the minter reports as a failure but cannot repair without re-enrolment.
    file.sync_all().await
}

#[cfg(not(unix))]
async fn write_private_key(path: &str, key: &[u8]) -> Result<(), std::io::Error> {
    tokio::fs::write(path, key).await
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// `write_certs` concatenates and writes bytes without parsing them, so
    /// these are opaque sentinels rather than PEM. Real-looking `-----BEGIN EC
    /// PRIVATE KEY-----` markers would buy nothing here and trip secret
    /// scanners, which match the marker and not its contents.
    fn test_certificate() -> MachineCertificate {
        MachineCertificate {
            public_key: b"test-leaf-certificate-bytes".to_vec(),
            issuing_ca: b"test-issuing-ca-bytes".to_vec(),
            private_key: b"test-machine-private-key-bytes".to_vec(),
        }
    }

    fn key_mode(path: &str) -> u32 {
        std::fs::metadata(path)
            .expect("key metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[tokio::test]
    async fn private_key_is_written_owner_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = ClientCert {
            cert_path: dir.path().join("cert.pem").to_string_lossy().into_owned(),
            key_path: dir.path().join("cert.key").to_string_lossy().into_owned(),
        };

        write_certs(Some(test_certificate()), Some(&paths))
            .await
            .expect("write_certs succeeds");

        assert_eq!(key_mode(&paths.key_path), 0o600, "fresh key must be 0600");
        let cert = std::fs::read_to_string(&paths.cert_path).expect("cert readable");
        assert!(
            cert.contains("leaf") && cert.contains("ca"),
            "cert file carries leaf + chain"
        );
    }

    #[tokio::test]
    async fn pre_existing_loose_key_is_tightened_on_rewrite() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = ClientCert {
            cert_path: dir.path().join("cert.pem").to_string_lossy().into_owned(),
            key_path: dir.path().join("cert.key").to_string_lossy().into_owned(),
        };
        // A key written by an older agent via plain fs::write (umask default).
        std::fs::write(&paths.key_path, b"old key").expect("seed old key");
        std::fs::set_permissions(&paths.key_path, std::fs::Permissions::from_mode(0o644))
            .expect("loosen");

        write_certs(Some(test_certificate()), Some(&paths))
            .await
            .expect("write_certs succeeds");

        assert_eq!(
            key_mode(&paths.key_path),
            0o600,
            "renewal must tighten an old 0644 key"
        );
        let key = std::fs::read_to_string(&paths.key_path).expect("key readable by owner");
        assert!(
            key.contains("test-machine-private-key-bytes"),
            "key content replaced"
        );
    }
}
