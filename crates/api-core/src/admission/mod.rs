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

//! Carbide API admission policy and middleware integration.
//!
//! The engine, limits, and retry modules own application-independent admission.
//! This module supplies Carbide configuration, route classification, transport
//! responses, metrics, and rate-limited diagnostics.

mod engine;
mod limits;
mod peak_ewma;
mod retry;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{OriginalUri, Request, State};
use axum::http::{HeaderValue, Response, StatusCode, header};
use axum::middleware::Next;
use axum::response::IntoResponse;
use engine::{AdmissionObserver, AdmissionRejection, ClientKeyRef, FairAdmission, RejectionReason};
pub(crate) use limits::{AdmissionLimits, ClientLimits};
use opentelemetry::metrics::{Meter, ObservableGauge};
use retry::{RejectionScope, RetryAdvice};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::auth::AuthContext;
use crate::cfg::file::ApiAdmissionControlConfig;
use crate::logging::log_limiter::LogLimiter;

const EXCLUDED_ADMIN_PATHS: &[&str] = &["/admin/static", "/admin/auth-callback", "/admin/logs"];
const GRPC_RETRY_PUSHBACK_HEADER: &str = "grpc-retry-pushback-ms";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RequestTransport {
    Grpc,
    Http,
}

impl RequestTransport {
    fn classify(path: &str) -> Option<Self> {
        // Keep these route prefixes in sync with the gRPC services and admin
        // routes mounted by the listener. The descriptor-backed test below
        // makes adding a gRPC service without updating this policy fail in CI;
        // the table test documents the intentional admin bypasses.
        if path.starts_with(::rpc::service_path!("")) {
            return Some(Self::Grpc);
        }

        if !is_path_or_child(path, "/admin")
            || EXCLUDED_ADMIN_PATHS
                .iter()
                .any(|excluded_path| is_path_or_child(path, excluded_path))
        {
            return None;
        }

        Some(Self::Http)
    }

    fn overloaded_response(self, retry: RetryAdvice) -> Response<Body> {
        match self {
            Self::Grpc => {
                let mut status =
                    tonic::Status::resource_exhausted("API admission capacity exhausted");
                // gRPC retry pushback is the transport-native equivalent of
                // Retry-After. Use the same normalized delay (expressed in
                // milliseconds) so HTTP and gRPC clients receive one policy.
                let pushback_milliseconds = retry.delay_seconds().saturating_mul(1_000).to_string();
                status.metadata_mut().insert(
                    GRPC_RETRY_PUSHBACK_HEADER,
                    tonic::metadata::MetadataValue::try_from(pushback_milliseconds.as_str())
                        .expect("retry pushback is an ASCII integer"),
                );
                status.into_http::<Body>()
            }
            Self::Http => {
                let retry_after = HeaderValue::from_str(&retry.delay_seconds().to_string())
                    .expect("retry delay is an ASCII integer");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(header::RETRY_AFTER, retry_after)],
                    "API admission capacity exhausted",
                )
                    .into_response()
            }
        }
    }
}

