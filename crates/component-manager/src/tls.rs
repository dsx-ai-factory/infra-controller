// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};
use trace_propagation::TraceInjectService;

use crate::config::BackendTlsConfig;
use crate::error::ComponentManagerError;

/// Build a tonic `Channel` to `url`, optionally configured with mTLS, wrapped so every request it
/// sends carries the current span's W3C trace context (issue #2438). Because the wrap lives here,
/// any backend built on top of `build_channel` gets trace propagation automatically.
///
/// When `tls` is `None`, a plain-text channel is returned (suitable for
/// local development or in-cluster plaintext).  When `Some`, the channel
/// is configured with:
///   - CA certificate for server verification
///   - Client certificate + key for mutual TLS (if both are present)
pub async fn build_channel(
    url: &str,
    tls: Option<&BackendTlsConfig>,
    service_label: &str,
) -> Result<TraceInjectService<Channel>, ComponentManagerError> {
    let endpoint = Channel::from_shared(url.to_owned()).map_err(|e| {
        ComponentManagerError::InvalidArgument(format!("invalid {service_label} URL: {e}"))
    })?;

    let channel = match tls {
        Some(tls_cfg) => {
            let mut client_tls = ClientTlsConfig::new();

            if let Some(domain) = &tls_cfg.domain {
                client_tls = client_tls.domain_name(domain);
            }

            if let Some(ca_path) = tls_cfg.resolve_ca_cert_path() {
                let ca_pem = tokio::fs::read(&ca_path).await.map_err(|e| {
                    ComponentManagerError::Internal(format!(
                        "{service_label}: failed to read CA cert at {ca_path}: {e}"
                    ))
                })?;
                client_tls = client_tls.ca_certificate(Certificate::from_pem(ca_pem));
            }

            let cert_path = tls_cfg.resolve_client_cert_path();
            let key_path = tls_cfg.resolve_client_key_path();
            if let (Some(cp), Some(kp)) = (cert_path, key_path) {
                let cert_pem = tokio::fs::read(&cp).await.map_err(|e| {
                    ComponentManagerError::Internal(format!(
                        "{service_label}: failed to read client cert at {cp}: {e}"
                    ))
                })?;
                let key_pem = tokio::fs::read(&kp).await.map_err(|e| {
                    ComponentManagerError::Internal(format!(
                        "{service_label}: failed to read client key at {kp}: {e}"
                    ))
                })?;
                client_tls = client_tls.identity(Identity::from_pem(cert_pem, key_pem));
            }

            endpoint
                .tls_config(client_tls)
                .map_err(|e| {
                    ComponentManagerError::Internal(format!(
                        "{service_label}: TLS configuration error: {e}"
                    ))
                })?
                .connect()
                .await?
        }
        None => endpoint.connect().await?,
    };

    Ok(TraceInjectService::new(channel))
}
