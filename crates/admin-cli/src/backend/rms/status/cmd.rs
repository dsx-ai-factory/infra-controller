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

use ::rpc::admin_cli::OutputFormat;
use serde::Serialize;
use tokio::io::AsyncWriteExt as _;

use crate::async_writeln;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

/// Outcome of a single RMS connectivity probe.
///
/// `status` is a machine-readable token; `message` is a human-readable
/// sentence; `version` is the RMS version string on success or `None` on any
/// error.
#[derive(Serialize)]
struct Report {
    status: &'static str,
    message: String,
    version: Option<String>,
}

impl Report {
    fn connected(version: String) -> Self {
        Self {
            status: "connected",
            message: "rms status probe successful".to_owned(),
            version: Some(version),
        }
    }
}

/// Translate a gRPC [`tonic::Status`] into a user-readable [`Report`].
///
/// The mapping tries to distinguish the three failure legs the operator cares
/// about:
///
/// - **CLI → nico-api**: A transport-level `UNAVAILABLE` means nico-api is
///   down or unreachable from this host.
/// - **nico-api → RMS (not configured)**: The handler returns a specific
///   `UNAVAILABLE` message when no RMS endpoint is configured.
/// - **nico-api → RMS (mTLS rejected)**: `UNAUTHENTICATED` from the server
///   usually means a cert was presented but rejected.
fn classify(s: tonic::Status) -> Report {
    let msg = s.message().to_owned();
    match s.code() {
        tonic::Code::Unavailable => {
            if msg.contains("rms is not configured") {
                const NOT_CFG: &str = "rms is not configured on this nico-api instance \
                    — set the rms endpoint in the nico-api configuration";
                Report {
                    status: "not-configured",
                    message: NOT_CFG.to_owned(),
                    version: None,
                }
            } else if msg.is_empty()
                || msg.contains("transport error")
                || msg.contains("error trying to connect")
                || msg.contains("connection refused")
            {
                // Tonic transport errors surface as UNAVAILABLE with a
                // message that describes the underlying TCP/TLS failure;
                // an empty message is also a transport-layer symptom.
                Report {
                    status: "api-unreachable",
                    message: format!(
                        "could not connect to nico-api — check that the api server is \
                         running and reachable: {}",
                        if msg.is_empty() {
                            "connection refused or service unavailable"
                        } else {
                            &msg
                        }
                    ),
                    version: None,
                }
            } else {
                // UNAVAILABLE with a server-generated message means nico-api
                // reached RMS but the RMS connection failed.
                Report {
                    status: "rms-unreachable",
                    message: format!("nico-api cannot reach the rms backend: {msg}"),
                    version: None,
                }
            }
        }

        tonic::Code::Unauthenticated => Report {
            status: "auth-failed",
            message: format!(
                "authentication was rejected — check the mtls certificate on the \
                 cli→nico-api or nico-api→rms path: {msg}"
            ),
            version: None,
        },

        tonic::Code::PermissionDenied => Report {
            status: "auth-failed",
            message: format!("permission denied: {msg}"),
            version: None,
        },

        tonic::Code::DeadlineExceeded => Report {
            status: "timeout",
            message: "the connection attempt timed out before rms responded".to_owned(),
            version: None,
        },

        code => Report {
            status: "error",
            message: format!("{code}: {msg}"),
            version: None,
        },
    }
}

async fn print_report(
    report: &Report,
    format: OutputFormat,
    out: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    if format == OutputFormat::Json {
        async_writeln!(out, "{}", serde_json::to_string_pretty(report)?)?;
    } else {
        async_writeln!(out, "status:  {}", report.status)?;
        async_writeln!(out, "message: {}", report.message)?;
        async_writeln!(out, "version: {}", report.version.as_deref().unwrap_or("-"))?;
    }
    Ok(())
}

/// Probe the RMS backend via `GetRmsVersion` and print a connection status
/// report to `out`.
///
/// Exits with status code `1` when the probe does not return `connected`,
/// after flushing `out` so the status report is always visible.
pub(super) async fn probe(
    api_client: &ApiClient,
    format: OutputFormat,
    out: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    let report = match api_client.0.get_rms_version().await {
        Ok(resp) => Report::connected(resp.version),
        Err(status) => classify(status),
    };

    let is_error = report.status != "connected";
    print_report(&report, format, out).await?;

    if is_error {
        out.flush().await?;
        std::process::exit(1);
    }

    Ok(())
}
