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

//! Fixtures shared by the proxy's tests.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rpc::forge::find_bmc_ips_request::LookupBy;
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use tokio::task::JoinSet;
use tokio_rustls::rustls::pki_types::CertificateDer;
use tokio_util::sync::CancellationToken;

use crate::config::SwitchTlsConfig;
use crate::proxy::admission::Admission;
use crate::proxy::credentials::{
    BmcCredentials, CREDENTIAL_CACHE_IDLE_TTL, SwitchProbe, TargetCredentials, TargetKind,
};
use crate::proxy::target::IP_CACHE_TTL;
use crate::proxy::upstream::UpstreamClients;
use crate::proxy::{BmcProxyState, bounded_cache, idle_bounded_cache};

const TEST_CONFIG: &str = r#"
    [tls]
    identity_pemfile_path = ""
    identity_keyfile_path = ""
    root_cafile_path = ""
    admin_root_cafile_path = ""

    [auth]
"#;

#[derive(Debug, PartialEq)]
pub(super) enum CredentialSummary {
    UsernamePassword { username: String, password: String },
    SessionToken { token: String },
}

pub(super) fn test_state_with_config(config: &str) -> BmcProxyState {
    let client_config = ForgeClientConfig::default();
    let api_config = ApiConfig::new("https://example.com", &client_config);
    let config = Arc::new(crate::Config::parse(config).expect("test config should parse"));

    // A test's admission tasks run until its runtime stops.
    let mut tasks = JoinSet::new();
    let admission = Admission::start(
        &config.classes,
        &config.admission,
        CancellationToken::new(),
        &mut tasks,
    );
    tasks.detach_all();
    BmcProxyState {
        upstream_clients: Arc::new(
            UpstreamClients::new(config.redirects.mode, config.tls.switch.as_ref())
                .expect("test HTTP clients build"),
        ),
        config,
        api_client: ForgeApiClient::new(&api_config),
        credential_cache: idle_bounded_cache(CREDENTIAL_CACHE_IDLE_TTL),
        ip_cache: bounded_cache(IP_CACHE_TTL),
        admission,
        switch_probe: Arc::new(SwitchProbe::default()),
    }
}

pub(super) async fn test_state_with_ip_cache(
    seeded_ips: HashMap<LookupBy, IpAddr>,
) -> BmcProxyState {
    let state = test_state_with_config(TEST_CONFIG);
    for (lookup_by, ip) in seeded_ips {
        state.ip_cache.insert(lookup_by, ip).await;
    }
    state
}

/// `credentials` as the cache holds them for a BMC's address.
pub(super) fn for_bmc(credentials: BmcCredentials) -> TargetCredentials {
    TargetCredentials {
        target: TargetKind::Bmc,
        credentials,
    }
}

/// A CA standing in for a site's switch issuer: the one switches' server
/// certificates chain to, and that issues the client certificates they
/// accept.
pub(super) struct SwitchCa {
    certificate: rcgen::Certificate,
    issuer: Issuer<'static, KeyPair>,
}

impl SwitchCa {
    pub(super) fn new() -> Self {
        Self::with(|_| {})
    }

    /// A CA whose certificate expired in 2021.
    pub(super) fn expired() -> Self {
        Self::with(|params| {
            params.not_before = rcgen::date_time_ymd(2020, 1, 1);
            params.not_after = rcgen::date_time_ymd(2021, 1, 1);
        })
    }

    fn with(adjust: impl FnOnce(&mut CertificateParams)) -> Self {
        let mut params = CertificateParams::new(Vec::new()).expect("CA parameters");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(DnType::CommonName, "switch test CA");
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        adjust(&mut params);
        let key = KeyPair::generate().expect("CA key");
        let certificate = params.self_signed(&key).expect("CA certificate");
        Self {
            certificate,
            issuer: Issuer::new(params, key),
        }
    }

    /// The CA's own certificate.
    pub(super) fn certificate(&self) -> CertificateDer<'static> {
        self.certificate.der().clone()
    }

    /// A certificate the CA issues for `names`, DNS names or IPs, to
    /// authenticate as `usage`, and its key.
    pub(super) fn issue(
        &self,
        names: &[&str],
        usage: ExtendedKeyUsagePurpose,
    ) -> (rcgen::Certificate, KeyPair) {
        let names: Vec<String> = names.iter().map(ToString::to_string).collect();
        let mut params = CertificateParams::new(names).expect("leaf parameters");
        params.extended_key_usages = vec![usage];
        self.issue_with(params)
    }

    /// A certificate the CA issues as `params` describe, and its key.
    pub(super) fn issue_with(&self, params: CertificateParams) -> (rcgen::Certificate, KeyPair) {
        let key = KeyPair::generate().expect("leaf key");
        let certificate = params
            .signed_by(&key, &self.issuer)
            .expect("leaf certificate");
        (certificate, key)
    }

    /// `[tls.switch]` naming files written into `dir`: this CA, and a client
    /// certificate it issues the proxy, with its key.
    pub(super) fn switch_tls(&self, dir: &Path, tls_server_name: Option<&str>) -> SwitchTlsConfig {
        let (client, key) = self.issue(&[], ExtendedKeyUsagePurpose::ClientAuth);
        let write = |name: &str, contents: String| {
            let path = dir.join(name);
            std::fs::write(&path, contents).expect("TLS file writes");
            path.to_str().expect("UTF-8 path").to_string()
        };
        SwitchTlsConfig {
            ca_cert_path: write("ca.crt", self.certificate.pem()),
            client_cert_path: write("tls.crt", client.pem()),
            client_key_path: write("tls.key", key.serialize_pem()),
            tls_server_name: tls_server_name.map(str::to_string),
        }
    }
}

pub(super) fn summarize_credentials(credentials: BmcCredentials) -> CredentialSummary {
    match credentials {
        BmcCredentials::UsernamePassword { username, password } => {
            CredentialSummary::UsernamePassword { username, password }
        }
        BmcCredentials::SessionToken { token } => CredentialSummary::SessionToken { token },
    }
}
