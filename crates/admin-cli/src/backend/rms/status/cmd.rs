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
use serde::{Serialize, Serializer};

use crate::async_writeln;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

/// Machine-readable outcome of an RMS connectivity probe.
///
/// The string form of each variant ([`StatusToken::as_str`]) is the contract
/// that operators and scripts depend on; it is what both the text and JSON
/// output print.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusToken {
    Connected,
    NotConfigured,
    ApiUnreachable,
    RmsUnreachable,
    CliConfigError,
    AuthFailed,
    AuthOrVersionMismatch,
    ApiVersionMismatch,
    Timeout,
    Error,
}

impl StatusToken {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::NotConfigured => "not-configured",
            Self::ApiUnreachable => "api-unreachable",
            Self::RmsUnreachable => "rms-unreachable",
            Self::CliConfigError => "cli-config-error",
            Self::AuthFailed => "auth-failed",
            Self::AuthOrVersionMismatch => "auth-or-version-mismatch",
            Self::ApiVersionMismatch => "api-version-mismatch",
            Self::Timeout => "timeout",
            Self::Error => "error",
        }
    }
}

impl Serialize for StatusToken {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Outcome of a single RMS connectivity probe.
///
/// `status` is a machine-readable token; `message` is a human-readable
/// sentence; `version` is the RMS version string on success or `None` on any
/// error.
#[derive(Serialize)]
struct Report {
    status: StatusToken,
    message: String,
    version: Option<String>,
}

impl Report {
    fn connected(version: String) -> Self {
        Self {
            status: StatusToken::Connected,
            message: "rms status probe successful".to_owned(),
            version: Some(version),
        }
    }

