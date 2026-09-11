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

/// Prefix added by `get_rms_version` to every forwarded RMS error message.
///
/// Using a structural prefix rather than substring heuristics lets
/// [`classify`] unambiguously tell apart "RMS said connection refused" from
/// "the CLI's transport layer said connection refused."
const RMS_PREFIX: &str = "rms: ";

/// Translate a gRPC [`tonic::Status`] into a user-readable [`Report`].
///
/// The mapping tries to distinguish the three failure legs the operator cares
/// about:
///
/// - **CLI → nico-api**: A transport-level `UNAVAILABLE` means nico-api is
///   down or unreachable from this host.  `ForgeTlsClientError` also maps
///   local configuration failures (missing root CA, bad cert, etc.) to
///   `UNAVAILABLE`; those are identified by their message prefix.
/// - **nico-api → RMS (not configured)**: The handler returns a specific
///   `UNAVAILABLE` message when no RMS endpoint is configured.
/// - **nico-api → RMS (connection/TLS failure)**: The handler prefixes every
///   forwarded RMS error with [`RMS_PREFIX`] so the CLI can route it to
///   `rms-unreachable` without substring-matching on transport words that
///   also appear in CLI-side errors.
///
/// `PermissionDenied` is inherently ambiguous: the server's RBAC middleware
/// rejects calls that have no matching rule with HTTP 403 (which tonic maps to
/// `PermissionDenied`) *before* the gRPC layer can return `Unimplemented`.
/// On older nico-api servers that predate `GetRmsVersion`, the RBAC rule is
/// absent and the result is `PermissionDenied`, not `Unimplemented`.
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
            } else if let Some(rms_detail) = msg.strip_prefix(RMS_PREFIX) {
                // Error forwarded from the RMS backend by our handler.
                // Always an nico-api → RMS failure regardless of what the
                // underlying transport message says (e.g. "connection refused"
                // here means RMS is down, not that nico-api is unreachable).
                Report {
                    status: "rms-unreachable",
                    message: format!("nico-api cannot reach the rms backend: {rms_detail}"),
                    version: None,
                }
            } else if msg.is_empty()
                || msg.contains("transport error")
                || msg.contains("error trying to connect")
                || msg.contains("connection refused")
                // ForgeTlsClientError::Connection surfaces as "ConnectError error: …"
                || msg.contains("ConnectError")
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
            } else if msg.contains("configuration error") {
                // ForgeTlsClientError::Configuration (missing CA file, invalid
                // cert, etc.) — the CLI never contacted nico-api.
                Report {
                    status: "cli-config-error",
                    message: format!(
                        "cli configuration problem prevented connecting to nico-api: {msg}"
                    ),
                    version: None,
                }
            } else {
                // Catch-all: server-generated UNAVAILABLE without an rms: prefix.
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

        tonic::Code::PermissionDenied => {
            // Ambiguous: either a genuine authorisation failure, or the
            // targeted nico-api server predates GetRmsVersion (its RBAC rules
            // reject the call with HTTP 403 before gRPC can return
            // Unimplemented).
            Report {
                status: "auth-or-version-mismatch",
                message: format!(
                    "permission denied — either the cli certificate lacks the required \
                     role, or this nico-api server predates the GetRmsVersion rpc and \
                     its rbac rules reject the call before dispatch: {msg}"
                ),
                version: None,
            }
        }

        tonic::Code::DeadlineExceeded => Report {
            status: "timeout",
            message: "the connection attempt timed out before rms responded".to_owned(),
            version: None,
        },

        // Defensive: Unimplemented is not normally reachable on servers with
        // RBAC (which rejects unknown RPCs via HTTP 403 → PermissionDenied
        // before gRPC dispatch), but may surface on deployments without RBAC.
        tonic::Code::Unimplemented => Report {
            status: "api-version-mismatch",
            message: "the targeted nico-api server does not implement GetRmsVersion \
                — it may predate this rpc"
                .to_owned(),
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

#[cfg(test)]
mod tests {
    use super::*;

    // Each case drives classify() with a tonic::Status and asserts the
    // resulting status token.  The message field is not checked here because
    // it is human-readable prose whose wording may evolve; the token is the
    // machine-readable contract that operators and scripts depend on.
    #[test]
    fn classify_status_tokens() {
        let cases: &[(&str, tonic::Status, &str)] = &[
            // ── nico-api not configured ──────────────────────────────────
            (
                "not-configured: handler sentinel message",
                tonic::Status::unavailable("rms is not configured on this API server"),
                "not-configured",
            ),
            // ── RMS-side failures (prefixed by handler with "rms: ") ─────
            (
                "rms-unreachable: TlsError forwarded as Unavailable",
                tonic::Status::unavailable("rms: tls error: certificate verify failed"),
                "rms-unreachable",
            ),
            (
                "rms-unreachable: RMS returns connection refused (not api-unreachable)",
                tonic::Status::unavailable("rms: connection refused"),
                "rms-unreachable",
            ),
            (
                "rms-unreachable: RMS returns transport error",
                tonic::Status::unavailable("rms: transport error"),
                "rms-unreachable",
            ),
            // ── CLI → nico-api failures ──────────────────────────────────
            (
                "api-unreachable: empty tonic transport message",
                tonic::Status::unavailable(""),
                "api-unreachable",
            ),
            (
                "api-unreachable: plain connection refused (no rms: prefix)",
                tonic::Status::unavailable("connection refused"),
                "api-unreachable",
            ),
            (
                "api-unreachable: ForgeTlsClientError::Connection",
                tonic::Status::unavailable("ConnectError error: tcp connect error"),
                "api-unreachable",
            ),
            (
                "cli-config-error: ForgeTlsClientError::Configuration",
                tonic::Status::unavailable(
                    "configuration error: could not read root CA cert at /bad/path: \
                     no such file or directory",
                ),
                "cli-config-error",
            ),
            // ── auth / permission ────────────────────────────────────────
            (
                "auth-failed: Unauthenticated (cert rejected)",
                tonic::Status::unauthenticated("certificate verify failed"),
                "auth-failed",
            ),
            (
                "auth-or-version-mismatch: PermissionDenied (RBAC or role)",
                tonic::Status::permission_denied("no rule permits these principals"),
                "auth-or-version-mismatch",
            ),
            // ── version skew ─────────────────────────────────────────────
            (
                "api-version-mismatch: Unimplemented (non-RBAC old server)",
                tonic::Status::unimplemented("GetRmsVersion"),
                "api-version-mismatch",
            ),
            // ── timeout ──────────────────────────────────────────────────
            (
                "timeout: DeadlineExceeded",
                tonic::Status::deadline_exceeded("rms get_version timed out after 30 seconds"),
                "timeout",
            ),
            // ── generic error ────────────────────────────────────────────
            (
                "error: unexpected Internal code",
                tonic::Status::internal("some unexpected server error"),
                "error",
            ),
        ];

        for (name, status, want_token) in cases {
            let report = classify(status.clone());
            assert_eq!(
                report.status, *want_token,
                "classify({name:?}): got status {:?}, want {want_token:?}",
                report.status
            );
        }
    }
}
