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

//! Error types. Messages are lowercase fragments without trailing periods so
//! they compose when wrapped by a caller.

use std::path::PathBuf;
use std::time::Duration;
use std::{fmt, io};

use crate::wire::{Operation, ResultReason, ResultStatus};

/// Longest Result Message, in characters, that a [`KmipError::Refused`]
/// message renders; the server's full text stays in the variant.
const MAX_RENDERED_RESULT_MESSAGE: usize = 256;

/// One of the three PEM files a [`KmipClientConfig`](crate::KmipClientConfig)
/// names. `Display` is the configuration field's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TlsFile {
    /// `ca_bundle`: the CA certificates trusted to sign the server
    /// certificate.
    CaBundle,
    /// `client_cert`: the certificate chain presented to the server.
    ClientCert,
    /// `client_key`: the private key matching `client_cert`.
    ClientKey,
}

impl fmt::Display for TlsFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CaBundle => "ca_bundle",
            Self::ClientCert => "client_cert",
            Self::ClientKey => "client_key",
        })
    }
}

/// A problem with the configured CA bundle, client certificate or key. Every
/// variant but [`Builder`](Self::Builder) names the file it concerns.
#[derive(Debug, thiserror::Error)]
pub enum TlsMaterialError {
    /// The file could not be read.
    #[error("failed to read {file} file {}", .path.display())]
    Read {
        /// Which configured file.
        file: TlsFile,
        /// Its configured path.
        path: PathBuf,
        /// The read failure.
        #[source]
        source: io::Error,
    },

    /// The file is not valid PEM.
    #[error("{file} file {} is not valid PEM", .path.display())]
    Parse {
        /// Which configured file.
        file: TlsFile,
        /// Its configured path.
        path: PathBuf,
        /// The parse failure.
        #[source]
        source: io::Error,
    },

    /// The file is valid PEM but holds none of what it must.
    #[error("{file} file {} contains no {expected}", .path.display())]
    Empty {
        /// Which configured file.
        file: TlsFile,
        /// Its configured path.
        path: PathBuf,
        /// What the file must contain: `certificates` or `private key`.
        expected: &'static str,
    },

    /// The file parsed but rustls rejected its contents, for example a CA
    /// bundle entry that is not a CA certificate or a client certificate that
    /// does not match the key.
    #[error("{file} file {} was rejected", .path.display())]
    Rejected {
        /// Which configured file.
        file: TlsFile,
        /// Its configured path.
        path: PathBuf,
        /// The rejection.
        #[source]
        source: rustls::Error,
    },

    /// The rustls client configuration could not be built from otherwise
    /// valid material.
    #[error("building the TLS client configuration failed")]
    Builder(#[source] rustls::Error),
}

/// Which wait a [`KmipError::RequestTimeout`] expired in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeoutPhase {
    /// Waiting for the client's single connection behind another caller;
    /// nothing was sent.
    Queued,
    /// Writing the request or reading its response; whether the server
    /// applied the request is unknown, and the connection is discarded.
    InFlight,
}

impl fmt::Display for TimeoutPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Queued => "waiting for the connection; nothing was sent",
            Self::InFlight => {
                "waiting for the response; whether the server applied the request is unknown"
            }
        })
    }
}

/// Why a KMIP operation did not produce a result.
///
/// Transport, protocol and configuration failures are distinct from a
/// [`KmipError::Refused`] answer, which carries the server's Result Reason and
/// Result Message so callers can map it (see [`KmipError::is_item_not_found`]).
/// No variant's message includes request or response payload bytes: the
/// codec's errors name offsets, tags and types only, and a server's Result
/// Message is rendered quoted and truncated.
#[derive(Debug, thiserror::Error)]
pub enum KmipError {
    /// A configuration value failed validation before any connection was made.
    #[error("kmip {field}: {detail}")]
    InvalidConfig {
        /// The configuration field at fault.
        field: &'static str,
        /// What is wrong with it.
        detail: String,
    },

