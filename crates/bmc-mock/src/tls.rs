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

use std::ffi::OsStr;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use rustls::ServerConfig;
use rustls::pki_types::PrivateKeyDer;
use rustls_pemfile::Item;

#[derive(Debug)]
pub enum Error {
    CargoManifestCheckFail(std::io::Error),
    CertFileRead(std::io::Error),
    KeyFileRead(std::io::Error),
    CertPemFile(std::io::Error),
    KeyPemFile(std::io::Error),
    PrivateKeyCreate(&'static str),
    NoKeysFound,
    ConfigBuild(rustls::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CargoManifestCheckFail(_) => {
                "failed to check dir existance from CARGO_MANIFEST_DIR".fmt(f)
            }
            Self::CertFileRead(_) => "failed to read cert file".fmt(f),
            Self::KeyFileRead(_) => "failed to read key file".fmt(f),
            Self::CertPemFile(_) => "cert pem file error".fmt(f),
            Self::KeyPemFile(_) => "key pem file error".fmt(f),
            Self::PrivateKeyCreate(_) => "cannot create private key".fmt(f),
            Self::NoKeysFound => "no keys found in keys file".fmt(f),
            Self::ConfigBuild(_) => "config build error".fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CargoManifestCheckFail(v) => Some(v),
            Self::CertFileRead(v) => Some(v),
            Self::KeyFileRead(v) => Some(v),
            Self::CertPemFile(v) => Some(v),
            Self::KeyPemFile(v) => Some(v),
            Self::PrivateKeyCreate(_) => None,
            Self::NoKeysFound => None,
            Self::ConfigBuild(v) => Some(v),
        }
    }
}

pub fn server_config(cert_path: Option<impl AsRef<OsStr>>) -> Result<ServerConfig, Error> {
    let cert_path = match cert_path.as_ref() {
        Some(cert_path) => Path::new(cert_path),
        None => {
            let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
            if manifest_dir
                .try_exists()
                .map_err(Error::CargoManifestCheckFail)?
            {
                manifest_dir
            } else {
                Path::new("/opt/carbide")
            }
        }
    };

    let mut cert_file = cert_path.join("tls.crt");
    let mut key_file = cert_path.join("tls.key");
    if !cert_file.exists() {
        // let's try once more. This can be docker-compose case.
        let root_var =
            std::env::var("REPO_ROOT").expect("Could not find the crt file for bmc-mock.");
        let root_dir = Path::new(&root_var);
        let cert_path = root_dir.join("crates/bmc-mock");
        cert_file = cert_path.join("tls.crt");
        key_file = cert_path.join("tls.key");
    }
    tracing::info!(
        cert_file = ?cert_file,
        key_file = ?key_file,
        "Loading BMC mock TLS certificate and key",
    );
    let tls_cert = std::fs::read(cert_file).map_err(Error::CertFileRead)?;
    let tls_key = std::fs::read(key_file).map_err(Error::KeyFileRead)?;

    // Note: Axum has a simple RustlsConfig::from_pem we could use, but it constructs a rustls
    // ServerConfig without a default crypto provider. So we have to make our own rustls::ServerConfig
    // and pass that to RustlsConfig::from.
    let certs = rustls_pemfile::certs(&mut tls_cert.to_vec().as_ref())
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::CertPemFile)?;

    // Check the entire PEM file for the key in case it is not first section
    let key = rustls_pemfile::read_all(&mut tls_key.to_vec().as_ref())
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::KeyPemFile)?
        .into_iter()
        .filter_map(|i| match i {
            Item::Sec1Key(key) => Some(key.secret_sec1_der().to_vec()),
            Item::Pkcs1Key(key) => Some(key.secret_pkcs1_der().to_vec()),
            Item::Pkcs8Key(key) => Some(key.secret_pkcs8_der().to_vec()),
            _ => None,
        })
        .map(|data| PrivateKeyDer::try_from(data).map_err(Error::PrivateKeyCreate))
        .next()
        .ok_or(Error::NoKeysFound)??;

    let mut server_config = ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .map_err(Error::ConfigBuild)?;
    // This is what axum is normally doing for you
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(server_config)
}
