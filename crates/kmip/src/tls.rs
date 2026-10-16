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
//! Loading the mutual-TLS material named by the configuration into a rustls
//! client configuration on the workspace's aws-lc-rs provider.

use std::path::Path;
use std::sync::Arc;

use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::CertificateDer;
use zeroize::Zeroizing;

use crate::config::KmipClientConfig;
use crate::error::TlsMaterialError;

const CA_BUNDLE: &str = "ca_bundle";
const CLIENT_CERT: &str = "client_cert";
const CLIENT_KEY: &str = "client_key";

/// The three PEM files named by the configuration, read but not yet parsed.
pub(crate) struct PemMaterial {
    ca_bundle: Vec<u8>,
    client_cert: Vec<u8>,
    client_key: Zeroizing<Vec<u8>>,
}

/// Reads the PEM files synchronously, for validation at construction time.
pub(crate) fn read_material_blocking(
    config: &KmipClientConfig,
) -> Result<PemMaterial, TlsMaterialError> {
    let read = |role: &'static str, path: &Path| {
        std::fs::read(path).map_err(|source| TlsMaterialError::Read {
            role,
            path: path.to_path_buf(),
            source,
        })
    };
    Ok(PemMaterial {
        ca_bundle: read(CA_BUNDLE, &config.ca_bundle)?,
        client_cert: read(CLIENT_CERT, &config.client_cert)?,
        client_key: Zeroizing::new(read(CLIENT_KEY, &config.client_key)?),
    })
}

/// Reads the PEM files without blocking the runtime, for (re)connects.
pub(crate) async fn read_material(
    config: &KmipClientConfig,
) -> Result<PemMaterial, TlsMaterialError> {
    async fn read(role: &'static str, path: &Path) -> Result<Vec<u8>, TlsMaterialError> {
        tokio::fs::read(path)
            .await
            .map_err(|source| TlsMaterialError::Read {
                role,
                path: path.to_path_buf(),
                source,
            })
    }
    Ok(PemMaterial {
        ca_bundle: read(CA_BUNDLE, &config.ca_bundle).await?,
        client_cert: read(CLIENT_CERT, &config.client_cert).await?,
        client_key: Zeroizing::new(read(CLIENT_KEY, &config.client_key).await?),
    })
}

/// Parses the material into a client configuration that trusts only the CA
/// bundle and presents the client certificate.
pub(crate) fn build_client_config(
    config: &KmipClientConfig,
    material: &PemMaterial,
) -> Result<ClientConfig, TlsMaterialError> {
    let mut roots = RootCertStore::empty();
    for certificate in parse_certificates(CA_BUNDLE, &config.ca_bundle, &material.ca_bundle)? {
        roots
            .add(certificate)
            .map_err(|source| TlsMaterialError::Rejected {
                role: CA_BUNDLE,
                path: config.ca_bundle.clone(),
                source,
            })?;
    }
    let chain = parse_certificates(CLIENT_CERT, &config.client_cert, &material.client_cert)?;
    let key = rustls_pemfile::private_key(&mut material.client_key.as_slice())
        .map_err(|source| TlsMaterialError::Parse {
            role: CLIENT_KEY,
            path: config.client_key.clone(),
            source,
        })?
        .ok_or_else(|| TlsMaterialError::Empty {
            role: CLIENT_KEY,
            path: config.client_key.clone(),
            expected: "private key",
        })?;
    ClientConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(TlsMaterialError::Builder)?
        .with_root_certificates(roots)
        .with_client_auth_cert(chain, key)
        .map_err(|source| TlsMaterialError::Rejected {
            role: CLIENT_CERT,
            path: config.client_cert.clone(),
            source,
        })
}

fn parse_certificates(
    role: &'static str,
    path: &Path,
    pem: &[u8],
) -> Result<Vec<CertificateDer<'static>>, TlsMaterialError> {
    let certificates = rustls_pemfile::certs(&mut &pem[..])
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| TlsMaterialError::Parse {
            role,
            path: path.to_path_buf(),
            source,
        })?;
    if certificates.is_empty() {
        return Err(TlsMaterialError::Empty {
            role,
            path: path.to_path_buf(),
            expected: "certificates",
        });
    }
    Ok(certificates)
}
