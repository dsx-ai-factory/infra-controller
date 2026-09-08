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

use std::borrow::Cow;
use std::net::Ipv6Addr;
use std::str::FromStr;
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use carbide_instrument::red;
use carbide_secrets::credentials::{CredentialReader, Credentials};
use carbide_utils::HostPortPair;
use carbide_utils::redfish::format_forwarded_host_parameter;
use libredfish::model::service_root::RedfishVendor;
use libredfish::{Endpoint, Redfish};

use crate::libredfish::instrumented::{InstrumentedRedfish, REDFISH_BACKEND};
use crate::libredfish::{RedfishAuth, RedfishClientCreationError, RedfishClientPool};

/// Formats a host for the URL authority that `libredfish` constructs internally.
///
/// `libredfish::Endpoint` accepts a host and port separately, but the pinned
/// implementation interpolates them into a URL string. Keep callers' host values
/// bare and add IPv6 brackets only at this external serialization boundary.
fn libredfish_endpoint_host(host: &str) -> Cow<'_, str> {
    if host.parse::<Ipv6Addr>().is_ok() {
        Cow::Owned(format!("[{host}]"))
    } else {
        Cow::Borrowed(host)
    }
}

pub(super) struct RedfishClientPoolImpl {
    pool: libredfish::RedfishClientPool,
    credential_reader: Arc<dyn CredentialReader>,
    proxy_address: Arc<ArcSwap<Option<HostPortPair>>>,
}

impl RedfishClientPoolImpl {
    pub(super) fn new(
        credential_reader: Arc<dyn CredentialReader>,
        pool: libredfish::RedfishClientPool,
        proxy_address: Arc<ArcSwap<Option<HostPortPair>>>,
    ) -> Self {
        RedfishClientPoolImpl {
            credential_reader,
            pool,
            proxy_address,
        }
    }
}

#[async_trait]
impl RedfishClientPool for RedfishClientPoolImpl {
    async fn create_client(
        &self,
        host: &str,
        port: Option<u16>,
        auth: RedfishAuth,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        let original_host = host;

        // Allow globally overriding the bmc port via site-config. We read this on every call to
        // create_client, because self.proxy_address is a dynamic setting.
        let proxy_address = self.proxy_address.load();
        let (host, port, add_custom_header) = match proxy_address.as_ref() {
            // No override
            None => (host, port, false),
            // Override the host and port
            Some(HostPortPair::HostAndPort(h, p)) => (h.as_str(), Some(*p), true),
            // Only override the host
            Some(HostPortPair::HostOnly(h)) => (h.as_str(), port, true),
            // Only override the port
            Some(HostPortPair::PortOnly(p)) => (host, Some(*p), false),
        };

        let (username, password) = match auth {
            RedfishAuth::Anonymous => (None, None), // anonymous login, usually to get service root Vendor info
            RedfishAuth::Direct(username, password) => (Some(username), Some(password)),
            RedfishAuth::Key(credential_key) => {
                let credentials = self
                    .credential_reader
                    .get_credentials(&credential_key)
                    .await?
                    .ok_or_else(|| RedfishClientCreationError::MissingCredentials {
                        key: credential_key.to_key_str().to_string(),
                    })?;

                let (username, password) = match credentials {
                    Credentials::UsernamePassword { username, password } => {
                        (Some(username), Some(password))
                    }
                };

                (username, password)
            }
        };

        let endpoint = Endpoint {
            host: libredfish_endpoint_host(host).into_owned(),
            port,
            user: username,
            password,
        };

        let custom_headers = if add_custom_header {
            // If we're overriding the host, inject a header indicating the IP address we were
            // originally going to use, using the HTTP "Forwarded" header:
            // https://datatracker.ietf.org/doc/html/rfc7239

            // Override host only if host value is provided in config.
            vec![(
                http::HeaderName::from_str("forwarded")
                    .map_err(|err| RedfishClientCreationError::InvalidHeader(err.to_string()))?,
                format_forwarded_host_parameter(original_host),
            )]
        } else {
            Vec::default()
        };

        // The initializing paths below make HTTP calls of their own, so they
        // are metered like any other Redfish operation.
        let client = match vendor {
            // Auto-detect vendor from the service root.
            None => red::instrumented(
                REDFISH_BACKEND,
                "create_client",
                self.pool
                    .create_client_with_custom_headers(endpoint, custom_headers),
            )
            .await
            .map_err(RedfishClientCreationError::RedfishError)?,
            // Unknown means "no vendor" — return a standard client without
            // making any HTTP calls (used by the anonymous probe client).
            // This restores the behavior of the old `initialize: false` path
            // which called create_standard_client. The full initialization
            // path (create_client_with_vendor) makes HTTP calls to /Systems,
            // /Managers, etc. that fail with 401 on BMCs requiring auth.
            // With no I/O here, there is no external call to meter either.
            Some(RedfishVendor::Unknown) => self
                .pool
                .create_standard_client_with_custom_headers(endpoint, custom_headers)
                .map_err(RedfishClientCreationError::RedfishError)
                .map(|c| c as Box<dyn Redfish>)?,
            // Use the provided vendor directly.
            Some(vendor) => red::instrumented(
                REDFISH_BACKEND,
                "create_client",
                self.pool
                    .create_client_with_vendor(endpoint, vendor, custom_headers),
            )
            .await
            .map_err(RedfishClientCreationError::RedfishError)?,
        };

        // Every client the pool creates goes out decorated, so each Redfish
        // call records the per-operation RED triad no matter the call site.
        Ok(Box::new(InstrumentedRedfish::new(client)))
    }

    fn credential_reader(&self) -> &dyn CredentialReader {
        &*self.credential_reader
    }
}

impl super::sealed::Sealed for RedfishClientPoolImpl {}

#[async_trait]
impl super::BmcCredentialOps for RedfishClientPoolImpl {}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::libredfish_endpoint_host;

    #[test]
    fn endpoint_host_brackets_only_bare_ipv6_literals() {
        value_scenarios!(run = |host| libredfish_endpoint_host(host).into_owned();
            "unchanged hosts" {
                "bmc.example.com" => "bmc.example.com".to_string(),
                "192.0.2.10" => "192.0.2.10".to_string(),
                "[2001:db8::10]" => "[2001:db8::10]".to_string(),
            }

            "bracketed IPv6 host" {
                "2001:db8::10" => "[2001:db8::10]".to_string(),
            }
        );
    }
}
