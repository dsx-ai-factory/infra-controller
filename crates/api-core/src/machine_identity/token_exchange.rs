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

//! RFC 8693 token exchange HTTP client for tenant `token_endpoint` (machine identity delegation).

use std::time::Duration;

use ::rpc::forge::MachineIdentityResponse;
use base64::Engine;
use carbide_instrument::{Event, LabelValue, MetricFamily, emit};
use carbide_utils::none_if_empty::NoneIfEmpty;
use serde::Deserialize;
use tonic::Status;

use crate::CarbideError;

const OAUTH_GRANT_TYPE_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const OAUTH_TOKEN_TYPE_JWT: &str = "urn:ietf:params:oauth:token-type:jwt";
const TOKEN_EXCHANGE_RESPONSE_SCHEMA_ERROR: &str =
    "token exchange response did not match the expected schema";

/// Where an RFC 8693 exchange stopped, as the bounded `failure_stage`
/// label shared by the failure Events below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum TokenExchangeFailureStage {
    Request,
    ResponseBody,
    HttpStatus,
    ResponseJson,
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_machine_identity_token_exchange_failures_total",
    kind = counter,
    component = "nico-api",
    describe = "Number of machine identity token exchange failures, by failure stage"
)]
struct MachineIdentityTokenExchangeFailures {
    failure_stage: TokenExchangeFailureStage,
}

/// The HTTP request did not reach a token-exchange response.
#[derive(Event)]
#[event(
    event_name = "machine_identity_token_exchange_request_failed",
    metric_family = MachineIdentityTokenExchangeFailures,
    log = error,
    message = "token exchange HTTP request failed"
)]
struct TokenExchangeRequestFailed {
    #[label]
    failure_stage: TokenExchangeFailureStage,
    #[context]
    error: String,
    #[context]
    token_endpoint: String,
}

/// The token endpoint answered, but its response body could not be read.
#[derive(Event)]
#[event(
    event_name = "machine_identity_token_exchange_response_read_failed",
    metric_family = MachineIdentityTokenExchangeFailures,
    log = error,
    message = "token exchange response body read failed"
)]
struct TokenExchangeResponseReadFailed {
    #[label]
    failure_stage: TokenExchangeFailureStage,
    #[context]
    error: String,
    #[context]
    token_endpoint: String,
}

/// The token endpoint returned a non-success HTTP status.
/// Response content stays out of diagnostics because OAuth error payloads can
/// include credentials or token-shaped values.
#[derive(Event)]
#[event(
    event_name = "machine_identity_token_exchange_endpoint_rejected",
    metric_family = MachineIdentityTokenExchangeFailures,
    log = warn,
    message = "token exchange endpoint returned error"
)]
struct TokenExchangeEndpointRejected {
    #[label]
    failure_stage: TokenExchangeFailureStage,
    #[context]
    http_status: String,
    #[context]
    response_body_length: usize,
    #[context]
    token_endpoint: String,
}

/// A successful HTTP response did not contain a valid token-exchange body.
/// Neither the body nor the parser error is logged: serde errors can quote a
/// response value, and a partially valid response can still contain a token.
#[derive(Event)]
#[event(
    event_name = "machine_identity_token_exchange_response_invalid",
    metric_family = MachineIdentityTokenExchangeFailures,
    log = warn,
    message = "token exchange JSON parse failed"
)]
struct TokenExchangeResponseInvalid {
    #[label]
    failure_stage: TokenExchangeFailureStage,
    #[context]
    error: String,
    #[context]
    response_body_length: usize,
    #[context]
    token_endpoint: String,
}

#[derive(Debug, Deserialize)]
struct TokenExchangeHttpResponseBody {
    access_token: String,
    #[serde(default)]
    issued_token_type: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    /// RFC 6749 `expires_in` (seconds). JSON must be a non-negative integer in `u32` range.
    #[serde(default)]
    expires_in: Option<u32>,
}