    fn failure(status: StatusToken, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            version: None,
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
/// - **nico-api → RMS**: The handler prefixes the message of every status
///   that reports a failure of its call to RMS with [`RMS_PREFIX`],
///   whatever the code.  The prefix is checked first, so such a status is
///   never mistaken for a CLI → nico-api failure or for nico-api's own RBAC
///   rejecting the call.  See [`classify_rms_failure`].
/// - **nico-api → RMS (not configured)**: The handler returns a specific
///   `UNAVAILABLE` message when no RMS endpoint is configured.
/// - **CLI → nico-api**: A transport-level `UNAVAILABLE` means nico-api is
///   down or unreachable from this host.  `ForgeTlsClientError` also maps
///   local configuration failures (missing root CA, bad cert, etc.) to
///   `UNAVAILABLE`; those are identified by their message prefix.
///
/// `PermissionDenied` without the RMS prefix is inherently ambiguous: the
/// server's RBAC middleware rejects calls that have no matching rule with
/// HTTP 403 (which tonic maps to `PermissionDenied`) *before* the gRPC layer
/// can return `Unimplemented`.  On older nico-api servers that predate
/// `GetRmsVersion`, the RBAC rule is absent and the result is
/// `PermissionDenied`, not `Unimplemented`.
fn classify(s: tonic::Status) -> Report {
    let msg = s.message().to_owned();

    if let Some(rms_detail) = msg.strip_prefix(RMS_PREFIX) {
        return classify_rms_failure(s.code(), rms_detail);
    }

    match s.code() {
        tonic::Code::Unavailable => {
            if msg.contains("rms is not configured") {
                Report::failure(
                    StatusToken::NotConfigured,
                    "rms is not configured on this nico-api instance \
                     — set the rms endpoint in the nico-api configuration",
                )
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
                Report::failure(
                    StatusToken::ApiUnreachable,
                    format!(
                        "could not connect to nico-api — check that the api server is \
                         running and reachable: {}",
                        if msg.is_empty() {
                            "connection refused or service unavailable"
                        } else {
                            &msg
                        }
                    ),
                )
            } else if msg.contains("configuration error") {
                // ForgeTlsClientError::Configuration (missing CA file, invalid
                // cert, etc.) — the CLI never contacted nico-api.
                Report::failure(
                    StatusToken::CliConfigError,
                    format!("cli configuration problem prevented connecting to nico-api: {msg}"),
                )
            } else {
                // Catch-all: server-generated UNAVAILABLE without an rms: prefix.
                Report::failure(
                    StatusToken::RmsUnreachable,
                    format!("nico-api cannot reach the rms backend: {msg}"),
                )
            }
        }

        // Defensive: nico-api's auth middleware rejects with HTTP 403, and a
        // client certificate it rejects fails at connect time (api-unreachable),
        // so a server-generated Unauthenticated is not expected here.
        tonic::Code::Unauthenticated => Report::failure(
            StatusToken::AuthFailed,
            format!("authentication was rejected by nico-api: {msg}"),
        ),

        tonic::Code::PermissionDenied => {
            // Ambiguous: either a genuine authorisation failure, or the
            // targeted nico-api server predates GetRmsVersion (its RBAC rules
            // reject the call with HTTP 403 before gRPC can return
            // Unimplemented).
            Report::failure(
                StatusToken::AuthOrVersionMismatch,
                format!(
                    "permission denied — either the cli certificate lacks the required \
                     role, or this nico-api server predates the GetRmsVersion rpc and \
                     its rbac rules reject the call before dispatch: {msg}"
                ),
            )
        }

        tonic::Code::DeadlineExceeded => Report::failure(
            StatusToken::Timeout,
            "the request to nico-api timed out before it responded",
        ),

        // Defensive: Unimplemented is not normally reachable on servers with
        // RBAC (which rejects unknown RPCs via HTTP 403 → PermissionDenied
        // before gRPC dispatch), but may surface on deployments without RBAC.
        tonic::Code::Unimplemented => Report::failure(
            StatusToken::ApiVersionMismatch,
            "the targeted nico-api server does not implement GetRmsVersion \
             — it may predate this rpc",
        ),

        code => Report::failure(StatusToken::Error, format!("{code}: {msg}")),
    }
}

/// Classify a status that nico-api forwarded for a failed call to the RMS
/// backend, given the message with [`RMS_PREFIX`] removed.
///
/// Every such failure happened on the nico-api → RMS leg, so the code is
/// interpreted from that leg's point of view: a timeout waiting for RMS is a
/// failure to reach it, and a rejected credential is nico-api's credential at
/// RMS rather than the CLI's at nico-api.
fn classify_rms_failure(code: tonic::Code, rms_detail: &str) -> Report {
    match code {
        tonic::Code::Unavailable => Report::failure(
            StatusToken::RmsUnreachable,
            format!("nico-api cannot reach the rms backend: {rms_detail}"),
        ),
        tonic::Code::DeadlineExceeded => Report::failure(
            StatusToken::RmsUnreachable,
            format!("nico-api timed out waiting for the rms backend: {rms_detail}"),
        ),
        tonic::Code::Unauthenticated | tonic::Code::PermissionDenied => Report::failure(
            StatusToken::AuthFailed,
            format!(
                "the rms backend rejected nico-api's credentials — check the \
                 nico-api→rms mtls certificate and the rms authorization: {rms_detail}"
            ),
        ),
        code => Report::failure(
            StatusToken::Error,
            format!("the rms backend returned {code}: {rms_detail}"),
        ),
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
        async_writeln!(out, "status:  {}", report.status.as_str())?;
        async_writeln!(out, "message: {}", report.message)?;
        async_writeln!(out, "version: {}", report.version.as_deref().unwrap_or("-"))?;
    }
    Ok(())
}

/// Print `report` to `out`, then fail unless it is `connected`.
///
/// The report is written (and flushed by `async_writeln!`) before the error is
/// returned, so it is always visible; the error makes the CLI exit non-zero.
async fn print_report_and_fail_unless_connected(
    report: Report,
    format: OutputFormat,
    out: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    print_report(&report, format, out).await?;

    if report.status == StatusToken::Connected {
        Ok(())
    } else {
        Err(eyre::eyre!("rms status probe failed: {}", report.status.as_str()).into())
    }
}

/// Probe the RMS backend via `GetRmsVersion` and print a connection status
/// report to `out`.
///
/// Returns an error, after printing the report, when the probe does not
/// return `connected`; the CLI then exits with status code `1`.
pub(super) async fn probe(
    api_client: &ApiClient,
    format: OutputFormat,
    out: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    let report = match api_client.0.get_rms_version().await {
        Ok(resp) => Report::connected(resp.version),
        Err(status) => classify(status),
    };

    print_report_and_fail_unless_connected(report, format, out).await
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt as _;

    use super::*;

    // Each case drives classify() with a tonic::Status and asserts the
    // resulting status token.  The message field is not checked here because
    // it is human-readable prose whose wording may evolve; the token is the
    // machine-readable contract that operators and scripts depend on.
    //
    // The rms-prefixed and "rms is not configured" messages below are the ones
    // `get_rms_version` in `crates/api-core/src/handlers/rms.rs` produces; its
    // own tests pin the same strings.
    #[test]
    fn classify_status_tokens() {
        let cases: &[(&str, tonic::Status, StatusToken)] = &[
            // ── nico-api not configured ──────────────────────────────────
            (
                "not-configured: handler sentinel message",
                tonic::Status::unavailable("rms is not configured on this API server"),
                StatusToken::NotConfigured,
            ),
            // ── RMS-side failures (prefixed by handler with "rms: ") ─────
            (
                "rms-unreachable: TlsError forwarded as Unavailable",
                tonic::Status::unavailable("rms: tls error: certificate verify failed"),
                StatusToken::RmsUnreachable,
            ),
            (
                "rms-unreachable: RMS returns connection refused (not api-unreachable)",
                tonic::Status::unavailable("rms: connection refused"),
                StatusToken::RmsUnreachable,
            ),
            (
                "rms-unreachable: RMS returns transport error",
                tonic::Status::unavailable("rms: transport error"),
                StatusToken::RmsUnreachable,
            ),
            (
                "rms-unreachable: handler timeout waiting for RMS (not timeout)",
                tonic::Status::deadline_exceeded("rms: get_version timed out after 30 seconds"),
                StatusToken::RmsUnreachable,
            ),
            (
                "auth-failed: RMS rejects nico-api (not auth-or-version-mismatch)",
                tonic::Status::permission_denied("rms: no role"),
                StatusToken::AuthFailed,
            ),
            (
                "auth-failed: RMS rejects nico-api cert",
                tonic::Status::unauthenticated("rms: certificate verify failed"),
                StatusToken::AuthFailed,
            ),
            (
                "error: RMS does not implement GetVersion (not api-version-mismatch)",
                tonic::Status::unimplemented("rms: GetVersion"),
                StatusToken::Error,
            ),
            (
                "error: RMS internal failure",
                tonic::Status::internal("rms: boom"),
                StatusToken::Error,
            ),
            // ── CLI → nico-api failures ──────────────────────────────────
            (
                "api-unreachable: empty tonic transport message",
                tonic::Status::unavailable(""),
                StatusToken::ApiUnreachable,
            ),
            (
                "api-unreachable: plain connection refused (no rms: prefix)",
                tonic::Status::unavailable("connection refused"),
                StatusToken::ApiUnreachable,
            ),
            (
                "api-unreachable: ForgeTlsClientError::Connection",
                tonic::Status::unavailable("ConnectError error: tcp connect error"),
                StatusToken::ApiUnreachable,
            ),
            (
                "cli-config-error: ForgeTlsClientError::Configuration",
                tonic::Status::unavailable(
                    "configuration error: could not read root CA cert at /bad/path: \
                     no such file or directory",
                ),
                StatusToken::CliConfigError,
            ),
            // ── auth / permission ────────────────────────────────────────
            (
                "auth-failed: Unauthenticated (cert rejected)",
                tonic::Status::unauthenticated("certificate verify failed"),
                StatusToken::AuthFailed,
            ),
            (
                "auth-or-version-mismatch: PermissionDenied (RBAC or role)",
                tonic::Status::permission_denied("no rule permits these principals"),
                StatusToken::AuthOrVersionMismatch,
            ),
            // ── version skew ─────────────────────────────────────────────
            (
                "api-version-mismatch: Unimplemented (non-RBAC old server)",
                tonic::Status::unimplemented("GetRmsVersion"),
                StatusToken::ApiVersionMismatch,
            ),
            // ── timeout ──────────────────────────────────────────────────
            (
                "timeout: DeadlineExceeded not attributed to rms",
                tonic::Status::deadline_exceeded("request timed out"),
                StatusToken::Timeout,
            ),
            // ── generic error ────────────────────────────────────────────
            (
                "error: unexpected Internal code",
                tonic::Status::internal("some unexpected server error"),
                StatusToken::Error,
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

    // The status token is what scripts parse, so pin its serialized form and
    // check that only `connected` carries a version.
    #[tokio::test]
    async fn report_output_and_failure_exit() {
        let cases = [
            (
                "connected, text",
                Report::connected("v1.2.3".to_owned()),
                OutputFormat::AsciiTable,
                "status:  connected\n\
                 message: rms status probe successful\n\
                 version: v1.2.3\n",
                None,
            ),
            (
                "failure, text",
                Report::failure(StatusToken::RmsUnreachable, "nico-api cannot reach rms"),
                OutputFormat::AsciiTable,
                "status:  rms-unreachable\n\
                 message: nico-api cannot reach rms\n\
                 version: -\n",
                Some("rms status probe failed: rms-unreachable"),
            ),
            (
                "failure, json",
                Report::failure(StatusToken::AuthFailed, "rejected"),
                OutputFormat::Json,
                "{\n  \"status\": \"auth-failed\",\n  \"message\": \"rejected\",\n  \"version\": null\n}\n",
                Some("rms status probe failed: auth-failed"),
            ),
        ];

        for (name, report, format, want_output, want_error) in cases {
            let (writer, mut reader) = tokio::io::duplex(4096);
            let mut out: Box<dyn tokio::io::AsyncWrite + Unpin> = Box::new(writer);

            let result = print_report_and_fail_unless_connected(report, format, &mut out).await;
            drop(out);

            let mut output = String::new();
            reader.read_to_string(&mut output).await.unwrap();
            assert_eq!(output, want_output, "{name}: output");
            match (result, want_error) {
                (Ok(()), None) => {}
                (Err(error), Some(want)) => assert_eq!(error.to_string(), want, "{name}: error"),
                (result, want) => panic!("{name}: got {result:?}, want error {want:?}"),
            }
        }
    }
}
