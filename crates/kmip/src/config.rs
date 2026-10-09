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

//! Client configuration and its validation.

use std::path::PathBuf;
use std::time::Duration;

use crate::error::KmipError;

/// The IANA-registered KMIP port, used when `endpoint` names no port.
pub const DEFAULT_PORT: u16 = 5696;

/// Default per-attempt bound on TCP connect plus TLS handshake.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default per-attempt bound on writing a request and reading its complete
/// response.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Hard upper bound for `connect_timeout`; a larger value is a configuration
/// error.
pub const MAX_CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

/// Hard upper bound for `request_timeout`; a larger value is a configuration
/// error.
pub const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Where and how to reach one KMIP server.
///
/// Every field is validated by [`KmipClient::new`](crate::KmipClient::new);
/// the TLS files are read again on every (re)connect, so rotated certificates
/// take effect at the next connection without a restart.
#[derive(Clone, Debug)]
pub struct KmipClientConfig {
    /// `host:port`, `host`, `[v6-literal]:port` or `[v6-literal]` of the KMIP
    /// TLS listener. Omitting the port selects [`DEFAULT_PORT`].
    pub endpoint: String,

    /// The name the server certificate must match, also sent as SNI. Defaults
    /// to the host part of `endpoint`; an IP address matches an IP SAN.
    pub server_name: Option<String>,

    /// PEM bundle of the CA certificates trusted to sign the server
    /// certificate. System roots are never consulted.
    pub ca_bundle: PathBuf,

    /// PEM certificate chain presented to the server.
    pub client_cert: PathBuf,

    /// PEM private key (PKCS#8, PKCS#1 or SEC1) matching `client_cert`.
    pub client_key: PathBuf,

    /// Per-attempt bound on TCP connect plus TLS handshake. Must be non-zero
    /// and at most [`MAX_CONNECT_TIMEOUT`].
    pub connect_timeout: Duration,

    /// Per-attempt bound on writing one request and reading its complete
    /// response. Must be non-zero and at most [`MAX_REQUEST_TIMEOUT`].
    pub request_timeout: Duration,
}

impl KmipClientConfig {
    /// A configuration with the default timeouts and the server name taken
    /// from `endpoint`.
    pub fn new(
        endpoint: impl Into<String>,
        ca_bundle: impl Into<PathBuf>,
        client_cert: impl Into<PathBuf>,
        client_key: impl Into<PathBuf>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            server_name: None,
            ca_bundle: ca_bundle.into(),
            client_cert: client_cert.into(),
            client_key: client_key.into(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

/// Rejects a zero or out-of-range timeout, naming the field.
pub(crate) fn validate_timeouts(config: &KmipClientConfig) -> Result<(), KmipError> {
    check_timeout(
        "connect_timeout",
        config.connect_timeout,
        MAX_CONNECT_TIMEOUT,
    )?;
    check_timeout(
        "request_timeout",
        config.request_timeout,
        MAX_REQUEST_TIMEOUT,
    )
}

fn check_timeout(field: &'static str, value: Duration, max: Duration) -> Result<(), KmipError> {
    if value.is_zero() || value > max {
        return Err(KmipError::InvalidConfig {
            field,
            detail: format!("must be greater than zero and at most {max:?}, got {value:?}"),
        });
    }
    Ok(())
}

/// Splits `endpoint` into host and port. An IPv6 literal must be bracketed
/// when a port follows it; an unbracketed one is taken as a bare host.
pub(crate) fn parse_endpoint(endpoint: &str) -> Result<(String, u16), KmipError> {
    let invalid = |detail: String| KmipError::InvalidConfig {
        field: "endpoint",
        detail,
    };
    let endpoint = endpoint.trim();
    let (host, port) = if let Some(rest) = endpoint.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or_else(|| invalid("unterminated IPv6 literal".to_string()))?;
        let port =
            match after {
                "" => None,
                after => Some(after.strip_prefix(':').ok_or_else(|| {
                    invalid(format!("unexpected {after:?} after the IPv6 literal"))
                })?),
            };
        (host, port)
    } else if endpoint.matches(':').count() > 1 {
        (endpoint, None)
    } else {
        match endpoint.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (endpoint, None),
        }
    };
    if host.is_empty() {
        return Err(invalid("host is empty".to_string()));
    }
    let port = match port {
        None => DEFAULT_PORT,
        Some(port) => port
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| invalid(format!("invalid port {port:?}")))?,
    };
    Ok((host.to_string(), port))
}
