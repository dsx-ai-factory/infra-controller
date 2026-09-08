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

//! The bmc-proxy metrics endpoint and the proxy's instrumentation events.
//! Authorization events keep policy denials separate from missing middleware
//! context, while each outbound BMC request records its duration and status.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use carbide_instrument::{Event, LabelValue, MetricFamily};
use http::Method;
use metrics_endpoint::{MetricsEndpointConfig, MetricsSetup};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub(crate) async fn start(
    address: SocketAddr,
    metrics_setup: MetricsSetup,
    cancellation_token: CancellationToken,
    join_set: &mut JoinSet<()>,
) -> io::Result<()> {
    let listener = crate::net::bind_with_ipv4_fallback(address).await?;
    tracing::info!(metrics_address = %address, "Starting metrics listener");

    join_set
        .build_task()
        .name("bmc-proxy metrics service")
        .spawn(async move {
            if let Err(e) = metrics_endpoint::run_metrics_endpoint_with_listener(
                &MetricsEndpointConfig {
                    address,
                    registry: metrics_setup.registry,
                    health_controller: Some(metrics_setup.health_controller),
                    additional_prefix: None,
                },
                cancellation_token,
                listener,
            )
            .await
            {
                tracing::error!(error = %e, "metrics endpoint exited with error");
            }
        })
        // Safety: Should only fail if not in a tokio runtime
        .expect("Error spawning metrics endpoint");

    Ok(())
}

/// The HTTP method of a proxied request, as a bounded metric label: the
/// methods Redfish traffic uses, with anything else bucketed as `other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum MethodLabel {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
    Other,
}

impl From<&Method> for MethodLabel {
    fn from(method: &Method) -> Self {
        match *method {
            Method::GET => Self::Get,
            Method::HEAD => Self::Head,
            Method::POST => Self::Post,
            Method::PUT => Self::Put,
            Method::PATCH => Self::Patch,
            Method::DELETE => Self::Delete,
            _ => Self::Other,
        }
    }
}

/// The authorization boundary that rejected a request or could not evaluate
/// it. The outer allow-list decides which principals may use the proxy at all;
/// the request ACL then decides which Redfish method and path they may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum AuthorizationLayer {
    PrincipalAllowList,
    RequestAcl,
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_bmc_proxy_authorization_denied_total",
    kind = counter,
    component = "nico-bmc-proxy",
    describe = "Number of BMC proxy requests denied by authorization layer and HTTP method"
)]
pub(crate) struct BmcProxyAuthorizationDenied {
    authorization_layer: AuthorizationLayer,
    #[label(name = "method")]
    method_label: MethodLabel,
}

/// The request reached the per-principal ACL, but no configured rule allowed
/// its method and path. `method_label` bounds the metric while `method` keeps
/// the existing uppercase log field operators already search.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_request_acl_denied",
    metric_family = BmcProxyAuthorizationDenied,
    log = info,
    message = "Request denied by BMC proxy ACLs"
)]
pub(crate) struct RequestAclDenied {
    #[label]
    authorization_layer: AuthorizationLayer,
    #[label]
    method_label: MethodLabel,
    #[context]
    principals: String,
    #[context]
    path: String,
    #[context]
    method: String,
}

impl RequestAclDenied {
    pub(crate) fn new(method: &Method, principals: String, path: String) -> Self {
        Self {
            authorization_layer: AuthorizationLayer::RequestAcl,
            method_label: method.into(),
            principals,
            path,
            method: method.as_str().to_string(),
        }
    }
}

/// The outer principal allow-list rejected a request before it reached the
/// per-principal ACL. Principal identities and configured policy stay on the
/// log line; neither can create a metric series.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_principal_allow_list_denied",
    metric_family = BmcProxyAuthorizationDenied,
    log = info,
    message = "Request denied by BMC proxy principal allow-list"
)]
pub(crate) struct PrincipalAllowListDenied {
    #[label]
    authorization_layer: AuthorizationLayer,
    #[label]
    method_label: MethodLabel,
    #[context]
    allowed_principals: String,
    #[context]
    present_principals: String,
    #[context]
    path: String,
}

impl PrincipalAllowListDenied {
    pub(crate) fn new(
        method: &Method,
        allowed_principals: String,
        present_principals: String,
        path: String,
    ) -> Self {
        Self {
            authorization_layer: AuthorizationLayer::PrincipalAllowList,
            method_label: method.into(),
            allowed_principals,
            present_principals,
            path,
        }
    }
}