/// Builds the HTTP client used only for RFC 8693 calls to per-org `token_endpoint`.
/// When `token_endpoint_http_proxy` is set and non-empty, all those requests go through that proxy.
pub(crate) fn token_exchange_http_client(
    token_endpoint_http_proxy: Option<&str>,
) -> Result<reqwest_middleware::ClientWithMiddleware, Status> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none());
    if let Some(proxy_url) = token_endpoint_http_proxy.none_if_empty() {
        let proxy = reqwest::Proxy::all(proxy_url).map_err(|e| {
            CarbideError::InvalidArgument(format!(
                "invalid machine_identity.token_endpoint_http_proxy: {e}"
            ))
        })?;
        builder = builder.proxy(proxy);
    }
    let client = builder
        .build()
        .map_err(|e| CarbideError::internal(format!("token exchange HTTP client: {e}")))?;
    // The `reqwest-tracing` middleware injects the current span's W3C trace context into every
    // outgoing request (#2438).
    Ok(reqwest_middleware::ClientBuilder::new(client)
        .with(reqwest_tracing::TracingMiddleware::default())
        .build())
}

pub(crate) fn rfc8693_token_exchange_form(
    subject_jwt: &str,
    workload_audiences: &[String],
) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    ser.append_pair("grant_type", OAUTH_GRANT_TYPE_TOKEN_EXCHANGE);
    ser.append_pair("subject_token", subject_jwt);
    ser.append_pair("subject_token_type", OAUTH_TOKEN_TYPE_JWT);
    for a in workload_audiences {
        ser.append_pair("audience", a);
    }
    ser.finish()
}