fn is_path_or_child(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
enum RejectionReasonLabel {
    QueueFull,
    EstimatedQueueDelay,
    QueueTimeout,
    ControllerUnavailable,
    ShuttingDown,
}

impl From<RejectionReason> for RejectionReasonLabel {
    fn from(reason: RejectionReason) -> Self {
        match reason {
            RejectionReason::QueueFull(_) => Self::QueueFull,
            RejectionReason::EstimatedQueueDelay(_) => Self::EstimatedQueueDelay,
            RejectionReason::QueueTimeout => Self::QueueTimeout,
            RejectionReason::ControllerUnavailable => Self::ControllerUnavailable,
            RejectionReason::ShuttingDown => Self::ShuttingDown,
        }
    }
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "api_admission_request_admitted",
    metric_name = "carbide_api_admission_admitted_total",
    component = "nico-api",
    log = off,
    metric = counter,
    describe = "Number of API requests admitted for execution"
)]
struct RequestAdmitted;

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "api_admission_request_rejected",
    metric_name = "carbide_api_admission_rejected_total",
    component = "nico-api",
    log = off,
    metric = counter,
    describe = "Number of API requests rejected before handler execution"
)]
struct RequestRejected {
    #[label]
    reason: RejectionReasonLabel,
    #[label]
    scope: RejectionScopeLabel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
enum RejectionScopeLabel {
    Global,
    Client,
    Other,
}

impl From<Option<RejectionScope>> for RejectionScopeLabel {
    fn from(scope: Option<RejectionScope>) -> Self {
        match scope {
            Some(RejectionScope::Global) => Self::Global,
            Some(RejectionScope::Client) => Self::Client,
            None => Self::Other,
        }
    }
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "api_admission_pending_wait_finished",
    metric_name = "carbide_api_admission_pending_wait_duration_seconds",
    component = "nico-api",
    log = off,
    metric = histogram,
    describe = "Duration API requests spent waiting for admission"
)]
struct PendingWaitFinished {
    #[observation]
    duration: Duration,
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "api_admission_handler_execution_finished",
    metric_name = "carbide_api_admission_handler_execution_duration_seconds",
    component = "nico-api",
    log = off,
    metric = histogram,
    describe = "Duration of admitted API request handler execution"
)]
struct HandlerExecutionFinished {
    #[observation]
    duration: Duration,
}

struct CarbideAdmissionObserver;

impl AdmissionObserver for CarbideAdmissionObserver {
    fn admitted(&self) {
        carbide_instrument::emit(RequestAdmitted);
    }

    fn pending_finished(&self, duration: Duration) {
        carbide_instrument::emit(PendingWaitFinished { duration });
    }

    fn execution_finished(&self, duration: Duration) {
        carbide_instrument::emit(HandlerExecutionFinished { duration });
    }
}

pub(crate) struct ApiAdmissionControl {
    engine: Arc<FairAdmission>,
    default_client_limits: ClientLimits,
    service_limits: HashMap<String, ClientLimits>,
    rejection_log_limiter: LogLimiter<(RejectionReason, RequestTransport)>,
    _work_in_flight_gauge: ObservableGauge<u64>,
    _pending_requests_gauge: ObservableGauge<u64>,
}

/// Shared admission handle installed inside the authenticated admin router.
///
/// The admin UI owns its OAuth middleware, so admission must be layered inside
/// that crate after authentication has populated [`AuthContext`]. Keeping this
/// as an opaque handle lets the web crate place the middleware correctly
/// without exposing the transport-independent engine or Carbide policy.
#[derive(Clone)]
pub struct AdminAdmissionControl(Arc<ApiAdmissionControl>);

impl AdminAdmissionControl {
    pub(crate) fn new(control: Arc<ApiAdmissionControl>) -> Self {
        Self(control)
    }

    pub async fn middleware(
        State(control): State<Option<Self>>,
        request: Request,
        next: Next,
    ) -> Response<Body> {
        let Some(control) = control else {
            return next.run(request).await;
        };
        if classify_request(&request) != Some(RequestTransport::Http) {
            return next.run(request).await;
        }
        enforce_transport(control.0, request, next, RequestTransport::Http).await
    }
}

impl ApiAdmissionControl {
    pub(crate) fn from_config(
        config: &ApiAdmissionControlConfig,
        meter: &Meter,
        shutdown: CancellationToken,
        join_set: &mut JoinSet<()>,
    ) -> eyre::Result<Option<Arc<Self>>> {
        let Some(limits) = config.admission_limits()? else {
            return Ok(None);
        };

        let service_limits = config
            .service_limits
            .iter()
            .map(|(service_id, service_limits)| {
                ClientLimits::new(
                    service_limits.max_work_in_flight,
                    service_limits.max_pending,
                    service_limits.pending_timeout,
                    limits.max_work_in_flight(),
                    limits.max_pending(),
                )
                .map(|limits| (service_id.clone(), limits))
                .map_err(|error| {
                    eyre::eyre!("api_admission_control.service_limits.{service_id}.{error}")
                })
            })
            .collect::<eyre::Result<HashMap<_, _>>>()?;

        let observer: Arc<dyn AdmissionObserver> = Arc::new(CarbideAdmissionObserver);
        let engine = FairAdmission::start(limits, shutdown, observer, join_set);
        let work_in_flight_gauge = register_occupancy_gauge(
            meter,
            "carbide_api_admission_work_in_flight",
            "Number of API requests currently holding an execution slot",
            Arc::clone(&engine),
            |snapshot| snapshot.work_in_flight,
        );
        let pending_requests_gauge = register_occupancy_gauge(
            meter,
            "carbide_api_admission_pending_requests",
            "Number of API requests currently waiting for an execution slot",
            Arc::clone(&engine),
            |snapshot| snapshot.pending,
        );
        Ok(Some(Arc::new(Self {
            engine,
            default_client_limits: limits.default_client(),
            service_limits,
            rejection_log_limiter: LogLimiter::default(),
            _work_in_flight_gauge: work_in_flight_gauge,
            _pending_requests_gauge: pending_requests_gauge,
        })))
    }

