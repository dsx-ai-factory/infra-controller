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
use crate::error::{TlsFile, TlsMaterialError};

impl TlsFile {
    /// The path the configuration names for this file.
    fn path(self, config: &KmipClientConfig) -> &Path {
        match self {
            Self::CaBundle => &config.ca_bundle,
            Self::ClientCert => &config.client_cert,
            Self::ClientKey => &config.client_key,
        }
    }
}

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
    let read = |file: TlsFile| {
        let path = file.path(config);
        std::fs::read(path).map_err(|source| TlsMaterialError::Read {
            file,
            path: path.to_path_buf(),
            source,
        })
    };
    Ok(PemMaterial {
        ca_bundle: read(TlsFile::CaBundle)?,
        client_cert: read(TlsFile::ClientCert)?,
        client_key: Zeroizing::new(read(TlsFile::ClientKey)?),
    })
}

/// Reads the PEM files without blocking the runtime, for (re)connects.
pub(crate) async fn read_material(
    config: &KmipClientConfig,
) -> Result<PemMaterial, TlsMaterialError> {
    async fn read(file: TlsFile, config: &KmipClientConfig) -> Result<Vec<u8>, TlsMaterialError> {
        let path = file.path(config);
        tokio::fs::read(path)
            .await
            .map_err(|source| TlsMaterialError::Read {
                file,
                path: path.to_path_buf(),
                source,
            })
    }
    Ok(PemMaterial {
        ca_bundle: read(TlsFile::CaBundle, config).await?,
        client_cert: read(TlsFile::ClientCert, config).await?,
        client_key: Zeroizing::new(read(TlsFile::ClientKey, config).await?),
    })
}

/// Parses the material into a client configuration that trusts only the CA
/// bundle and presents the client certificate.
pub(crate) fn build_client_config(
    config: &KmipClientConfig,
    material: &PemMaterial,
) -> Result<ClientConfig, TlsMaterialError> {
    let mut roots = RootCertStore::empty();
    for certificate in parse_certificates(TlsFile::CaBundle, config, &material.ca_bundle)? {
        roots
            .add(certificate)
            .map_err(|source| TlsMaterialError::Rejected {
                file: TlsFile::CaBundle,
                path: config.ca_bundle.clone(),
                source,
            })?;
    }
    let chain = parse_certificates(TlsFile::ClientCert, config, &material.client_cert)?;
    let key = rustls_pemfile::private_key(&mut material.client_key.as_slice())
        .map_err(|source| TlsMaterialError::Parse {
            file: TlsFile::ClientKey,
            path: config.client_key.clone(),
            source,
        })?
        .ok_or_else(|| TlsMaterialError::Empty {
            file: TlsFile::ClientKey,
            path: config.client_key.clone(),
            expected: "private key",
        })?;
    ClientConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(TlsMaterialError::Builder)?
        .with_root_certificates(roots)
        .with_client_auth_cert(chain, key)
        .map_err(|source| TlsMaterialError::Rejected {
            file: TlsFile::ClientCert,
            path: config.client_cert.clone(),
            source,
        })
}

fn parse_certificates(
    file: TlsFile,
    config: &KmipClientConfig,
    pem: &[u8],
) -> Result<Vec<CertificateDer<'static>>, TlsMaterialError> {
    let path = file.path(config);
    let certificates = rustls_pemfile::certs(&mut &pem[..])
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| TlsMaterialError::Parse {
            file,
            path: path.to_path_buf(),
            source,
        })?;
    if certificates.is_empty() {
        return Err(TlsMaterialError::Empty {
            file,
            path: path.to_path_buf(),
            expected: "certificates",
        });
    }
    Ok(certificates)
}
