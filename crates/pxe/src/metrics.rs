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
use std::time::Duration;

use carbide_instrument::{Event, LabelValue, MetricFamily};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use opentelemetry::StringValue;
use tokio::time::sleep;

const TIME_BUCKETS: &[f64; 11] = &[
    0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0,
];

const SIZE_BUCKETS: &[f64; 9] = &[
    100.0,
    1000.0,
    10000.0,
    100000.0,
    1000000.0,
    10000000.0,
    100000000.0,
    1000000000.0,
    10000000000.0,
];

pub(crate) fn setup_prometheus() -> PrometheusHandle {
    let prometheus_builder = PrometheusBuilder::new()
        .add_global_label("system", "carbide-pxe")
        .add_global_label("build_version", carbide_version::v!(build_version))
        .add_global_label("build_date", carbide_version::v!(build_date))
        .add_global_label("rust_version", carbide_version::v!(rust_version))
        .add_global_label("build_hostname", carbide_version::v!(build_hostname))
        .set_buckets_for_metric(
            Matcher::Suffix("duration_seconds".to_string()),
            TIME_BUCKETS,
        )
        .expect("couldn't set prometheus buckets?")
        .set_buckets_for_metric(Matcher::Suffix("size_bytes".to_string()), SIZE_BUCKETS)
        .expect("couldn't set prometheus buckets?");

    let prometheus_handle = prometheus_builder
        .install_recorder()
        .expect("unable to install recorder?");

    let handle_clone = prometheus_handle.clone();
    tokio::spawn(async move {
        sleep(Duration::from_secs(5)).await;
        handle_clone.run_upkeep();
    });

    prometheus_handle
}

/// Which consumer a cloud-init instruction is intended for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloudInitConsumer {
    Dpu,
    Tenant,
    Scout,
}

/// The boot-path endpoint an outcome describes, as a bounded metric label.
/// Cloud-init values are scoped to the consumer rather than the document, so
/// every document a consumer fetches shares one label value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BootEndpoint {
    Whoami,
    Boot,
    CloudInit(CloudInitConsumer),
}

impl LabelValue for BootEndpoint {
    fn label_value(&self) -> StringValue {
        StringValue::from(match self {
            Self::Whoami => "whoami",
            Self::Boot => "boot",
            Self::CloudInit(CloudInitConsumer::Dpu) => "cloud_init_dpu",
            Self::CloudInit(CloudInitConsumer::Tenant) => "cloud_init_tenant",
            Self::CloudInit(CloudInitConsumer::Scout) => "cloud_init_scout",
        })
    }
}

/// How a boot-path request resolved, as a bounded metric label. Every
/// non-`Ok` variant is a response the machine receives as an error script
/// or generic error template over HTTP 200 -- this label is what makes
/// those outcomes visible, since the status-code metrics cannot see them.
/// Requests rejected before a handler runs (a malformed `buildarch`, an
/// upstream failure inside the `Machine` extractor) return real 4xx codes
/// the `http_*` metrics already count; only `architecture_not_found` is
/// also emitted from its extractor, because a bad architecture is a boot
/// outcome operators watch for. `upstream_api_error` is therefore
/// structurally boot-only. `ok` means the request resolved to a servable
/// response; a template that later fails to render returns a real 5xx the
/// `http_*` metrics count, which is outside this metric's HTTP-200 scope.
///
/// Two of these are non-error outcomes on the Scout routes:
/// `instructions_empty` means no snippets are configured, the supported
/// default, and `snippet_directory_unreadable` means configured snippets were
/// silently not applied -- the only one worth alerting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum OutcomeReason {
    Ok,
    ArchitectureNotFound,
    InterfaceNotFound,
    InstructionsEmpty,
    InstructionsInvalid,
    MetadataNotFound,
    SnippetDirectoryUnreadable,
    UpstreamApiError,
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_pxe_boot_outcomes_total",
    kind = counter,
    component = "carbide-pxe",
    describe = "Number of PXE boot-path outcomes served, by endpoint and reason."
)]
pub(crate) struct PxeBootOutcomes {
    endpoint: BootEndpoint,
    reason: OutcomeReason,
}

/// `PxeBootOutcome` records a boot-path result without writing a log. The
/// quiet success and extractor paths use this type; failures that already
/// have an `ERROR` record use the sibling Events below so one `emit()` writes
/// the log and increments the counter.
#[derive(Event)]
#[event(
    event_name = "pxe_boot_outcome",
    metric_family = PxeBootOutcomes,
    log = off
)]
pub(crate) struct PxeBootOutcome {
    #[label]
    pub(super) endpoint: BootEndpoint,
    #[label]
    pub(super) reason: OutcomeReason,
}

// Both failure Events write the same counter as `PxeBootOutcome`. Keep the
// metric kind, description, and label keys identical so OpenTelemetry sees
// one instrument while each route keeps its existing message.