    fn rejection_response(
        &self,
        transport: RequestTransport,
        rejection: AdmissionRejection,
    ) -> Response<Body> {
        let AdmissionRejection { reason, retry } = rejection;
        carbide_instrument::emit(RequestRejected {
            reason: reason.into(),
            scope: reason.scope().into(),
        });
        if self.rejection_log_limiter.should_log(
            &(reason, transport),
            "API request rejected by admission control",
        ) {
            tracing::warn!(
                rejection_reason = ?reason,
                rejection_scope = ?reason.scope(),
                retry_pressure_scope = ?retry.pressure_scope(),
                retry_delay_seconds = retry.delay_seconds(),
                request_transport = ?transport,
                "API request rejected by admission control"
            );
        }
        transport.overloaded_response(retry)
    }

    #[cfg(test)]
    fn snapshot(&self) -> engine::AdmissionSnapshot {
        self.engine.snapshot()
    }

    fn client<'a>(&self, request: &'a Request) -> (ClientKeyRef<'a>, ClientLimits) {
        let auth_context = request.extensions().get::<AuthContext>();
        if let Some(external_user) = auth_context.and_then(AuthContext::get_external_user_info) {
            let key = ClientKeyRef::ExternalUser(external_user);
            return (key, self.default_client_limits);
        }
        if let Some(service_id) = auth_context.and_then(AuthContext::get_spiffe_service_id) {
            let limits = self
                .service_limits
                .get(service_id)
                .copied()
                .unwrap_or(self.default_client_limits);
            return (ClientKeyRef::ServiceId(service_id), limits);
        }
        if let Some(machine_id) = auth_context.and_then(AuthContext::get_spiffe_machine_id) {
            return (
                ClientKeyRef::MachineId(machine_id),
                self.default_client_limits,
            );
        }
        (ClientKeyRef::Default, self.default_client_limits)
    }
}

fn register_occupancy_gauge(
    meter: &Meter,
    name: &'static str,
    description: &'static str,
    engine: Arc<FairAdmission>,
    value: fn(engine::AdmissionSnapshot) -> usize,
) -> ObservableGauge<u64> {
    meter
        .u64_observable_gauge(name)
        .with_description(description)
        .with_callback(move |observer| {
            observer.observe(value(engine.snapshot()) as u64, &[]);
        })
        .build()
}

#[cfg(test)]
pub(crate) async fn enforce(
    State(control): State<Arc<ApiAdmissionControl>>,
    request: Request,
    next: Next,
) -> Response<Body> {
    let Some(transport) = classify_request(&request) else {
        return next.run(request).await;
    };

    enforce_transport(control, request, next, transport).await
}

pub(crate) async fn enforce_grpc(
    State(control): State<Arc<ApiAdmissionControl>>,
    request: Request,
    next: Next,
) -> Response<Body> {
    if classify_request(&request) != Some(RequestTransport::Grpc) {
        return next.run(request).await;
    }
    enforce_transport(control, request, next, RequestTransport::Grpc).await
}

fn classify_request(request: &Request) -> Option<RequestTransport> {
    let path = request
        .extensions()
        .get::<OriginalUri>()
        .map_or_else(|| request.uri().path(), |uri| uri.path());
    RequestTransport::classify(path)
}

