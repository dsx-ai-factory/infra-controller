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

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crate::wire::{Operation, ResultReason, ResultStatus};

/// A problem with the configured CA bundle, client certificate, key or server
/// name. Every variant names the configuration field and file it concerns.
#[derive(Debug, thiserror::Error)]
pub enum TlsMaterialError {
    #[error("failed to read {role} file {}", .path.display())]
    Read {
        role: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{role} file {} is not valid PEM", .path.display())]
    Parse {
        role: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{role} file {} contains no {expected}", .path.display())]
    Empty {
        role: &'static str,
        path: PathBuf,
        expected: &'static str,
    },

    #[error("{role} file {} was rejected", .path.display())]
    Rejected {
        role: &'static str,
        path: PathBuf,
        #[source]
        source: rustls::Error,
    },

    #[error("server name {name:?} is not a valid DNS name or IP address")]
    ServerName { name: String },

    #[error("building the TLS client configuration failed")]
    Builder(#[source] rustls::Error),
}

/// Why a KMIP operation did not produce a result.
///
/// Transport, protocol and configuration failures are distinct from a
/// [`KmipError::Refused`] answer, which carries the server's Result Reason and
/// Result Message so callers can map it (see [`KmipError::is_item_not_found`]).
/// No variant's message includes request or response payload bytes.
#[derive(Debug, thiserror::Error)]
pub enum KmipError {
    /// A configuration value failed validation before any connection was made.
    #[error("kmip {field}: {detail}")]
    InvalidConfig { field: &'static str, detail: String },

    #[error("kmip tls material: {0}")]
    Tls(#[from] TlsMaterialError),

    /// TCP connect or TLS handshake failed within the connect deadline.
    #[error("kmip connect to {endpoint} failed")]
    Connect {
        endpoint: String,
        #[source]
        source: io::Error,
    },

    #[error("kmip connect to {endpoint} timed out after {timeout:?}")]
    ConnectTimeout { endpoint: String, timeout: Duration },

    /// The request deadline expired, either while waiting for the client's
    /// single connection behind another caller, in which case nothing was
    /// sent, or while writing the request and reading its complete response,
    /// in which case the connection is discarded and the outcome on the server
    /// is unknown. Neither case is retried automatically.
    #[error("kmip {operation} timed out after {timeout:?}")]
    RequestTimeout {
        operation: Operation,
        timeout: Duration,
    },

    /// The connection failed while exchanging the request.
    #[error("kmip transport failure during {operation}")]
    Transport {
        operation: Operation,
        #[source]
        source: io::Error,
    },

    /// The server's bytes could not be understood as the expected KMIP
    /// response.
    #[error("kmip protocol violation during {operation}: {detail}")]
    Protocol {
        operation: Operation,
        detail: String,
    },

    /// The response frame announced a length above
    /// [`MAX_RESPONSE_BYTES`](crate::MAX_RESPONSE_BYTES); nothing past the
    /// header was read.
    #[error("kmip response of {length} bytes exceeds the {max} byte limit")]
    ResponseTooLarge { length: u32, max: u32 },

    /// The server answered with a Result Status other than Success.
    #[error("kmip server refused {operation}: {}", refusal_detail(.status, .reason.as_ref(), .message.as_deref()))]
    Refused {
        operation: Operation,
        status: ResultStatus,
        reason: Option<ResultReason>,
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
        detail.push_str(": ");
        detail.push_str(message);
    }
    detail
}