    /// The TLS material on disk could not be loaded, at construction or on a
    /// reconnect.
    #[error(transparent)]
    Tls(#[from] TlsMaterialError),

    /// The TCP connect or TLS handshake failed within the connect deadline.
    /// Under TLS 1.3 a server rejects an untrusted client certificate only
    /// after the handshake, so that case surfaces as
    /// [`Transport`](Self::Transport) on the first exchange instead.
    #[error("kmip connect to {endpoint} failed")]
    Connect {
        /// The configured endpoint.
        endpoint: String,
        /// The connect or handshake failure.
        #[source]
        source: io::Error,
    },

    /// Reading the TLS files, connecting and completing the TLS handshake took
    /// longer than the connect deadline.
    #[error("kmip connect to {endpoint} timed out after {timeout:?}")]
    ConnectTimeout {
        /// The configured endpoint.
        endpoint: String,
        /// The configured connect deadline.
        timeout: Duration,
    },

    /// The request deadline expired; `phase` tells whether anything was sent.
    /// Neither phase is retried automatically.
    #[error("kmip {operation} timed out after {timeout:?} {phase}")]
    RequestTimeout {
        /// The operation attempted.
        operation: Operation,
        /// The configured request deadline.
        timeout: Duration,
        /// Which wait expired.
        phase: TimeoutPhase,
    },

    /// The connection failed while exchanging the request, and is discarded.
    #[error("kmip transport failure during {operation}")]
    Transport {
        /// The operation attempted.
        operation: Operation,
        /// The connection failure.
        #[source]
        source: io::Error,
    },

    /// The request could not be encoded, or the server's bytes could not be
    /// understood as the answer to it: a malformed frame; a batch count,
    /// operation or Unique Identifier that does not match the request;
    /// another request's Unique Batch Item ID, or none on a successful
    /// answer; a successful answer in a protocol version other than 1.4 to
    /// anything but Discover Versions; a critical Message Extension; or an
    /// Encrypt result without a usable IV, tag and ciphertext. The connection
    /// is discarded.
    #[error("kmip protocol violation during {operation}: {detail}")]
    Protocol {
        /// The operation attempted.
        operation: Operation,
        /// Which check failed.
        detail: String,
        /// The codec's error, when the bytes did not decode at all.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    },

    /// The response header announced a message of `length` bytes, header
    /// included, above `max`
    /// ([`MAX_RESPONSE_BYTES`](crate::MAX_RESPONSE_BYTES)); nothing past the
    /// header was read and the connection is discarded.
    #[error("kmip response of {length} bytes exceeds the {max} byte limit")]
    ResponseTooLarge {
        /// The announced message size.
        length: u32,
        /// The limit it exceeded.
        max: u32,
    },

    /// The server answered with a Result Status other than Success. The
    /// connection stays usable.
    #[error("kmip server refused {operation}: {}", refusal_detail(.status, .reason.as_ref(), .message.as_deref()))]
    Refused {
        /// The operation attempted.
        operation: Operation,
        /// The server's Result Status.
        status: ResultStatus,
        /// The server's Result Reason, when it sent one.
        reason: Option<ResultReason>,
        /// The server's Result Message, verbatim, when it sent one.
        message: Option<String>,
    },
}

impl KmipError {
    /// Whether the server refused the operation because the named object does
    /// not exist (Result Reason `ItemNotFound`).
    pub fn is_item_not_found(&self) -> bool {
        matches!(
            self,
            Self::Refused {
                reason: Some(ResultReason::ItemNotFound),
                ..
            }
        )
    }

    /// The server's Result Reason, when the error is a refusal that carried
    /// one.
    pub fn result_reason(&self) -> Option<ResultReason> {
        match self {
            Self::Refused { reason, .. } => *reason,
            _ => None,
        }
    }
}

fn refusal_detail(
    status: &ResultStatus,
    reason: Option<&ResultReason>,
    message: Option<&str>,
) -> String {
    let mut detail = match reason {
        Some(reason) => format!("{reason} ({status})"),
        None => status.to_string(),
    };
    if let Some(message) = message.filter(|message| !message.is_empty()) {
        // The server's text is untrusted: quote it and keep it short.
        let mut rendered: String = message.chars().take(MAX_RENDERED_RESULT_MESSAGE).collect();
        if message.chars().count() > MAX_RENDERED_RESULT_MESSAGE {
            rendered.push_str("...");
        }
        detail.push_str(&format!(": {rendered:?}"));
    }
    detail
}