/// An authorization layer could not run because authentication middleware did
/// not attach an `AuthContext`. A wiring error, not a policy denial, so it is
/// counted apart from a normal 403. Each variant keeps the level its layer had.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_auth_context_missing",
    metric_name = "carbide_bmc_proxy_authorization_errors_total",
    component = "nico-bmc-proxy",
    metric = counter,
    describe = "Number of BMC proxy authorization errors caused by missing authentication context, by authorization layer and HTTP method",
    labels(authorization_layer: AuthorizationLayer, method: MethodLabel),
)]
pub(crate) enum AuthContextMissing {
    #[event(
        labels(authorization_layer = AuthorizationLayer::RequestAcl),
        log = error,
        message = "BUG: No AuthContext middleware found, all requests will be denied"
    )]
    RequestAcl {
        #[label(name = "method")]
        method_label: MethodLabel,
    },

    #[event(
        labels(authorization_layer = AuthorizationLayer::PrincipalAllowList),
        log = warn,
        message = "authorize_proxy_request found a request with no AuthContext in its extensions"
    )]
    PrincipalAllowList {
        #[label(name = "method")]
        method_label: MethodLabel,
    },
}
/// How the BMC answered a proxied request, as a bounded metric label: the
/// response's HTTP status class, or `error` when the forward produced no
/// response at all (a connect failure, timeout, or TLS failure on the
/// upstream leg). A status outside the standard 2xx-5xx classes -- which no
/// real BMC sends as a final response -- also counts as `error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum UpstreamStatus {
    Http2xx,
    Http3xx,
    Http4xx,
    Http5xx,
    Error,
}

impl From<reqwest::StatusCode> for UpstreamStatus {
    fn from(status: reqwest::StatusCode) -> Self {
        match status.as_u16() / 100 {
            2 => Self::Http2xx,
            3 => Self::Http3xx,
            4 => Self::Http4xx,
            5 => Self::Http5xx,
            _ => Self::Error,
        }
    }
}

impl UpstreamStatus {
    /// The class of a completed forward: the response's status class, or
    /// `Error` when the request never produced a response.
    pub(crate) fn from_result<E>(result: &Result<reqwest::Response, E>) -> Self {
        match result {
            Ok(response) => response.status().into(),
            Err(_) => Self::Error,
        }
    }
}

/// A request the proxy forwarded to a BMC completed, successfully or not.
/// The duration covers the upstream leg through the response headers;
/// response bodies stream back separately. One send may follow up to five
/// redirects internally, so the status is the final hop's -- `http3xx`
/// generally means a non-followed 3xx such as a 304. Metric-only: the
/// forward has never logged per request in either direction, and a
/// failure's detail already reaches the caller in the 502 response body.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_upstream_request_completed",
    metric_name = "carbide_bmc_proxy_upstream_request_duration_milliseconds",
    component = "nico-bmc-proxy",
    log = off,
    metric = histogram,
    describe = "Duration of requests the proxy forwarded to BMCs, by HTTP method and upstream status class; the _count series, split by status, gives the request and outcome rates."
)]
pub(crate) struct UpstreamRequestCompleted {
    #[label]
    pub(crate) method: MethodLabel,
    #[label]
    pub(crate) status: UpstreamStatus,
    #[observation]
    pub(crate) took: Duration,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use carbide_instrument::emit;
    use carbide_instrument::testing::{ApproxHistogramSum, MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values, value_scenarios};
    use opentelemetry::StringValue;
    use tokio::time::timeout;

    use super::*;

    const AUTHORIZATION_DENIED_METRIC: &str = "carbide_bmc_proxy_authorization_denied_total";
    const AUTHORIZATION_ERROR_METRIC: &str = "carbide_bmc_proxy_authorization_errors_total";

    struct AuthorizationEventInput {
        authorization_layer: &'static str,
        method: &'static str,
        emit: fn(),
    }

    #[derive(Debug, PartialEq)]
    struct AuthorizationEventObservation {
        denied_delta: f64,
        error_delta: f64,
        log: AuthorizationEventLog,
    }

    #[derive(Debug, PartialEq)]
    struct AuthorizationEventLog {
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        authorization_layer: Option<String>,
        method_label: Option<String>,
        principals: Option<String>,
        path: Option<String>,
        method: Option<String>,
        allowed_principals: Option<String>,
        present_principals: Option<String>,
    }

    fn emit_request_acl_denied() {
        emit(RequestAclDenied::new(
            &Method::PATCH,
            r#"["spiffe-service-id/nico-api", "anonymous"]"#.to_string(),
            "/redfish/v1/Systems/1".to_string(),
        ));
    }