/// Sends an [RFC 8693](https://datatracker.ietf.org/doc/html/rfc8693) token exchange **request** to
/// the tenant `token_endpoint` (HTTP POST, `application/x-www-form-urlencoded` body from
/// [`rfc8693_token_exchange_form`]) and maps the JSON response to [`MachineIdentityResponse`].
pub(crate) async fn token_exchange_request(
    http: &reqwest_middleware::ClientWithMiddleware,
    token_endpoint: &str,
    subject_jwt: &str,
    workload_audiences: &[String],
    basic_credentials: Option<&(String, String)>,
) -> Result<MachineIdentityResponse, Status> {
    let body = rfc8693_token_exchange_form(subject_jwt, workload_audiences);

    let mut req = http
        .post(token_endpoint)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body);

    if let Some((client_id, client_secret)) = basic_credentials {
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(format!("{client_id}:{client_secret}"));
        req = req.header(reqwest::header::AUTHORIZATION, format!("Basic {encoded}"));
    }

    let resp = req.send().await.map_err(|e| {
        emit(TokenExchangeRequestFailed {
            failure_stage: TokenExchangeFailureStage::Request,
            error: e.to_string(),
            token_endpoint: token_endpoint.to_string(),
        });
        CarbideError::internal(format!("token exchange request failed: {e}"))
    })?;

    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| {
        emit(TokenExchangeResponseReadFailed {
            failure_stage: TokenExchangeFailureStage::ResponseBody,
            error: e.to_string(),
            token_endpoint: token_endpoint.to_string(),
        });
        CarbideError::internal(format!("token exchange response failed: {e}"))
    })?;

    if !status.is_success() {
        emit(TokenExchangeEndpointRejected {
            failure_stage: TokenExchangeFailureStage::HttpStatus,
            http_status: status.to_string(),
            response_body_length: bytes.len(),
            token_endpoint: token_endpoint.to_string(),
        });
        return Err(CarbideError::InvalidArgument(format!(
            "token exchange endpoint returned HTTP {status}"
        ))
        .into());
    }

    let parsed: TokenExchangeHttpResponseBody = serde_json::from_slice(&bytes).map_err(|_| {
        emit(TokenExchangeResponseInvalid {
            failure_stage: TokenExchangeFailureStage::ResponseJson,
            error: TOKEN_EXCHANGE_RESPONSE_SCHEMA_ERROR.to_string(),
            response_body_length: bytes.len(),
            token_endpoint: token_endpoint.to_string(),
        });
        CarbideError::internal("token exchange response was not valid JSON".to_string())
    })?;

    let issued = parsed
        .issued_token_type
        .unwrap_or_else(|| "urn:ietf:params:oauth:token-type:jwt".to_string());
    let token_type = parsed.token_type.unwrap_or_else(|| "Bearer".to_string());
    let expires_in_sec = parsed.expires_in.unwrap_or(0);

    Ok(MachineIdentityResponse {
        access_token: parsed.access_token,
        issued_token_type: issued,
        token_type,
        expires_in_sec,
    })
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{CapturedLog, MetricsCapture, capture_logs};
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, Check, check_cases_async, check_values};
    use tokio::io::AsyncWriteExt as _;

    use super::*;

    const TOKEN_EXCHANGE_FAILURE_METRIC: &str =
        "carbide_machine_identity_token_exchange_failures_total";

    #[test]
    fn token_exchange_body_deserializes_expires_in_as_json_number() {
        let n: TokenExchangeHttpResponseBody =
            serde_json::from_str(r#"{"access_token":"t","expires_in":3600}"#).unwrap();
        assert_eq!(n.expires_in, Some(3600_u32));
        let omitted: TokenExchangeHttpResponseBody =
            serde_json::from_str(r#"{"access_token":"t"}"#).unwrap();
        assert_eq!(omitted.expires_in, None);
    }

    #[test]
    fn token_exchange_body_rejects_expires_in_as_string() {
        let err = serde_json::from_str::<TokenExchangeHttpResponseBody>(
            r#"{"access_token":"t","expires_in":"7200"}"#,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("expires_in") || err.to_string().contains("invalid type"),
            "{err}"
        );
    }

    #[test]
    fn token_exchange_body_rejects_negative_expires_in() {
        assert!(
            serde_json::from_str::<TokenExchangeHttpResponseBody>(
                r#"{"access_token":"t","expires_in":-1}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn rfc8693_token_exchange_form_encoding() {
        let form = rfc8693_token_exchange_form(
            "header.payload.sig",
            &["spiffe://z/a".to_string(), "spiffe://z/b".to_string()],
        );
        assert!(
            form.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange")
        );
        assert!(form.contains("subject_token=header.payload.sig"));
        assert!(form.contains("subject_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Ajwt"));
        assert!(form.contains("audience=spiffe%3A%2F%2Fz%2Fa"));
        assert!(form.contains("audience=spiffe%3A%2F%2Fz%2Fb"));
    }

    #[tokio::test]
    async fn token_exchange_request_maps_endpoint_response() {
        // What the mocked `token_endpoint` returns for one request.
        struct Reply {
            status: usize,
            body: &'static str,
        }

        // The four token fields we expect a 2xx response to parse into.
        #[derive(Debug, PartialEq)]
        struct Exchanged {
            access_token: String,
            issued_token_type: String,
            token_type: String,
            expires_in_sec: u32,
        }

        check_cases_async(
            [
                Case {
                    scenario: "2xx JSON parses into the response fields",
                    input: Reply {
                        status: 200,
                        body: r#"{"access_token":"exchanged","issued_token_type":"urn:ietf:params:oauth:token-type:jwt","token_type":"Bearer","expires_in":42}"#,
                    },
                    expect: Yields(Exchanged {
                        access_token: "exchanged".to_string(),
                        issued_token_type: "urn:ietf:params:oauth:token-type:jwt".to_string(),
                        token_type: "Bearer".to_string(),
                        expires_in_sec: 42,
                    }),
                },
                Case {
                    scenario: "omitted optional fields use RFC defaults",
                    input: Reply {
                        status: 200,
                        body: r#"{"access_token":"defaulted"}"#,
                    },
                    expect: Yields(Exchanged {
                        access_token: "defaulted".to_string(),
                        issued_token_type: "urn:ietf:params:oauth:token-type:jwt".to_string(),
                        token_type: "Bearer".to_string(),
                        expires_in_sec: 0,
                    }),
                },
            ],
            |reply| async move {
                let mut server = mockito::Server::new_async().await;
                let _m = server
                    .mock("POST", "/token")
                    .match_header("content-type", "application/x-www-form-urlencoded")
                    .with_status(reply.status)
                    .with_header("content-type", "application/json")
                    .with_body(reply.body)
                    .create_async()
                    .await;

                let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
                    .with(reqwest_tracing::TracingMiddleware::default())
                    .build();
                let url = format!("{}/token", server.url());
                token_exchange_request(
                    &client,
                    &url,
                    "sub.jwt",
                    &["spiffe://workload".to_string()],
                    None,
                )
                .await
                .map(|out| Exchanged {
                    access_token: out.access_token,
                    issued_token_type: out.issued_token_type,
                    token_type: out.token_type,
                    expires_in_sec: out.expires_in_sec,
                })
                .map_err(drop)
            },
        )
        .await;
    }

    #[derive(Debug, PartialEq)]
    struct FailureLog {
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: String,
        metric_name: String,
        error_present: bool,
        http_status: Option<String>,
        response_body_length: Option<String>,
        token_endpoint_present: bool,
    }

    fn observe_failure_log(logs: &[CapturedLog], failure_stage: &str) -> FailureLog {
        let log = logs
            .iter()
            .find(|log| log.field("failure_stage") == Some(failure_stage))
            .unwrap_or_else(|| panic!("missing {failure_stage} failure log in {logs:?}"));

        FailureLog {
            level: log.level,
            metadata_name: log.metadata_name.clone(),
            message: log.message.clone(),
            event_name: log
                .field("event_name")
                .expect("Event log has event_name")
                .to_string(),
            metric_name: log
                .field("metric_name")
                .expect("metric-backed Event log has metric_name")
                .to_string(),
            error_present: log.field("error").is_some_and(|error| !error.is_empty()),
            http_status: log.field("http_status").map(str::to_owned),
            response_body_length: log.field("response_body_length").map(str::to_owned),
            token_endpoint_present: log
                .field("token_endpoint")
                .is_some_and(|endpoint| !endpoint.is_empty()),
        }
    }

    #[test]
    fn token_exchange_failures_emit_metrics_and_structured_logs() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should build");
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            runtime.block_on(async {
                let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
                    .with(reqwest_tracing::TracingMiddleware::default())
                    .build();

                token_exchange_request(
                    &client,
                    "://invalid-token-endpoint",
                    "subject.jwt",
                    &[],
                    None,
                )
                .await
                .expect_err("an invalid URL must fail before receiving a response");

                let mut server = mockito::Server::new_async().await;
                let _rejected = server
                    .mock("POST", "/rejected")
                    .with_status(401)
                    .with_body(r#"{"error":"invalid_client"}"#)
                    .create_async()
                    .await;
                let _invalid_json = server
                    .mock("POST", "/invalid-json")
                    .with_status(200)
                    .with_body(r#"{"expires_in":"response-secret"}"#)
                    .create_async()
                    .await;

                for path in ["rejected", "invalid-json"] {
                    token_exchange_request(
                        &client,
                        &format!("{}/{path}", server.url()),
                        "subject.jwt",
                        &[],
                        None,
                    )
                    .await
                    .expect_err("the mocked response must fail token exchange");
                }

                // Send a complete response header and then close before the declared body
                // length. `reqwest` accepts the response, but `Response::bytes` sees the
                // incomplete body and exercises the response-read failure boundary.
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("raw HTTP listener should bind");
                let endpoint = format!(
                    "http://{}/token",
                    listener
                        .local_addr()
                        .expect("raw HTTP listener should have an address")
                );
                let responder = tokio::spawn(async move {
                    let (mut stream, _) = listener.accept().await.expect("request should connect");
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\nshort",
                        )
                        .await
                        .expect("partial response should write");
                    stream.shutdown().await.expect("response should close");
                });
                token_exchange_request(&client, &endpoint, "subject.jwt", &[], None)
                    .await
                    .expect_err("an incomplete response body must fail token exchange");
                responder.await.expect("raw HTTP responder should finish");
            });
        });

        let event_logs = logs
            .into_iter()
            .filter(|log| log.field("metric_name") == Some(TOKEN_EXCHANGE_FAILURE_METRIC))
            .collect::<Vec<_>>();
        assert_eq!(event_logs.len(), 4, "one Event per failure stage");

        check_values(
            [
                Check {
                    scenario: "request send failure",
                    input: "request",
                    expect: FailureLog {
                        level: tracing::Level::ERROR,
                        metadata_name: "machine_identity_token_exchange_request_failed".to_string(),
                        message: "token exchange HTTP request failed".to_string(),
                        event_name: "machine_identity_token_exchange_request_failed".to_string(),
                        metric_name: TOKEN_EXCHANGE_FAILURE_METRIC.to_string(),
                        error_present: true,
                        http_status: None,
                        response_body_length: None,
                        token_endpoint_present: true,
                    },
                },
                Check {
                    scenario: "response body read failure",
                    input: "response_body",
                    expect: FailureLog {
                        level: tracing::Level::ERROR,
                        metadata_name: "machine_identity_token_exchange_response_read_failed"
                            .to_string(),
                        message: "token exchange response body read failed".to_string(),
                        event_name: "machine_identity_token_exchange_response_read_failed"
                            .to_string(),
                        metric_name: TOKEN_EXCHANGE_FAILURE_METRIC.to_string(),
                        error_present: true,
                        http_status: None,
                        response_body_length: None,
                        token_endpoint_present: true,
                    },
                },
                Check {
                    scenario: "endpoint HTTP rejection",
                    input: "http_status",
                    expect: FailureLog {
                        level: tracing::Level::WARN,
                        metadata_name: "machine_identity_token_exchange_endpoint_rejected"
                            .to_string(),
                        message: "token exchange endpoint returned error".to_string(),
                        event_name: "machine_identity_token_exchange_endpoint_rejected".to_string(),
                        metric_name: TOKEN_EXCHANGE_FAILURE_METRIC.to_string(),
                        error_present: false,
                        http_status: Some("401 Unauthorized".to_string()),
                        response_body_length: Some("26".to_string()),
                        token_endpoint_present: true,
                    },
                },
                Check {
                    scenario: "invalid response JSON",
                    input: "response_json",
                    expect: FailureLog {
                        level: tracing::Level::WARN,
                        metadata_name: "machine_identity_token_exchange_response_invalid"
                            .to_string(),
                        message: "token exchange JSON parse failed".to_string(),
                        event_name: "machine_identity_token_exchange_response_invalid".to_string(),
                        metric_name: TOKEN_EXCHANGE_FAILURE_METRIC.to_string(),
                        error_present: true,
                        http_status: None,
                        response_body_length: Some("32".to_string()),
                        token_endpoint_present: true,
                    },
                },
            ],
            |failure_stage| observe_failure_log(&event_logs, failure_stage),
        );

        for failure_stage in ["request", "response_body", "http_status", "response_json"] {
            assert_eq!(
                metrics.counter_delta(
                    TOKEN_EXCHANGE_FAILURE_METRIC,
                    &[("failure_stage", failure_stage)],
                ),
                1.0,
                "failure stage {failure_stage}",
            );
        }

        let response_json_log = event_logs
            .iter()
            .find(|log| log.field("failure_stage") == Some("response_json"))
            .expect("response JSON failure log must exist");
        assert_eq!(
            response_json_log.field("error"),
            Some(TOKEN_EXCHANGE_RESPONSE_SCHEMA_ERROR),
            "response-derived values must not reach the parser diagnostic",
        );

        for log in &event_logs {
            for sensitive_field in [
                "subject_jwt",
                "authorization",
                "access_token",
                "body",
                "body_prefix",
            ] {
                assert_eq!(
                    log.field(sensitive_field),
                    None,
                    "{sensitive_field} must never reach token-exchange diagnostics",
                );
            }
        }
    }

    #[tokio::test]
    async fn token_exchange_request_sends_basic_auth() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/token")
            .match_header("authorization", "Basic Zm9vOmJhcg==")
            .with_status(200)
            .with_body(r#"{"access_token":"t"}"#)
            .create_async()
            .await;

        let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
            .with(reqwest_tracing::TracingMiddleware::default())
            .build();
        let url = format!("{}/token", server.url());
        let creds = ("foo".to_string(), "bar".to_string());
        let out = token_exchange_request(&client, &url, "j", &[], Some(&creds))
            .await
            .unwrap();
        assert_eq!(out.access_token, "t");
    }
}
