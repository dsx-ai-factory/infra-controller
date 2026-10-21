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

use std::net::Ipv6Addr;
use std::path::PathBuf;
use std::time::Duration;

use rustls_pki_types::ServerName;

use crate::error::KmipError;

/// The IANA-registered KMIP port, used when `endpoint` names no port.
pub const DEFAULT_PORT: u16 = 5696;

/// Default per-attempt bound on reading the TLS files, the TCP connect and
/// the TLS handshake.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default bound on waiting for the connection, and on writing a request and
/// reading its complete response.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Hard upper bound for `connect_timeout`; a larger value is a configuration
/// error.
pub const MAX_CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

/// Hard upper bound for `request_timeout`; a larger value is a configuration
/// error.
pub const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// How long the cached connection may sit unused before the next call closes
/// it and connects afresh, re-reading the TLS files. This bounds how long a
/// connection that a firewall or NAT has silently dropped can linger, and how
/// long a rotated certificate waits once traffic pauses; a connection in
/// continuous use is kept.
pub const MAX_CONNECTION_IDLE: Duration = Duration::from_secs(30);

/// Where and how to reach one KMIP server.
///
/// [`KmipClient::new`](crate::KmipClient::new) validates every field. The TLS
/// files are read again on every (re)connect, which happens after a failure
/// or once the connection has been idle for [`MAX_CONNECTION_IDLE`], so
/// rotated certificates take effect at the next connection without a restart;
/// a connection in continuous use keeps the identity it was opened with.
#[derive(Clone, Debug)]
pub struct KmipClientConfig {
    /// `host:port`, `host`, `[v6-literal]:port` or `[v6-literal]` of the KMIP
    /// TLS listener. The host must be a DNS name or an IP address; an IPv6
    /// literal must be bracketed even without a port, and only an IPv6
    /// literal may be. Omitting the port selects [`DEFAULT_PORT`].
    pub endpoint: String,

    /// The name the server certificate must match, defaulting to the host
    /// part of `endpoint`. A DNS name is also sent as SNI; an IP address is
    /// matched against the certificate's IP SANs and no SNI is sent.
    pub server_name: Option<String>,

    /// PEM bundle of the CA certificates trusted to sign the server
    /// certificate. System roots are never consulted.
    pub ca_bundle: PathBuf,

    /// PEM certificate chain presented to the server.
    pub client_cert: PathBuf,

    /// PEM private key (PKCS#8, PKCS#1 or SEC1) matching `client_cert`.
    pub client_key: PathBuf,

    /// Per-attempt bound on reading the TLS files, the TCP connect and the
    /// TLS handshake together. Must be non-zero and at most
    /// [`MAX_CONNECT_TIMEOUT`].
    pub connect_timeout: Duration,

    /// Bound on waiting for the client's single connection behind another
    /// caller, and separately on writing one request and reading its complete
    /// response. Must be non-zero and at most [`MAX_REQUEST_TIMEOUT`].
    pub request_timeout: Duration,
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

/// Splits `endpoint` into host and port, requiring the host to be a DNS name
/// or an IP address. An IPv6 literal must be bracketed, with or without a
/// port, so that none of its colons can be read as the port separator.
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
        if host.parse::<Ipv6Addr>().is_err() {
            return Err(invalid(format!(
                "{host:?} in brackets is not an IPv6 literal"
            )));
        }
        let port =
            match after {
                "" => None,
                after => Some(after.strip_prefix(':').ok_or_else(|| {
                    invalid(format!("unexpected {after:?} after the IPv6 literal"))
                })?),
            };
        (host, port)
    } else if endpoint.matches(':').count() > 1 {
        return Err(invalid(
            "host contains more than one colon; an IPv6 literal must be bracketed, as in \
             [2001:db8::1]:5696"
                .to_string(),
        ));
    } else {
        match endpoint.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (endpoint, None),
        }
    };
    if host.is_empty() {
        return Err(invalid("host is empty".to_string()));
    }
    server_name("endpoint", host)?;
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

/// Parses `name` as the DNS name or IP address a server certificate is
/// checked against, naming `field` on failure.
pub(crate) fn server_name(
    field: &'static str,
    name: &str,
) -> Result<ServerName<'static>, KmipError> {
    ServerName::try_from(name)
        .map(|server_name| server_name.to_owned())
        .map_err(|_| KmipError::InvalidConfig {
            field,
            detail: format!("{name:?} is not a valid DNS name or IP address"),
        })
}