    fn emit_principal_allow_list_denied() {
        emit(PrincipalAllowListDenied::new(
            &Method::GET,
            r#"{"spiffe-service-id/nico-api"}"#.to_string(),
            r#"["trusted-certificate", "anonymous"]"#.to_string(),
            "/redfish/v1".to_string(),
        ));
    }

    fn emit_request_acl_auth_context_missing() {
        emit(AuthContextMissing::RequestAcl {
            method_label: (&Method::DELETE).into(),
        });
    }

    fn emit_principal_allow_list_auth_context_missing() {
        emit(AuthContextMissing::PrincipalAllowList {
            method_label: (&Method::OPTIONS).into(),
        });
    }

    fn observe_authorization_event(
        input: AuthorizationEventInput,
    ) -> AuthorizationEventObservation {
        let metrics = MetricsCapture::start();
        let mut logs = capture_logs(input.emit);
        assert_eq!(logs.len(), 1, "an authorization Event logs exactly once");
        let log = logs.pop().expect("the authorization Event log");
        let field = |name: &str| log.field(name).map(str::to_owned);
        let labels = [
            ("authorization_layer", input.authorization_layer),
            ("method", input.method),
        ];

        AuthorizationEventObservation {
            denied_delta: metrics.counter_delta(AUTHORIZATION_DENIED_METRIC, &labels),
            error_delta: metrics.counter_delta(AUTHORIZATION_ERROR_METRIC, &labels),
            log: AuthorizationEventLog {
                level: log.level,
                metadata_name: log.metadata_name.clone(),
                message: log.message.clone(),
                event_name: field("event_name"),
                metric_name: field("metric_name"),
                authorization_layer: field("authorization_layer"),
                method_label: field("method_label"),
                principals: field("principals"),
                path: field("path"),
                method: field("method"),
                allowed_principals: field("allowed_principals"),
                present_principals: field("present_principals"),
            },
        }
    }

    fn expected_authorization_event(
        level: tracing::Level,
        event_name: &str,
        metric_name: &str,
        message: &str,
        authorization_layer: &str,
        method_label: &str,
    ) -> AuthorizationEventObservation {
        AuthorizationEventObservation {
            denied_delta: if metric_name == AUTHORIZATION_DENIED_METRIC {
                1.0
            } else {
                0.0
            },
            error_delta: if metric_name == AUTHORIZATION_ERROR_METRIC {
                1.0
            } else {
                0.0
            },
            log: AuthorizationEventLog {
                level,
                metadata_name: event_name.to_string(),
                message: message.to_string(),
                event_name: Some(event_name.to_string()),
                metric_name: Some(metric_name.to_string()),
                authorization_layer: Some(authorization_layer.to_string()),
                method_label: Some(method_label.to_string()),
                principals: None,
                path: None,
                method: None,
                allowed_principals: None,
                present_principals: None,
            },
        }
    }

    #[tokio::test]
    async fn start_binds_listener_and_spawns_endpoint_task() {
        let address = "127.0.0.1:0".parse().expect("valid listen address");
        let metrics_setup =
            metrics_endpoint::new_metrics_setup("carbide-bmc-proxy-test", "test", false)
                .expect("metrics setup succeeds");
        let cancellation_token = CancellationToken::new();
        let mut join_set = JoinSet::new();

        start(
            address,
            metrics_setup,
            cancellation_token.clone(),
            &mut join_set,
        )
        .await
        .expect("metrics endpoint starts");

        assert_eq!(join_set.len(), 1);

        cancellation_token.cancel();
        timeout(Duration::from_secs(5), join_set.join_next())
            .await
            .expect("metrics endpoint exits after cancellation")
            .expect("metrics endpoint task is joined")
            .expect("metrics endpoint task succeeds");
    }

    #[test]
    fn method_label_maps_the_proxied_set_and_buckets_the_rest() {
        check_values(
            [
                Check {
                    scenario: "GET",
                    input: Method::GET,
                    expect: MethodLabel::Get,
                },
                Check {
                    scenario: "HEAD",
                    input: Method::HEAD,
                    expect: MethodLabel::Head,
                },
                Check {
                    scenario: "POST",
                    input: Method::POST,
                    expect: MethodLabel::Post,
                },
                Check {
                    scenario: "PUT",
                    input: Method::PUT,
                    expect: MethodLabel::Put,
                },
                Check {
                    scenario: "PATCH",
                    input: Method::PATCH,
                    expect: MethodLabel::Patch,
                },
                Check {
                    scenario: "DELETE",
                    input: Method::DELETE,
                    expect: MethodLabel::Delete,
                },
                Check {
                    scenario: "OPTIONS buckets as other",
                    input: Method::OPTIONS,
                    expect: MethodLabel::Other,
                },
                Check {
                    scenario: "extension method buckets as other",
                    input: Method::from_bytes(b"PROPFIND").expect("valid method token"),
                    expect: MethodLabel::Other,
                },
            ],
            |method| MethodLabel::from(&method),
        );
    }