async fn enforce_transport(
    control: Arc<ApiAdmissionControl>,
    request: Request,
    next: Next,
    transport: RequestTransport,
) -> Response<Body> {
    let (client_key, client_limits) = control.client(&request);
    let permit = match control.engine.acquire(client_key, client_limits).await {
        Ok(permit) => permit,
        Err(rejection) => return control.rejection_response(transport, rejection),
    };
    let response = next.run(request).await;
    drop(permit);
    response
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::http::Request;
    use axum::routing::get;
    use carbide_instrument::testing::MetricsCapture;
    use futures::poll;
    use prost::Message;
    use prost_types::FileDescriptorSet;
    use tokio::sync::Notify;
    use tower::ServiceExt;

    use super::*;

    static ADMISSION_TEST_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn controller(
        max_work_in_flight: usize,
        max_pending: usize,
        pending_timeout: Duration,
        shutdown: CancellationToken,
    ) -> (Arc<ApiAdmissionControl>, JoinSet<()>) {
        let mut join_set = JoinSet::new();
        let control = ApiAdmissionControl::from_config(
            &ApiAdmissionControlConfig {
                enabled: true,
                max_work_in_flight,
                max_pending,
                max_work_in_flight_per_client: max_work_in_flight,
                max_pending_per_client: max_pending,
                pending_timeout,
                client_idle_timeout: Duration::from_secs(60),
                service_limits: Default::default(),
            },
            &opentelemetry::global::meter("api-admission-tests"),
            shutdown,
            &mut join_set,
        )
        .expect("test admission config is valid")
        .expect("test admission is enabled");
        (control, join_set)
    }

    fn test_client(control: &ApiAdmissionControl) -> (ClientKeyRef<'_>, ClientLimits) {
        (
            ClientKeyRef::ServiceId("test-client"),
            control.default_client_limits,
        )
    }

    #[test]
    fn client_keys_distinguish_identity_kinds() {
        assert_ne!(
            ClientKeyRef::ServiceId("same-identifier"),
            ClientKeyRef::MachineId("same-identifier"),
        );
        assert_ne!(ClientKeyRef::ServiceId("default"), ClientKeyRef::Default,);
    }

    fn rejection(reason: RejectionReason, seconds: u64) -> AdmissionRejection {
        AdmissionRejection {
            reason,
            retry: RetryAdvice::for_test(seconds, reason.scope().unwrap_or(RejectionScope::Global)),
        }
    }

    #[test]
    fn request_classification_covers_business_and_infrastructure_routes() {
        let cases = [
            (
                ::rpc::service_path!("FindMachines"),
                Some(RequestTransport::Grpc),
            ),
            ("/admin", Some(RequestTransport::Http)),
            ("/admin/machine", Some(RequestTransport::Http)),
            ("/", None),
            (
                "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
                None,
            ),
            ("/administrator", None),
            ("/admin/staticity", Some(RequestTransport::Http)),
            ("/unrecognized", None),
        ];

        for (path, expected) in cases {
            assert_eq!(RequestTransport::classify(path), expected, "path: {path}");
        }

        for excluded_path in EXCLUDED_ADMIN_PATHS {
            assert!(
                is_path_or_child(excluded_path, "/admin"),
                "excluded path must be an admin path: {excluded_path}"
            );
            assert_eq!(RequestTransport::classify(excluded_path), None);
            let child_path = format!("{excluded_path}/child");
            assert_eq!(RequestTransport::classify(&child_path), None);
        }
    }

    #[test]
    fn nested_admin_classification_uses_original_uri() {
        let cases = [
            ("/machine", "/admin/machine", Some(RequestTransport::Http)),
            ("/static/app.css", "/admin/static/app.css", None),
        ];

        for (nested_path, original_path, expected) in cases {
            let mut request = Request::builder()
                .uri(nested_path)
                .body(Body::empty())
                .unwrap();
            request
                .extensions_mut()
                .insert(OriginalUri(original_path.parse().unwrap()));
            assert_eq!(
                classify_request(&request),
                expected,
                "original path: {original_path}"
            );
        }
    }

    #[test]
    fn every_forge_grpc_route_is_classified_for_admission() {
        let descriptor = FileDescriptorSet::decode(::rpc::REFLECTION_API_SERVICE_DESCRIPTOR)
            .expect("API service descriptor is valid");
        let mut route_count = 0;

        for file in descriptor.file {
            let package = file.package.unwrap_or_default();
            // The reflection descriptor also contains protocols used by API
            // clients. Only the forge package is mounted by this listener.
            if package != "forge" {
                continue;
            }

            for service in file.service {
                let service_name = service.name.clone().expect("service has a name");
                let qualified_service = if package.is_empty() {
                    service_name
                } else {
                    format!("{package}.{service_name}")
                };

                for method in service.method {
                    let method = method.name.expect("method has a name");
                    let path = format!("/{qualified_service}/{method}");
                    route_count += 1;
                    assert_eq!(
                        RequestTransport::classify(&path),
                        Some(RequestTransport::Grpc),
                        "gRPC route {path} bypasses admission; update the route policy"
                    );
                }
            }
        }

        assert!(route_count > 0, "API descriptor contains no gRPC routes");
    }

    #[test]
    fn overload_responses_match_the_request_transport() {
        let retry = RetryAdvice::for_test(7, RejectionScope::Client);
        let grpc = RequestTransport::Grpc.overloaded_response(retry);
        assert_eq!(grpc.status(), StatusCode::OK);
        assert_eq!(grpc.headers().get("grpc-status").unwrap(), "8");
        assert_eq!(
            grpc.headers().get(GRPC_RETRY_PUSHBACK_HEADER).unwrap(),
            "7000"
        );

        let http = RequestTransport::Http.overloaded_response(retry);
        assert_eq!(http.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(http.headers().get(header::RETRY_AFTER).unwrap(), "7");
    }

    #[tokio::test(start_paused = true)]
    async fn rejection_reason_is_preserved_in_metrics() {
        let _serial = ADMISSION_TEST_SERIAL.lock().await;
        let metrics = MetricsCapture::start();
        let (control, _join_set) =
            controller(1, 1, Duration::from_secs(5), CancellationToken::new());
        let (client_key, client_limits) = test_client(&control);
        let executing = control
            .engine
            .acquire(client_key, client_limits)
            .await
            .expect("first work is admitted");
        let pending = control.engine.acquire(client_key, client_limits);
        tokio::pin!(pending);
        assert!(poll!(&mut pending).is_pending());

        tokio::time::advance(Duration::from_secs(5)).await;
        let timeout_rejection = pending.await.expect_err("pending work must time out");
        let response = control.rejection_response(RequestTransport::Grpc, timeout_rejection);
        assert_eq!(response.headers().get("grpc-status").unwrap(), "8");
        drop(executing);

        assert_eq!(
            metrics.counter_delta(
                "carbide_api_admission_rejected_total",
                &[("reason", "queue_timeout"), ("scope", "other")],
            ),
            1.0
        );
        assert_eq!(
            metrics
                .histogram_count_delta("carbide_api_admission_pending_wait_duration_seconds", &[]),
            1
        );
        let cases = [
            (
                RejectionReason::QueueFull(RejectionScope::Global),
                "queue_full",
                "global",
            ),
            (
                RejectionReason::ControllerUnavailable,
                "controller_unavailable",
                "other",
            ),
            (RejectionReason::ShuttingDown, "shutting_down", "other"),
        ];
        for (reason, label, scope) in cases {
            let response = control.rejection_response(RequestTransport::Http, rejection(reason, 3));
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                metrics.counter_delta(
                    "carbide_api_admission_rejected_total",
                    &[("reason", label), ("scope", scope)],
                ),
                1.0,
                "rejection reason {reason:?} must be preserved"
            );
        }
    }

    #[tokio::test]
    async fn rejection_log_limiter_distinguishes_reason_and_transport() {
        let (control, _join_set) =
            controller(1, 1, Duration::from_secs(1), CancellationToken::new());
        let summary = "API request rejected by admission control";

        assert!(control.rejection_log_limiter.should_log(
            &(
                RejectionReason::QueueFull(RejectionScope::Global),
                RequestTransport::Grpc,
            ),
            summary
        ));
        assert!(!control.rejection_log_limiter.should_log(
            &(
                RejectionReason::QueueFull(RejectionScope::Global),
                RequestTransport::Grpc,
            ),
            summary
        ));
        assert!(control.rejection_log_limiter.should_log(
            &(
                RejectionReason::QueueFull(RejectionScope::Global),
                RequestTransport::Http,
            ),
            summary
        ));
        assert!(control.rejection_log_limiter.should_log(
            &(RejectionReason::QueueTimeout, RequestTransport::Grpc),
            summary
        ));
    }

    #[tokio::test]
    async fn grpc_and_admin_routes_share_capacity_and_bypasses_remain_available() {
        let _serial = ADMISSION_TEST_SERIAL.lock().await;
        let (controller, _join_set) =
            controller(1, 1, Duration::from_secs(1), CancellationToken::new());
        let handler_calls = Arc::new(AtomicUsize::new(0));
        let handler_started = Arc::new(Notify::new());
        let release_handler = Arc::new(Notify::new());
        let blocking_handler = {
            let handler_calls = Arc::clone(&handler_calls);
            let handler_started = Arc::clone(&handler_started);
            let release_handler = Arc::clone(&release_handler);
            move || {
                let handler_calls = Arc::clone(&handler_calls);
                let handler_started = Arc::clone(&handler_started);
                let release_handler = Arc::clone(&release_handler);
                async move {
                    handler_calls.fetch_add(1, Ordering::SeqCst);
                    handler_started.notify_one();
                    release_handler.notified().await;
                    "business response"
                }
            }
        };
        let immediate_handler = {
            let handler_calls = Arc::clone(&handler_calls);
            move || {
                let handler_calls = Arc::clone(&handler_calls);
                async move {
                    handler_calls.fetch_add(1, Ordering::SeqCst);
                    "business response"
                }
            }
        };
        let grpc_router = Router::new()
            .route(::rpc::service_path!("Test"), get(immediate_handler))
            .layer(axum::middleware::from_fn_with_state(
                Arc::clone(&controller),
                enforce_grpc,
            ));
        let admin_router = Router::new()
            .route("/business", get(blocking_handler))
            .route("/static/test.css", get(|| async { "static" }))
            .layer(axum::middleware::from_fn_with_state(
                Some(AdminAdmissionControl::new(Arc::clone(&controller))),
                AdminAdmissionControl::middleware,
            ));
        let router = Router::new()
            .merge(grpc_router)
            .nest("/admin", admin_router);

        let executing_router = router.clone();
        let executing = tokio::spawn(async move {
            executing_router
                .oneshot(
                    Request::builder()
                        .uri("/admin/business")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        });
        handler_started.notified().await;
        assert_eq!(handler_calls.load(Ordering::SeqCst), 1);

        let pending_router = router.clone();
        let pending = tokio::spawn(async move {
            pending_router
                .oneshot(
                    Request::builder()
                        .uri(::rpc::service_path!("Test"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        });
        for _ in 0..100 {
            if controller.snapshot().pending == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(controller.snapshot().pending, 1);

        let bypass = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin/static/test.css")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bypass.status(), StatusCode::OK);

        let rejected = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin/business")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
        let http_retry_seconds = rejected
            .headers()
            .get(header::RETRY_AFTER)
            .unwrap()
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!((1..=30).contains(&http_retry_seconds));
        assert_eq!(handler_calls.load(Ordering::SeqCst), 1);

        let rejected_grpc = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(::rpc::service_path!("Test"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected_grpc.headers().get("grpc-status").unwrap(), "8");
        let grpc_pushback_milliseconds = rejected_grpc
            .headers()
            .get(GRPC_RETRY_PUSHBACK_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert_eq!(grpc_pushback_milliseconds % 1_000, 0);
        assert!((1_000..=30_000).contains(&grpc_pushback_milliseconds));
        assert_eq!(handler_calls.load(Ordering::SeqCst), 1);

        release_handler.notify_one();
        let executing = executing.await.unwrap();
        let admitted_grpc = pending.await.unwrap();
        assert_eq!(executing.status(), StatusCode::OK);
        assert_eq!(admitted_grpc.status(), StatusCode::OK);
        assert_eq!(handler_calls.load(Ordering::SeqCst), 2);
        // Capacity is released when handlers return, even while their response
        // bodies remain alive and unconsumed.
        assert_eq!(controller.snapshot().work_in_flight, 0);
    }
}