/// `PxeCloudInitRequestFailed` records a cloud-init request that fell back to
/// the generic error template.
#[derive(Event)]
#[event(
    event_name = "pxe_cloud_init_request_failed",
    metric_family = PxeBootOutcomes,
    log = error,
    message = "cloud-init request could not be served"
)]
pub(crate) struct PxeCloudInitRequestFailed {
    #[label]
    pub(super) endpoint: BootEndpoint,
    #[label]
    pub(super) reason: OutcomeReason,
    #[context]
    pub(super) error: String,
}

/// `PxeCustomIpxeFetchFailed` records a custom iPXE lookup that fell back to
/// the error script returned to the machine.
#[derive(Event)]
#[event(
    event_name = "pxe_custom_ipxe_fetch_failed",
    metric_family = PxeBootOutcomes,
    log = error,
    message = "failed to fetch custom ipxe script"
)]
pub(crate) struct PxeCustomIpxeFetchFailed {
    #[label]
    pub(super) endpoint: BootEndpoint,
    #[label]
    pub(super) reason: OutcomeReason,
    #[context]
    pub(super) error: String,
}

/// A Scout `user-data` request that could not list the snippet directory. The
/// request is still answered with the no-snippets document, so this Event is
/// the only signal that configured snippets did not reach the machine.
#[derive(Event)]
#[event(
    event_name = "pxe_snippet_directory_unreadable",
    metric_family = PxeBootOutcomes,
    log = error,
    message = "discovery cloud-init snippet directory could not be read"
)]
pub(crate) struct PxeSnippetDirectoryUnreadable {
    #[label]
    pub(super) endpoint: BootEndpoint,
    #[label]
    pub(super) reason: OutcomeReason,
    #[context]
    pub(super) error: String,
}