    #[test]
    fn upstream_status_classes_map_from_status_codes() {
        check_values(
            [
                Check {
                    scenario: "200 OK",
                    input: reqwest::StatusCode::OK,
                    expect: UpstreamStatus::Http2xx,
                },
                Check {
                    scenario: "204 No Content",
                    input: reqwest::StatusCode::NO_CONTENT,
                    expect: UpstreamStatus::Http2xx,
                },
                Check {
                    scenario: "304 Not Modified",
                    input: reqwest::StatusCode::NOT_MODIFIED,
                    expect: UpstreamStatus::Http3xx,
                },
                Check {
                    scenario: "401 Unauthorized",
                    input: reqwest::StatusCode::UNAUTHORIZED,
                    expect: UpstreamStatus::Http4xx,
                },
                Check {
                    scenario: "404 Not Found",
                    input: reqwest::StatusCode::NOT_FOUND,
                    expect: UpstreamStatus::Http4xx,
                },
                Check {
                    scenario: "500 Internal Server Error",
                    input: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                    expect: UpstreamStatus::Http5xx,
                },
                Check {
                    scenario: "503 Service Unavailable",
                    input: reqwest::StatusCode::SERVICE_UNAVAILABLE,
                    expect: UpstreamStatus::Http5xx,
                },
                Check {
                    scenario: "interim 1xx class buckets as error",
                    input: reqwest::StatusCode::CONTINUE,
                    expect: UpstreamStatus::Error,
                },
                Check {
                    scenario: "non-standard 6xx class buckets as error",
                    input: reqwest::StatusCode::from_u16(600).expect("in the valid range"),
                    expect: UpstreamStatus::Error,
                },
            ],
            UpstreamStatus::from,
        );
    }

    #[test]
    fn upstream_status_from_result_uses_the_response_or_error() {
        let response = reqwest::Response::from(
            http::Response::builder()
                .status(http::StatusCode::BAD_GATEWAY)
                .body("")
                .expect("response builds"),
        );
        assert_eq!(
            UpstreamStatus::from_result::<reqwest_middleware::Error>(&Ok(response)),
            UpstreamStatus::Http5xx
        );

        let error = reqwest_middleware::Error::from(
            reqwest::Client::new()
                .get("http://")
                .build()
                .expect_err("an empty host cannot build a request"),
        );
        assert_eq!(
            UpstreamStatus::from_result(&Err(error)),
            UpstreamStatus::Error
        );
    }

    /// The label vocabulary is the dashboard contract: each variant renders
    /// as its snake_case name, byte for byte.
    #[test]
    fn label_values_render_as_snake_case() {
        value_scenarios!(
            run = |value: StringValue| value.to_string();
            "authorization layer" {
                AuthorizationLayer::PrincipalAllowList.label_value() => "principal_allow_list".to_string(),
                AuthorizationLayer::RequestAcl.label_value() => "request_acl".to_string(),
            }

            "proxied method" {
                MethodLabel::Get.label_value() => "get".to_string(),
                MethodLabel::Head.label_value() => "head".to_string(),
                MethodLabel::Post.label_value() => "post".to_string(),
                MethodLabel::Put.label_value() => "put".to_string(),
                MethodLabel::Patch.label_value() => "patch".to_string(),
                MethodLabel::Delete.label_value() => "delete".to_string(),
                MethodLabel::Other.label_value() => "other".to_string(),
            }

            "upstream status class" {
                UpstreamStatus::Http2xx.label_value() => "http2xx".to_string(),
                UpstreamStatus::Http3xx.label_value() => "http3xx".to_string(),
                UpstreamStatus::Http4xx.label_value() => "http4xx".to_string(),
                UpstreamStatus::Http5xx.label_value() => "http5xx".to_string(),
                UpstreamStatus::Error.label_value() => "error".to_string(),
            }
        );
    }

