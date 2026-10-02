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
use std::sync::Arc;

use rpc::forge::find_bmc_ips_request::LookupBy;
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::proxy::admission::Admission;
use crate::proxy::credentials::{BmcCredentials, CREDENTIAL_CACHE_IDLE_TTL};
use crate::proxy::target::IP_CACHE_TTL;
use crate::proxy::upstream::build_http_client;
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
        http_client: build_http_client(config.redirects.mode).expect("test HTTP client builds"),
        config,
        api_client: ForgeApiClient::new(&api_config),
        credential_cache: idle_bounded_cache(CREDENTIAL_CACHE_IDLE_TTL),
        ip_cache: bounded_cache(IP_CACHE_TTL),
        admission,
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

pub(super) fn summarize_credentials(credentials: BmcCredentials) -> CredentialSummary {
    match credentials {
        BmcCredentials::UsernamePassword { username, password } => {
            CredentialSummary::UsernamePassword { username, password }
        }
        BmcCredentials::SessionToken { token } => CredentialSummary::SessionToken { token },
    }
}