#[cfg(test)]
mod tests {
    use carbide_instrument::emit;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};

    use super::*;

    const BOOT_OUTCOMES_METRIC: &str = "carbide_pxe_boot_outcomes_total";

    /// The label vocabulary is the dashboard contract: each variant renders
    /// as its snake_case name, byte for byte.
    #[test]
    fn label_values_render_as_snake_case() {
        check_values(
            [
                Check {
                    scenario: "whoami endpoint",
                    input: BootEndpoint::Whoami.label_value(),
                    expect: "whoami".to_string(),
                },
                Check {
                    scenario: "boot endpoint",
                    input: BootEndpoint::Boot.label_value(),
                    expect: "boot".to_string(),
                },
                Check {
                    scenario: "DPU cloud-init endpoint",
                    input: BootEndpoint::CloudInit(CloudInitConsumer::Dpu).label_value(),
                    expect: "cloud_init_dpu".to_string(),
                },
                Check {
                    scenario: "tenant cloud-init endpoint",
                    input: BootEndpoint::CloudInit(CloudInitConsumer::Tenant).label_value(),
                    expect: "cloud_init_tenant".to_string(),
                },
                Check {
                    scenario: "Scout cloud-init endpoint",
                    input: BootEndpoint::CloudInit(CloudInitConsumer::Scout).label_value(),
                    expect: "cloud_init_scout".to_string(),
                },
                Check {
                    scenario: "ok",
                    input: OutcomeReason::Ok.label_value(),
                    expect: "ok".to_string(),
                },
                Check {
                    scenario: "architecture not found",
                    input: OutcomeReason::ArchitectureNotFound.label_value(),
                    expect: "architecture_not_found".to_string(),
                },
                Check {
                    scenario: "interface not found",
                    input: OutcomeReason::InterfaceNotFound.label_value(),
                    expect: "interface_not_found".to_string(),
                },
                Check {
                    scenario: "instructions empty",
                    input: OutcomeReason::InstructionsEmpty.label_value(),
                    expect: "instructions_empty".to_string(),
                },
                Check {
                    scenario: "instructions invalid",
                    input: OutcomeReason::InstructionsInvalid.label_value(),
                    expect: "instructions_invalid".to_string(),
                },
                Check {
                    scenario: "upstream API error",
                    input: OutcomeReason::UpstreamApiError.label_value(),
                    expect: "upstream_api_error".to_string(),
                },
                Check {
                    scenario: "render failure",
                    input: OutcomeReason::MetadataNotFound.label_value(),
                    expect: "metadata_not_found".to_string(),
                },
                Check {
                    scenario: "snippet directory unreadable",
                    input: OutcomeReason::SnippetDirectoryUnreadable.label_value(),
                    expect: "snippet_directory_unreadable".to_string(),
                },
            ],
            |value| value.to_string(),
        );
    }

    /// Each emit moves exactly its label pair's series. Even under the test
    /// subscriber, `log = off` constructs no record for these quiet paths.
    #[test]
    fn boot_outcomes_count_per_label_without_logging() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(PxeBootOutcome {
                endpoint: BootEndpoint::Whoami,
                reason: OutcomeReason::Ok,
            });
            emit(PxeBootOutcome {
                endpoint: BootEndpoint::Boot,
                reason: OutcomeReason::UpstreamApiError,
            });
            emit(PxeBootOutcome {
                endpoint: BootEndpoint::Boot,
                reason: OutcomeReason::UpstreamApiError,
            });
            emit(PxeBootOutcome {
                endpoint: BootEndpoint::CloudInit(CloudInitConsumer::Tenant),
                reason: OutcomeReason::MetadataNotFound,
            });
        });

        assert!(
            logs.is_empty(),
            "log = off must not construct any log line, got {logs:?}"
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_pxe_boot_outcomes_total",
                &[("endpoint", "whoami"), ("reason", "ok")],
            ),
            1.0,
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_pxe_boot_outcomes_total",
                &[("endpoint", "boot"), ("reason", "upstream_api_error")],
            ),
            2.0,
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_pxe_boot_outcomes_total",
                &[
                    ("endpoint", "cloud_init_tenant"),
                    ("reason", "metadata_not_found")
                ],
            ),
            1.0,
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_pxe_boot_outcomes_total",
                &[("endpoint", "boot"), ("reason", "ok")],
            ),
            0.0,
            "an untouched label pair must not move",
        );
    }

    #[derive(Clone, Copy)]
    enum FailureEvent {
        CloudInit,
        CustomIpxe,
    }

    #[derive(Debug, PartialEq)]
    struct FailureRecord {
        metadata_name: String,
        level: tracing::Level,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        endpoint: Option<String>,
        reason: Option<String>,
        error: Option<String>,
        counter_delta: f64,
    }

    /// Each failure Event keeps its route's `ERROR` record and increments the
    /// existing label pair once. This is the contract that lets the route
    /// replace its separate `tracing::error!` and `PxeBootOutcome` calls.
    #[test]
    fn boot_failures_log_and_count_once() {
        check_values(
            [
                Check {
                    scenario: "cloud-init generic error",
                    input: FailureEvent::CloudInit,
                    expect: FailureRecord {
                        metadata_name: "pxe_cloud_init_request_failed".to_string(),
                        level: tracing::Level::ERROR,
                        message: "cloud-init request could not be served".to_string(),
                        event_name: Some("pxe_cloud_init_request_failed".to_string()),
                        metric_name: Some(BOOT_OUTCOMES_METRIC.to_string()),
                        endpoint: Some("cloud_init_tenant".to_string()),
                        reason: Some("metadata_not_found".to_string()),
                        error: Some("metadata is missing".to_string()),
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "custom iPXE lookup error",
                    input: FailureEvent::CustomIpxe,
                    expect: FailureRecord {
                        metadata_name: "pxe_custom_ipxe_fetch_failed".to_string(),
                        level: tracing::Level::ERROR,
                        message: "failed to fetch custom ipxe script".to_string(),
                        event_name: Some("pxe_custom_ipxe_fetch_failed".to_string()),
                        metric_name: Some(BOOT_OUTCOMES_METRIC.to_string()),
                        endpoint: Some("boot".to_string()),
                        reason: Some("upstream_api_error".to_string()),
                        error: Some("API unavailable".to_string()),
                        counter_delta: 1.0,
                    },
                },
            ],
            |failure| {
                let metrics = MetricsCapture::start();
                let (endpoint, reason, logs) = match failure {
                    FailureEvent::CloudInit => {
                        let endpoint = BootEndpoint::CloudInit(CloudInitConsumer::Tenant);
                        let reason = OutcomeReason::MetadataNotFound;
                        let logs = capture_logs(|| {
                            emit(PxeCloudInitRequestFailed {
                                endpoint,
                                reason,
                                error: "metadata is missing".to_string(),
                            });
                        });
                        (endpoint, reason, logs)
                    }
                    FailureEvent::CustomIpxe => {
                        let endpoint = BootEndpoint::Boot;
                        let reason = OutcomeReason::UpstreamApiError;
                        let logs = capture_logs(|| {
                            emit(PxeCustomIpxeFetchFailed {
                                endpoint,
                                reason,
                                error: "API unavailable".to_string(),
                            });
                        });
                        (endpoint, reason, logs)
                    }
                };

                assert_eq!(logs.len(), 1, "one emit must produce one log record");
                let log = &logs[0];
                let endpoint = endpoint.label_value();
                let reason = reason.label_value();
                let labels = [("endpoint", endpoint.as_str()), ("reason", reason.as_str())];

                FailureRecord {
                    metadata_name: log.metadata_name.clone(),
                    level: log.level,
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    endpoint: log.field("endpoint").map(str::to_string),
                    reason: log.field("reason").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                    counter_delta: metrics.counter_delta(BOOT_OUTCOMES_METRIC, &labels),
                }
            },
        );
    }
}