    /// Policy denials and missing middleware context both keep their historical
    /// diagnostics, but they move different metric families. The shared labels
    /// let an operator identify the boundary and method without putting any
    /// principal or path into a time series.
    #[test]
    fn authorization_events_preserve_logs_and_separate_denials_from_errors() {
        let mut request_acl_denied = expected_authorization_event(
            tracing::Level::INFO,
            "bmc_proxy_request_acl_denied",
            AUTHORIZATION_DENIED_METRIC,
            "Request denied by BMC proxy ACLs",
            "request_acl",
            "patch",
        );
        request_acl_denied.log.principals =
            Some(r#"["spiffe-service-id/nico-api", "anonymous"]"#.to_string());
        request_acl_denied.log.path = Some("/redfish/v1/Systems/1".to_string());
        request_acl_denied.log.method = Some("PATCH".to_string());

        let mut principal_allow_list_denied = expected_authorization_event(
            tracing::Level::INFO,
            "bmc_proxy_principal_allow_list_denied",
            AUTHORIZATION_DENIED_METRIC,
            "Request denied by BMC proxy principal allow-list",
            "principal_allow_list",
            "get",
        );
        principal_allow_list_denied.log.allowed_principals =
            Some(r#"{"spiffe-service-id/nico-api"}"#.to_string());
        principal_allow_list_denied.log.present_principals =
            Some(r#"["trusted-certificate", "anonymous"]"#.to_string());
        principal_allow_list_denied.log.path = Some("/redfish/v1".to_string());

        check_values(
            [
                Check {
                    scenario: "request ACL denial",
                    input: AuthorizationEventInput {
                        authorization_layer: "request_acl",
                        method: "patch",
                        emit: emit_request_acl_denied,
                    },
                    expect: request_acl_denied,
                },
                Check {
                    scenario: "principal allow-list denial",
                    input: AuthorizationEventInput {
                        authorization_layer: "principal_allow_list",
                        method: "get",
                        emit: emit_principal_allow_list_denied,
                    },
                    expect: principal_allow_list_denied,
                },
                Check {
                    scenario: "request ACL missing authentication context",
                    input: AuthorizationEventInput {
                        authorization_layer: "request_acl",
                        method: "delete",
                        emit: emit_request_acl_auth_context_missing,
                    },
                    expect: expected_authorization_event(
                        tracing::Level::ERROR,
                        "bmc_proxy_auth_context_missing",
                        AUTHORIZATION_ERROR_METRIC,
                        "BUG: No AuthContext middleware found, all requests will be denied",
                        "request_acl",
                        "delete",
                    ),
                },
                Check {
                    scenario: "principal allow-list missing authentication context",
                    input: AuthorizationEventInput {
                        authorization_layer: "principal_allow_list",
                        method: "other",
                        emit: emit_principal_allow_list_auth_context_missing,
                    },
                    expect: expected_authorization_event(
                        tracing::Level::WARN,
                        "bmc_proxy_auth_context_missing",
                        AUTHORIZATION_ERROR_METRIC,
                        "authorize_proxy_request found a request with no AuthContext in its extensions",
                        "principal_allow_list",
                        "other",
                    ),
                },
            ],
            observe_authorization_event,
        );
    }

    /// Each forward moves exactly its label pair's series, records the
    /// duration in the milliseconds the metric name declares, and builds no
    /// log line -- the forward boundary has never logged per request.
    #[test]
    fn upstream_request_events_record_per_label_without_logging() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(UpstreamRequestCompleted {
                method: MethodLabel::Patch,
                status: UpstreamStatus::Http5xx,
                took: Duration::from_millis(1500),
            });
            emit(UpstreamRequestCompleted {
                method: MethodLabel::Patch,
                status: UpstreamStatus::Http5xx,
                took: Duration::from_millis(500),
            });
            emit(UpstreamRequestCompleted {
                method: MethodLabel::Delete,
                status: UpstreamStatus::Error,
                took: Duration::from_millis(250),
            });
        });

        assert!(
            logs.is_empty(),
            "log = off must not construct any log line, got {logs:?}"
        );
        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_bmc_proxy_upstream_request_duration_milliseconds",
                &[("method", "patch"), ("status", "http5xx")],
            ),
            2
        );
        let sum = metrics.histogram_sum_delta(
            "carbide_bmc_proxy_upstream_request_duration_milliseconds",
            &[("method", "patch"), ("status", "http5xx")],
        );
        assert_eq!(
            sum,
            ApproxHistogramSum(2000.0),
            "1500ms + 500ms record as milliseconds"
        );
        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_bmc_proxy_upstream_request_duration_milliseconds",
                &[("method", "delete"), ("status", "error")],
            ),
            1
        );
        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_bmc_proxy_upstream_request_duration_milliseconds",
                &[("method", "get"), ("status", "http2xx")],
            ),
            0,
            "an untouched label pair must not move",
        );
    }
}
