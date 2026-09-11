// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Redfish `EventService`: bounded publication and replay history, subscriber
//! admission, the `ServerSentEventUri` endpoint, and the `EventDestination`
//! members it creates for open streams. Stream delivery and fault scripts live
//! in `crate::sse`; the `/Mock/EventService` controls in `crate::event_controls`.

use std::borrow::Cow;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use bytes::Bytes;
use futures::stream;
use nv_redfish::event_service::EventStreamPayload;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use super::event_destination;
use super::event_destination::SUBSCRIPTIONS;
use crate::BmcState;
use crate::combined_server::OutputStallTimeout;
use crate::http::redfish_error;
use crate::json::{JsonExt, JsonPatch};
use crate::redfish::Resource;
use crate::redfish::session_service::generate_token;
use crate::sse::{Delivery, StreamStep, Subscriber};

const ROOT: &str = "/redfish/v1/EventService";
const SSE: &str = "/redfish/v1/EventService/SSE";
const MAX_SCRIPTS: usize = 8;
const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
const MAX_QUEUED_BYTES: usize = 4 * MAX_SCRIPT_BYTES;
const MAX_SCRIPT_STEPS: usize = 256;
const MAX_SCRIPT_DELAY_MS: u64 = 60_000;

/// Resource limits for one BMC's EventService. Validate them with
/// `EventServiceConfig::try_from`; `Default` is always valid.
#[derive(Clone, Debug)]
pub struct EventServiceLimits {
    /// Held response bodies, including closing streams. Excess opens return 503.
    pub max_subscribers: usize,
    /// Encoded bytes per frame, including SSE framing. Must fit in history.
    pub max_frame_bytes: usize,
    /// Retained replayable frames.
    pub max_frames: usize,
    /// Retained encoded bytes. At least `max_frame_bytes`.
    pub max_history_bytes: usize,
    /// Comment heartbeat interval on idle live streams. `None` disables heartbeats.
    pub heartbeat: Option<Duration>,
    /// How long an emitted frame may wait for transport progress before the
    /// serving connection is closed. Idle waits between frames do not count.
    pub output_stall_timeout: Duration,
}

impl Default for EventServiceLimits {
    fn default() -> Self {
        Self {
            max_subscribers: 16,
            max_frame_bytes: 256 * 1024,
            max_frames: 256,
            max_history_bytes: 4 * 1024 * 1024,
            heartbeat: Some(Duration::from_secs(15)),
            output_stall_timeout: Duration::from_secs(60),
        }
    }
}

/// Validated [`EventServiceLimits`]; an invalid configuration cannot reach
/// router construction.
#[derive(Clone, Debug, Default)]
pub struct EventServiceConfig {
    pub(crate) limits: EventServiceLimits,
}

impl TryFrom<EventServiceLimits> for EventServiceConfig {
    type Error = EventServiceError;

    /// Every count must be positive, a frame must fit in history, and each
    /// interval must be positive and at most one day.
    fn try_from(limits: EventServiceLimits) -> Result<Self, EventServiceError> {
        const MAX_INTERVAL: Duration = Duration::from_secs(86_400);
        let bounded = |interval: Duration| !interval.is_zero() && interval <= MAX_INTERVAL;
        let valid = limits.max_subscribers > 0
            && limits.max_frame_bytes > 0
            && limits.max_frames > 0
            && limits.max_frame_bytes <= limits.max_history_bytes
            && limits.heartbeat.is_none_or(bounded)
            && bounded(limits.output_stall_timeout);
        if !valid {
            return Err(EventServiceError::Invalid(
                "invalid event-service limits".into(),
            ));
        }
        Ok(Self { limits })
    }
}

/// Rejection from publication, subscription, or fault-script admission.
#[derive(Debug, thiserror::Error)]
pub enum EventServiceError {
    /// The event service or subscription does not exist.
    #[error("event service or subscription not found")]
    NotFound,
    /// The payload, cursor, or configuration is invalid; HTTP controls return 400.
    #[error("{0}")]
    Invalid(String),
    /// A configured frame or script byte/step limit was exceeded; HTTP returns 413.
    #[error("event or script exceeds its configured limit")]
    TooLarge,
    /// Subscriber or script capacity is exhausted; HTTP returns 503.
    #[error("event service is at capacity")]
    Unavailable,
}

/// Observable per-BMC stream state; transport IDs are unrelated to log entry IDs.
#[derive(Debug, Serialize)]
pub struct EventServiceStats {
    /// Opaque incarnation token; changes on BMC reset.
    pub generation: String,
    /// Number of active EventDestination resources.
    pub subscribers: usize,
    /// Response bodies still held by readers/transports, including closing streams.
    /// These retain admission slots until dropped.
    pub streams: usize,
    /// Number of replayable frames retained.
    pub retained_frames: usize,
    /// Encoded bytes retained, including frame delimiters.
    pub retained_bytes: usize,
    /// Scripts awaiting an accepted connection.
    pub queued_scripts: usize,
    /// Bytes in queued raw scripts.
    pub queued_script_bytes: usize,
    /// Subscribers terminated because their cursor fell behind retention.
    pub lagged: u64,
    /// Subscribers explicitly closed or closed by reset.
    pub closed: u64,
}

#[derive(Debug)]
struct Frame {
    seq: u64,
    bytes: Bytes,
}

#[derive(Debug)]
struct Subscription {
    script: Option<VecDeque<StreamStep>>,
}

#[derive(Debug)]
struct Inner {
    generation: String,
    next_seq: u64,
    next_subscriber: u64,
    subscribers: BTreeMap<u64, Subscription>,
    streams: usize,
    frames: VecDeque<Frame>,
    retained_bytes: usize,
    scripts: VecDeque<(VecDeque<StreamStep>, usize)>,
    script_bytes: usize,
    lagged: u64,
    closed: u64,
}

/// Outcome of taking a scripted subscriber's next step.
pub(crate) enum ScriptStep {
    /// The subscription was closed or deleted.
    Closed,
    /// The script has no remaining steps: clean EOF.
    Finished,
    Step(StreamStep),
}

/// Outcome of asking for a live subscriber's next frame.
pub(crate) enum LiveFrame {
    /// The subscription was closed or deleted.
    Closed,
    /// The cursor's next frame was evicted from history.
    Lagged,
    Frame(Bytes),
    /// Nothing new has been published.
    Pending,
}

/// Shared, bounded event state for one BMC. Publish never waits for a reader.
/// Hardware profiles configure support; access the running service through `BmcState`.
#[derive(Debug)]
pub struct EventServiceState {
    pub(crate) config: EventServiceConfig,
    inner: Mutex<Inner>,
    changed: watch::Sender<()>,
}

impl EventServiceState {
    pub(crate) fn new(config: EventServiceConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            inner: Mutex::new(Inner {
                generation: generate_token(),
                next_seq: 1,
                next_subscriber: 1,
                subscribers: BTreeMap::new(),
                streams: 0,
                frames: VecDeque::new(),
                retained_bytes: 0,
                scripts: VecDeque::new(),
                script_bytes: 0,
                lagged: 0,
                closed: 0,
            }),
            changed: watch::channel(()).0,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("event state poisoned")
    }

    /// Clear replay history, queued scripts, and active subscriptions, and
    /// start a new generation so earlier cursors are rejected.
    pub(crate) fn reset(&self) {
        let mut inner = self.lock();
        inner.generation = generate_token();
        inner.next_seq = 1;
        inner.frames.clear();
        inner.retained_bytes = 0;
        inner.scripts.clear();
        inner.script_bytes = 0;
        self.close_locked(&mut inner);
    }

    // Destructors must not recover potentially inconsistent poisoned state.
    pub(crate) fn unsubscribe_on_drop(&self, id: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.subscribers.remove(&id);
            inner.streams = inner.streams.saturating_sub(1);
        }
    }

    fn close_on_drop(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            self.close_locked(&mut inner);
        }
    }

    fn close_locked(&self, inner: &mut Inner) {
        inner.closed = inner.closed.saturating_add(inner.subscribers.len() as u64);
        // Claimed script data belongs to the registry, so closing frees it even
        // if transport flow control has stopped polling the response body.
        inner.subscribers.clear();
        self.changed.send_replace(());
    }

    /// Validate an Event or MetricReport document with the consumer's decoder
    /// and publish it, returning its opaque SSE ID. Invalid or oversized
    /// documents consume no sequence number and wake no reader.
    pub fn publish(&self, payload: Value) -> Result<String, EventServiceError> {
        let data = serde_json::to_string(&payload)
            .map_err(|e| EventServiceError::Invalid(e.to_string()))?;
        let _: EventStreamPayload = serde_json::from_value(payload)
            .map_err(|e| EventServiceError::Invalid(e.to_string()))?;
        let mut inner = self.lock();
        let seq = inner.next_seq;
        let next = seq.checked_add(1).ok_or(EventServiceError::Unavailable)?;
        let id = format!("{}:{seq}", inner.generation);
        let bytes = Bytes::from(format!("id: {id}\ndata: {data}\n\n"));
        if bytes.len() > self.config.limits.max_frame_bytes {
            return Err(EventServiceError::TooLarge);
        }
        inner.next_seq = next;
        inner.retained_bytes += bytes.len();
        inner.frames.push_back(Frame { seq, bytes });
        while inner.frames.len() > self.config.limits.max_frames
            || inner.retained_bytes > self.config.limits.max_history_bytes
        {
            inner.retained_bytes -= inner.frames.pop_front().unwrap().bytes.len();
        }
        self.changed.send_replace(());
        Ok(id)
    }

    /// End active subscriptions without clearing replay history or queued scripts.
    /// Closing bodies retain admission slots until the transport drops them.
    pub fn close_subscribers(&self) {
        let mut inner = self.lock();
        self.close_locked(&mut inner);
    }

    /// Return bounded state and counters.
    pub fn stats(&self) -> EventServiceStats {
        let inner = self.lock();
        EventServiceStats {
            generation: inner.generation.clone(),
            subscribers: inner.subscribers.len(),
            streams: inner.streams,
            retained_frames: inner.frames.len(),
            retained_bytes: inner.retained_bytes,
            queued_scripts: inner.scripts.len(),
            queued_script_bytes: inner.script_bytes,
            lagged: inner.lagged,
            closed: inner.closed,
        }
    }

    /// Queue a raw script for the next accepted live-only connection. Limits:
    /// 8 queued scripts, 256 steps and 1 MiB per script, 4 MiB queued bytes,
    /// and 60 seconds of total delay per script. A terminal step must be last;
    /// reaching the end without one is a clean EOF.
    pub fn queue_script(&self, steps: Vec<StreamStep>) -> Result<(), EventServiceError> {
        if steps.is_empty() || steps.len() > MAX_SCRIPT_STEPS {
            return Err(EventServiceError::TooLarge);
        }
        let mut bytes = 0usize;
        let mut delay = 0u64;
        for (index, step) in steps.iter().enumerate() {
            match step {
                StreamStep::Bytes { data } => bytes = bytes.saturating_add(data.len()),
                StreamStep::Delay { millis } => delay = delay.saturating_add(*millis),
                StreamStep::Eof | StreamStep::Error if index + 1 != steps.len() => {
                    return Err(EventServiceError::Invalid(
                        "terminal script step must be last".into(),
                    ));
                }
                _ => {}
            }
        }
        if bytes > MAX_SCRIPT_BYTES || delay > MAX_SCRIPT_DELAY_MS {
            return Err(EventServiceError::TooLarge);
        }
        let mut inner = self.lock();
        if inner.scripts.len() >= MAX_SCRIPTS
            || inner.script_bytes.saturating_add(bytes) > MAX_QUEUED_BYTES
        {
            return Err(EventServiceError::Unavailable);
        }
        inner.scripts.push_back((steps.into(), bytes));
        inner.script_bytes += bytes;
        Ok(())
    }

    /// Active EventDestination member IDs in ascending order.
    fn subscription_ids(&self) -> Vec<u64> {
        self.lock().subscribers.keys().copied().collect()
    }

    /// Opaque Context of an active subscription. Its shape differs from frame
    /// IDs so echoing it as a `Last-Event-ID` is rejected.
    fn subscription_context(&self, id: u64) -> Option<String> {
        let inner = self.lock();
        inner
            .subscribers
            .contains_key(&id)
            .then(|| format!("subscription:{}:{id}", inner.generation))
    }

    /// End one subscription. Its response body keeps the admission slot until dropped.
    fn delete_subscription(&self, id: u64) -> bool {
        let mut inner = self.lock();
        if inner.subscribers.remove(&id).is_none() {
            return false;
        }
        inner.closed = inner.closed.saturating_add(1);
        self.changed.send_replace(());
        true
    }

    pub(crate) fn is_subscribed(&self, id: u64) -> bool {
        self.lock().subscribers.contains_key(&id)
    }

    pub(crate) fn pop_script_step(&self, id: u64) -> ScriptStep {
        let mut inner = self.lock();
        match inner
            .subscribers
            .get_mut(&id)
            .and_then(|subscription| subscription.script.as_mut())
        {
            None => ScriptStep::Closed,
            Some(script) => script
                .pop_front()
                .map_or(ScriptStep::Finished, ScriptStep::Step),
        }
    }

    pub(crate) fn next_frame(&self, id: u64, next_seq: u64) -> LiveFrame {
        let mut inner = self.lock();
        if !inner.subscribers.contains_key(&id) {
            return LiveFrame::Closed;
        }
        let Some(first_seq) = inner.frames.front().map(|frame| frame.seq) else {
            return LiveFrame::Pending;
        };
        if first_seq > next_seq {
            inner.lagged += 1;
            return LiveFrame::Lagged;
        }
        // Frames have contiguous sequence numbers within one generation.
        usize::try_from(next_seq - first_seq)
            .ok()
            .and_then(|offset| inner.frames.get(offset))
            .map_or(LiveFrame::Pending, |frame| {
                LiveFrame::Frame(frame.bytes.clone())
            })
    }

    /// Register a subscriber. Cursor validation precedes the capacity check so
    /// a stale cursor is reported even while closing bodies hold every slot. A
    /// cursor naming the frame just before the oldest retained one still resumes
    /// losslessly. Only live-only opens claim a queued raw script.
    pub(crate) fn subscribe(
        self: &Arc<Self>,
        last_id: Option<&str>,
    ) -> Result<Subscriber, EventServiceError> {
        let mut inner = self.lock();
        let next_seq = match last_id {
            None => inner.next_seq,
            Some(id) => {
                let seq = id
                    .strip_prefix(&inner.generation)
                    .and_then(|suffix| suffix.strip_prefix(':'))
                    .and_then(|seq| seq.parse::<u64>().ok())
                    .filter(|seq| id == format!("{}:{seq}", inner.generation))
                    // Bound first: once `seq < next_seq`, `seq + 1` cannot overflow.
                    .filter(|seq| *seq < inner.next_seq)
                    .filter(|seq| inner.frames.front().is_some_and(|f| seq + 1 >= f.seq))
                    .ok_or_else(|| {
                        EventServiceError::Invalid(
                            "Last-Event-ID is not in retained history".into(),
                        )
                    })?;
                seq + 1
            }
        };
        if inner.streams >= self.config.limits.max_subscribers {
            return Err(EventServiceError::Unavailable);
        }
        let id = inner.next_subscriber;
        inner.next_subscriber = id.checked_add(1).ok_or(EventServiceError::Unavailable)?;
        let script = match last_id {
            None => inner.scripts.pop_front().map(|(steps, bytes)| {
                inner.script_bytes -= bytes;
                steps
            }),
            Some(_) => None,
        };
        let delivery = if script.is_some() {
            Delivery::Script { delay_until: None }
        } else {
            Delivery::Live {
                next_seq,
                heartbeat_at: None,
            }
        };
        inner.subscribers.insert(id, Subscription { script });
        inner.streams += 1;
        Ok(Subscriber::new(
            self.clone(),
            id,
            delivery,
            self.changed.subscribe(),
        ))
    }
}

impl IntoResponse for EventServiceError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        };
        redfish_error(status, &self.to_string())
    }
}

pub(crate) fn add_routes(router: Router<BmcState>) -> Router<BmcState> {
    router
        .route(&resource().odata_id, get(service))
        .route(
            SSE,
            // Axum would otherwise serve HEAD through the GET handler and register a subscriber.
            get(events).head(|| async { (StatusCode::METHOD_NOT_ALLOWED, [("allow", "GET")]) }),
        )
        .route(SUBSCRIPTIONS, get(subscriptions))
        .route(
            &format!("{SUBSCRIPTIONS}/{{id}}"),
            get(subscription).delete(delete_subscription),
        )
}

/// The BMC's event service, or the 404 every event route answers without one.
pub(crate) fn enabled(state: &BmcState) -> Result<Arc<EventServiceState>, EventServiceError> {
    state
        .event_service
        .clone()
        .ok_or(EventServiceError::NotFound)
}

pub(crate) fn resource() -> Resource<'static> {
    Resource {
        odata_id: Cow::Borrowed(ROOT),
        odata_type: Cow::Borrowed("#EventService.v1_2_0.EventService"),
        id: Cow::Borrowed("EventService"),
        name: Cow::Borrowed("Event Service"),
    }
}

async fn service(State(state): State<BmcState>) -> Result<Response, EventServiceError> {
    enabled(&state)?;
    Ok(Json(
        resource()
            .json_patch()
            .patch(json!({
                "ServiceEnabled": true, "ServerSentEventUri": SSE,
                "EventFormatTypes": ["Event", "MetricReport"]
            }))
            .patch(event_destination::collection().nav_property("Subscriptions")),
    )
    .into_response())
}

// HTTP list delimiters inside quoted parameter values are literal characters.
fn split_quoted(value: &str, delimiter: char) -> impl Iterator<Item = &str> {
    let mut quoted = false;
    let mut escaped = false;
    value.split(move |ch| {
        if escaped {
            escaped = false;
        } else if quoted && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        } else if !quoted && ch == delimiter {
            return true;
        }
        false
    })
}

/// Media-range negotiation for the served `text/event-stream` representation,
/// which carries no media parameters: parameters other than `q` are ignored, a
/// more specific range wins, and equally specific ranges take the highest weight.
fn accepts_sse(headers: &HeaderMap) -> bool {
    if !headers.contains_key("accept") {
        return true;
    }
    let mut selected: Option<(u8, f32)> = None;
    for value in headers.get_all("accept") {
        let Ok(value) = value.to_str() else {
            return false;
        };
        for range in split_quoted(value, ',') {
            let mut parts = split_quoted(range, ';');
            let specificity = match parts
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "text/event-stream" => 2,
                "text/*" => 1,
                "*/*" => 0,
                _ => continue,
            };
            let quality = parts
                .filter_map(|parameter| parameter.split_once('='))
                .find(|(key, _)| key.trim().eq_ignore_ascii_case("q"))
                .map_or(1.0, |(_, weight)| {
                    weight
                        .trim()
                        .parse::<f32>()
                        .ok()
                        .filter(|q| (0.0..=1.0).contains(q))
                        .unwrap_or(0.0)
                });
            selected = Some(match selected {
                Some((s, q)) if s > specificity => (s, q),
                Some((s, q)) if s == specificity => (s, q.max(quality)),
                _ => (specificity, quality),
            });
        }
    }
    selected.is_some_and(|(_, quality)| quality > 0.0)
}

async fn events(
    State(state): State<BmcState>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, EventServiceError> {
    let state = enabled(&state)?;
    if uri.query().is_some() {
        return Err(EventServiceError::Invalid(
            "SSE query options are unsupported".into(),
        ));
    }
    if !accepts_sse(&headers) {
        return Ok(StatusCode::NOT_ACCEPTABLE.into_response());
    }
    let last_id = match headers.get("last-event-id").map(|v| v.to_str()) {
        Some(Err(_)) => {
            return Err(EventServiceError::Invalid(
                "invalid Last-Event-ID header".into(),
            ));
        }
        Some(Ok(id)) => Some(id),
        None => None,
    };
    let subscriber = state.subscribe(last_id)?;
    let body = Body::from_stream(stream::unfold(Some(subscriber), |subscriber| async {
        let mut subscriber = subscriber?;
        match subscriber.next().await {
            Some(Ok(bytes)) => Some((Ok(bytes), Some(subscriber))),
            Some(Err(error)) => Some((Err(error), None)),
            None => None,
        }
    }));
    let mut response = (
        [
            ("content-type", "text/event-stream"),
            ("cache-control", "no-cache"),
        ],
        body,
    )
        .into_response();
    // CombinedServer honors this bound; a bare router (in-process tests, other
    // embedders) serves the body without one.
    response
        .extensions_mut()
        .insert(OutputStallTimeout(state.config.limits.output_stall_timeout));
    Ok(response)
}

async fn subscriptions(State(state): State<BmcState>) -> Result<Response, EventServiceError> {
    let members: Vec<_> = enabled(&state)?
        .subscription_ids()
        .into_iter()
        .map(|id| event_destination::resource(id).entity_ref())
        .collect();
    Ok(Json(event_destination::collection().with_members(&members)).into_response())
}

fn subscription_id(id: &str) -> Result<u64, EventServiceError> {
    id.parse::<u64>()
        .ok()
        .filter(|parsed| parsed.to_string() == id)
        .ok_or(EventServiceError::NotFound)
}

async fn subscription(
    State(state): State<BmcState>,
    Path(id): Path<String>,
) -> Result<Response, EventServiceError> {
    let state = enabled(&state)?;
    let id = subscription_id(&id)?;
    let context = state
        .subscription_context(id)
        .ok_or(EventServiceError::NotFound)?;
    let document = event_destination::builder(&event_destination::resource(id))
        .context(&context)
        .sse()
        .build();
    Ok(Json(document).into_response())
}

async fn delete_subscription(
    State(state): State<BmcState>,
    Path(id): Path<String>,
) -> Result<Response, EventServiceError> {
    let state = enabled(&state)?;
    if !state.delete_subscription(subscription_id(&id)?) {
        return Err(EventServiceError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

struct RouterLease(Weak<EventServiceState>);
impl Drop for RouterLease {
    fn drop(&mut self) {
        if let Some(state) = self.0.upgrade() {
            state.close_on_drop();
        }
    }
}

/// Tie active streams to the router's lifetime. Only router clones own this
/// lease; response streams own the event state alone, so removing the final
/// router closes streams even when a test retains `BmcState` to inspect cleanup.
pub(crate) fn with_lifetime(router: Router, state: Option<&Arc<EventServiceState>>) -> Router {
    match state {
        Some(state) => router.layer(Extension(Arc::new(RouterLease(Arc::downgrade(state))))),
        None => router,
    }
}

/// Payload, state, and router fixtures shared by the EventService, SSE
/// delivery, and mock-control tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::response::Response;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::{EventServiceConfig, EventServiceLimits, EventServiceState};
    use crate::test_support::{NoopCallbacks, host_info};
    use crate::{BmcState, HardwareType, MachineRouterOptions, machine_router};

    pub(crate) fn event() -> Value {
        json!({"@odata.id": "/redfish/v1/EventService/SSE#/Event1",
            "@odata.type": "#Event.v1_6_0.Event", "Id": "1", "Name": "Test event",
            "Events": [{"@odata.id": "/redfish/v1/EventService/SSE#/Events/1",
                "MemberId": "1", "EventId": "application-id", "EventType": "Alert",
                "MessageId": "ResourceEvent.1.2.ResourceRemoved", "Message": "Resource removed",
                "EventTimestamp": "2026-09-10T12:00:00Z", "MessageSeverity": "OK"}]})
    }

    pub(crate) fn metric() -> Value {
        json!({"@odata.id": "/redfish/v1/TelemetryService/MetricReports/Power",
            "@odata.type": "#MetricReport.v1_3_0.MetricReport", "Id": "Power", "Name": "Power",
            "MetricReportDefinition": {"@odata.id": "/redfish/v1/TelemetryService/MetricReportDefinitions/Power"},
            "MetricValues": [{"MetricId": "Watts", "MetricValue": "100",
                "Timestamp": "2026-09-10T12:00:00Z",
                "MetricProperty": "/redfish/v1/Chassis/1/Power#/PowerControl/0/PowerConsumedWatts"}]})
    }

    /// Heartbeats off; other limits default.
    pub(crate) fn limits(
        max_subscribers: usize,
        max_frame_bytes: usize,
        max_frames: usize,
        max_history_bytes: usize,
    ) -> EventServiceConfig {
        EventServiceConfig::try_from(EventServiceLimits {
            max_subscribers,
            max_frame_bytes,
            max_frames,
            max_history_bytes,
            heartbeat: None,
            ..Default::default()
        })
        .unwrap()
    }

    pub(crate) fn state(frames: usize) -> Arc<EventServiceState> {
        EventServiceState::new(limits(2, 4096, frames, 8192))
    }

    /// A Dell R750 mock with a ten-millisecond outage window on reset.
    pub(crate) fn router(auth: bool) -> (Router, BmcState) {
        machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(NoopCallbacks),
            "sse-test".into(),
            auth,
            MachineRouterOptions {
                bmc_reset_duration: Some(Duration::from_millis(10)),
                ..Default::default()
            },
        )
    }

    pub(crate) async fn request(
        router: &Router,
        method: &str,
        uri: &str,
        payload: Option<Value>,
    ) -> Response {
        let body = payload
            .map(|v| Body::from(v.to_string()))
            .unwrap_or_default();
        tokio::time::timeout(
            Duration::from_secs(2),
            router.clone().oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(body)
                    .unwrap(),
            ),
        )
        .await
        .expect("response headers must not wait for EOF")
        .unwrap()
    }

    pub(crate) async fn json_body(response: Response) -> Value {
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode};
    use axum::response::Response;
    use bytes::Bytes;
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::fixtures::{event, json_body, limits, metric, request, router, state};
    use super::*;
    use crate::test_support::{NoopCallbacks, host_info, serve_https};
    use crate::{
        BmcEvent, BmcState, EventServiceOverride, HardwareType, MachineRouterOptions,
        machine_router,
    };

    #[test]
    fn configuration_rejects_unbounded_or_invalid_limits() {
        carbide_test_support::value_scenarios!(run = |limits: EventServiceLimits|
            EventServiceConfig::try_from(limits).is_ok();
            "resource limits" {
                EventServiceLimits { max_subscribers: 1, max_frame_bytes: 1, max_frames: 1,
                    max_history_bytes: 1, heartbeat: None, ..Default::default() } => true,
                EventServiceLimits { max_subscribers: 0, ..Default::default() } => false,
                EventServiceLimits { max_frame_bytes: 0, ..Default::default() } => false,
                EventServiceLimits { max_frames: 0, ..Default::default() } => false,
                EventServiceLimits { max_frame_bytes: 2, max_history_bytes: 1, ..Default::default() } => false,
            }
            "interval bounds" {
                EventServiceLimits { heartbeat: Some(Duration::ZERO), ..Default::default() } => false,
                EventServiceLimits { heartbeat: Some(Duration::from_secs(86_401)), ..Default::default() } => false,
                EventServiceLimits { output_stall_timeout: Duration::ZERO, ..Default::default() } => false,
            }
        );
    }

    #[test]
    fn poisoned_state_does_not_panic_in_destructors() {
        let (router, bmc) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(NoopCallbacks),
            "poison".into(),
            false,
            MachineRouterOptions::default(),
        );
        let state = bmc.event_service.as_ref().unwrap();
        let subscriber = state.subscribe(None).unwrap();
        let poisoner = state.clone();
        std::thread::spawn(move || {
            let _guard = poisoner.lock();
            panic!("injected panic under the event lock");
        })
        .join()
        .unwrap_err();

        // Neither destructor tries to recover the cross-field invariants.
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(subscriber))).is_ok()
        );
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(router))).is_ok());
        // Ordinary operations must still fail loudly on the poisoned subsystem.
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.stats())).is_err());
    }

    #[test]
    fn publication_validation_and_encoded_byte_limits() {
        let state = state(2);
        carbide_test_support::value_scenarios!(run = |value| {
            let rejected = state.publish(value).is_err();
            (rejected, state.stats().retained_frames)
        };
            "invalid publication leaves history unchanged" {
                json!({"@odata.type": "#LogEntry.v1_9_0.LogEntry"}) => (true, 0),
                json!({"@odata.type": "#Event.v1_6_0.Event"}) => (true, 0),
                json!({"padding": "x".repeat(4096)}) => (true, 0),
            }
        );
        assert!(state.publish(event()).unwrap().ends_with(":1"));
        assert!(state.publish(metric()).unwrap().ends_with(":2"));
        let encoded_size = state.stats().retained_bytes;
        let event_size = {
            let single = self::state(1);
            single.publish(event()).unwrap();
            single.stats().retained_bytes
        };
        assert!(encoded_size > event_size);
        let exact = EventServiceState::new(limits(1, event_size, 10, event_size));
        exact.publish(event()).unwrap();
        exact.publish(event()).unwrap();
        assert_eq!(
            exact.stats().retained_frames,
            1,
            "byte limit evicts independently of count"
        );
        assert_eq!(exact.stats().retained_bytes, event_size);
        let short = EventServiceState::new(limits(1, event_size - 1, 1, event_size));
        assert!(matches!(
            short.publish(event()),
            Err(EventServiceError::TooLarge)
        ));
    }

    #[test]
    fn script_limits_do_not_mutate_queue_on_rejection() {
        let state = state(1);
        carbide_test_support::value_scenarios!(run = |steps| {
            (state.queue_script(steps).is_err(), state.stats().queued_scripts)
        };
            "invalid scripts leave the queue unchanged" {
                vec![] => (true, 0),
                vec![StreamStep::Eof, StreamStep::Eof] => (true, 0),
                vec![StreamStep::Delay { millis: 60_001 }] => (true, 0),
                vec![StreamStep::Bytes { data: vec![0; MAX_SCRIPT_BYTES + 1] }] => (true, 0),
                vec![StreamStep::Delay { millis: 0 }; MAX_SCRIPT_STEPS + 1] => (true, 0),
            }
        );
        for _ in 0..MAX_SCRIPTS {
            state.queue_script(vec![StreamStep::Eof]).unwrap();
        }
        assert!(matches!(
            state.queue_script(vec![StreamStep::Eof]),
            Err(EventServiceError::Unavailable)
        ));
    }

    #[test]
    fn subscription_context_is_never_a_valid_cursor() {
        let state = state(4);
        state.publish(event()).unwrap();
        let live = state.subscribe(None).unwrap();
        let [id] = state.subscription_ids()[..] else {
            panic!("one subscription");
        };
        let context = state.subscription_context(id).unwrap();
        assert!(matches!(
            state.subscribe(Some(&context)),
            Err(EventServiceError::Invalid(_))
        ));
        drop(live);
        assert!(state.subscription_context(id).is_none());
    }

    #[test]
    fn accept_header_honors_media_ranges_and_weights() {
        carbide_test_support::value_scenarios!(run = |accept: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("accept", accept.parse().unwrap());
            accepts_sse(&headers)
        };
            "compatible ranges" {
                "*/*" => true,
                "text/*;q=0.5" => true,
                "text/event-stream;charset=utf-8" => true,
                "text/event-stream;version=2, */*" => true,
                "text/event-stream;" => true,
                // The served representation has no parameters, so only the bare range applies.
                "text/event-stream;charset=utf-8;q=0, text/event-stream" => true,
                r#"text/event-stream;profile="a;b,c", */*"# => true,
            }
            "explicit exclusion or invalid weight" {
                "application/json" => false,
                "text/event-stream;q=0, */*;q=1" => false,
                "text/event-stream;q=0, text/*" => false,
                "text/event-stream;q=NaN" => false,
                r#"application/json;profile="a,text/event-stream,b", text/event-stream;q=0"# => false,
            }
        );
    }

    fn router_with(limits: EventServiceConfig) -> (Router, BmcState) {
        machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(NoopCallbacks),
            "sse-limits-test".into(),
            false,
            MachineRouterOptions {
                event_service: EventServiceOverride::Limits(limits),
                ..Default::default()
            },
        )
    }

    async fn resume(router: &Router, last_event_id: &str) -> Response {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(SSE)
                    .header("last-event-id", last_event_id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn frame(body: &mut Body) -> Bytes {
        tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap()
    }

    /// The JSON document of one `data:` SSE frame.
    fn payload(frame: &Bytes) -> Value {
        let text = std::str::from_utf8(frame).unwrap();
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("frame carries a data line");
        serde_json::from_str(data).unwrap()
    }

    async fn manager_reset_target(router: &Router) -> String {
        let managers = json_body(request(router, "GET", "/redfish/v1/Managers", None).await).await;
        let manager_path = managers["Members"][0]["@odata.id"].as_str().unwrap();
        let manager = json_body(request(router, "GET", manager_path, None).await).await;
        manager["Actions"]["#Manager.Reset"]["target"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn hardware_profiles_control_discovery_and_routes() {
        check_cases_async(
            [
                Case {
                    scenario: "Dell R750 host profile",
                    input: (
                        HardwareType::DellPowerEdgeR750,
                        false,
                        EventServiceOverride::Profile,
                    ),
                    expect: Yields((true, true, StatusCode::OK)),
                },
                Case {
                    scenario: "BlueField-3 DPU profile",
                    input: (
                        HardwareType::DellPowerEdgeR750,
                        true,
                        EventServiceOverride::Profile,
                    ),
                    expect: Yields((true, true, StatusCode::OK)),
                },
                Case {
                    scenario: "BlueField-4 DPU profile",
                    input: (
                        HardwareType::DellPowerEdgeR760Bf4,
                        true,
                        EventServiceOverride::Profile,
                    ),
                    expect: Yields((true, true, StatusCode::OK)),
                },
                Case {
                    scenario: "power shelf profile",
                    input: (
                        HardwareType::DeltaPowerShelf,
                        false,
                        EventServiceOverride::Profile,
                    ),
                    expect: Yields((true, true, StatusCode::OK)),
                },
                Case {
                    scenario: "switch profile",
                    input: (
                        HardwareType::NvidiaSwitchNd5200Ld,
                        false,
                        EventServiceOverride::Profile,
                    ),
                    expect: Yields((true, true, StatusCode::OK)),
                },
                Case {
                    scenario: "generic profile with limit overrides",
                    input: (
                        HardwareType::GenericAmi,
                        false,
                        EventServiceOverride::Limits(EventServiceConfig::default()),
                    ),
                    expect: Yields((true, true, StatusCode::OK)),
                },
            ],
            |(hardware, dpu, event_service)| async move {
                let info = if dpu {
                    use crate::mac_address_pool::{Config, MacAddressPool, PoolConfig};
                    let mut pool = MacAddressPool::new(Config {
                        ranges: None,
                        pool: Some(
                            PoolConfig::new(mac_address::MacAddress::new([2, 0, 0, 0, 0, 0]), 16)
                                .unwrap(),
                        ),
                    });
                    crate::MachineInfo::Dpu(crate::DpuMachineInfo::new(
                        hardware,
                        &mut pool,
                        crate::machine_info::DpuSettings::default(),
                    ))
                } else {
                    host_info(hardware)
                };
                let (router, state) = machine_router(
                    &info,
                    Arc::new(NoopCallbacks),
                    "hardware-event-service".into(),
                    false,
                    MachineRouterOptions {
                        event_service,
                        ..Default::default()
                    },
                );
                let root = json_body(request(&router, "GET", "/redfish/v1", None).await).await;
                let response = request(&router, "GET", SSE, None).await;
                Ok::<_, std::convert::Infallible>((
                    state.event_service.is_some(),
                    root.get("EventService").is_some(),
                    response.status(),
                ))
            },
        )
        .await;
    }

    #[tokio::test]
    async fn unknown_subscriptions_return_redfish_errors() {
        let (router, _) = router(false);
        check_cases_async(
            [
                Case {
                    scenario: "missing member",
                    input: ("GET", "999999"),
                    expect: Yields((StatusCode::NOT_FOUND, true)),
                },
                Case {
                    scenario: "nonnumeric member",
                    input: ("GET", "garbage"),
                    expect: Yields((StatusCode::NOT_FOUND, true)),
                },
                Case {
                    scenario: "deleting missing member",
                    input: ("DELETE", "garbage"),
                    expect: Yields((StatusCode::NOT_FOUND, true)),
                },
            ],
            |(method, id)| {
                let router = router.clone();
                async move {
                    let response =
                        request(&router, method, &format!("{SUBSCRIPTIONS}/{id}"), None).await;
                    let status = response.status();
                    let error = json_body(response).await;
                    Ok::<_, std::convert::Infallible>((status, error["error"]["code"].is_string()))
                }
            },
        )
        .await;
    }

    #[tokio::test]
    async fn closing_unpolled_bodies_holds_admission_until_drop() {
        check_cases_async(
            [
                Case {
                    scenario: "explicit close",
                    input: "close",
                    expect: Yields(()),
                },
                Case {
                    scenario: "BMC reset",
                    input: "reset",
                    expect: Yields(()),
                },
                Case {
                    scenario: "subscription deletion",
                    input: "delete",
                    expect: Yields(()),
                },
            ],
            |operation| async move {
                let (router, bmc) = router_with(limits(1, 4096, 2, 8192));
                let state = bmc.event_service.as_ref().unwrap();
                state
                    .queue_script(vec![StreamStep::Bytes {
                        data: vec![0; MAX_SCRIPT_BYTES],
                    }])
                    .unwrap();
                let response = request(&router, "GET", SSE, None).await;
                assert_eq!(response.status(), StatusCode::OK);
                match operation {
                    "close" => state.close_subscribers(),
                    "reset" => {
                        bmc.reset();
                    }
                    "delete" => {
                        let member = json_body(request(&router, "GET", SUBSCRIPTIONS, None).await)
                            .await["Members"][0]["@odata.id"]
                            .as_str()
                            .unwrap()
                            .to_owned();
                        assert_eq!(
                            request(&router, "DELETE", &member, None).await.status(),
                            StatusCode::NO_CONTENT
                        );
                    }
                    _ => unreachable!(),
                }
                // Removing the registry entry releases the unsent script, but the
                // held body keeps its admission slot.
                assert_eq!((state.stats().subscribers, state.stats().streams), (0, 1));
                assert_eq!(
                    request(&router, "GET", SSE, None).await.status(),
                    StatusCode::SERVICE_UNAVAILABLE
                );
                drop(response);
                assert_eq!(state.stats().streams, 0);
                assert_eq!(
                    request(&router, "GET", SSE, None).await.status(),
                    StatusCode::OK
                );
                Ok::<_, std::convert::Infallible>(())
            },
        )
        .await;
    }

    #[tokio::test]
    async fn stale_cursor_precedes_capacity_and_replay_leaves_scripts_queued() {
        let (router, bmc) = router_with(limits(1, 4096, 2, 8192));
        let state = bmc.event_service.as_ref().unwrap();
        let old_id = state.publish(event()).unwrap();
        let held = request(&router, "GET", SSE, None).await;
        bmc.reset();
        assert_eq!(
            resume(&router, &old_id).await.status(),
            StatusCode::BAD_REQUEST,
            "a stale cursor is reported even while the slot is held"
        );
        let id = state.publish(event()).unwrap();
        assert_eq!(
            resume(&router, &id).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        state.queue_script(vec![StreamStep::Eof]).unwrap();
        assert_eq!(
            request(&router, "GET", SSE, None).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            state.stats().queued_scripts,
            1,
            "rejected opens claim nothing"
        );
        drop(held);
        let replay = resume(&router, &id).await;
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(
            state.stats().queued_scripts,
            1,
            "replay never claims a script"
        );
        drop(replay);
        let response = request(&router, "GET", SSE, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            state.stats().queued_scripts,
            0,
            "the next live-only open claims it"
        );
    }

    #[tokio::test]
    async fn ipmi_cold_reset_closes_stream_and_invalidates_replay() {
        let (router, bmc) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(NoopCallbacks),
            "ipmi-reset".into(),
            false,
            MachineRouterOptions {
                bmc_reset_duration: Some(Duration::from_millis(100)),
                ..Default::default()
            },
        );
        let state = bmc.event_service.as_ref().unwrap();
        let old_id = state.publish(event()).unwrap();
        let old_generation = state.stats().generation;
        let mut body = request(&router, "GET", SSE, None).await.into_body();
        state.queue_script(vec![StreamStep::Eof]).unwrap();
        let response = request(
            &router,
            "POST",
            "/ipmi",
            Some(json!({"action":"bmc_cold_reset"})),
        )
        .await;
        assert_eq!(json_body(response).await["success"], true);
        assert!(body.frame().await.is_none());
        let stats = state.stats();
        assert_ne!(stats.generation, old_generation);
        assert_eq!(
            (
                stats.subscribers,
                stats.streams,
                stats.retained_frames,
                stats.queued_scripts
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(
            request(&router, "GET", SSE, None).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            resume(&router, &old_id).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(&router, "GET", SSE, None).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn http2_stalled_reader_cannot_bypass_admission_after_close() {
        use hyper_util::rt::{TokioExecutor, TokioIo};
        tokio::time::timeout(Duration::from_secs(5), async {
            let (router, bmc) = router_with(limits(1, 4096, 2, 8192));
            let state = bmc.event_service.as_ref().unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let socket = tokio::net::TcpStream::connect(address).await.unwrap();
            let (mut client, connection) =
                hyper::client::conn::http2::Builder::new(TokioExecutor::new())
                    .initial_stream_window_size(1024)
                    .handshake::<_, Body>(TokioIo::new(socket))
                    .await
                    .unwrap();
            let connection = tokio::spawn(connection);
            let open = || {
                Request::builder()
                    .uri(format!("http://{address}{SSE}"))
                    .body(Body::empty())
                    .unwrap()
            };
            state
                .queue_script(vec![
                    StreamStep::Bytes {
                        data: vec![b'x'; 16 * 1024]
                    };
                    64
                ])
                .unwrap();
            let mut stalled = client.send_request(open()).await.unwrap();
            assert_eq!(stalled.status(), StatusCode::OK);
            // One prefix proves the server handed a frame to Hyper. The rest of
            // that frame exceeds the receive window, so Hyper cannot poll the next.
            let prefix = stalled
                .body_mut()
                .frame()
                .await
                .unwrap()
                .unwrap()
                .into_data()
                .unwrap();
            assert!(prefix.len() <= 1024);
            state.close_subscribers();
            assert_eq!((state.stats().subscribers, state.stats().streams), (0, 1));
            for _ in 0..32 {
                assert_eq!(
                    client.send_request(open()).await.unwrap().status(),
                    StatusCode::SERVICE_UNAVAILABLE
                );
                state.close_subscribers();
            }
            drop(stalled);
            while state.stats().streams != 0 {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                client.send_request(open()).await.unwrap().status(),
                StatusCode::OK
            );
            drop(client);
            connection.abort();
            server.abort();
        })
        .await
        .expect("stalled HTTP/2 admission test must finish promptly");
    }

    #[tokio::test]
    async fn router_controls_discovery_subscription_lifetime_and_isolation() {
        let (router, bmc) = router(false);
        let events = bmc.event_service.as_ref().unwrap();
        bmc.injection.put(vec![crate::injection::Rule {
            id: "sse-json-rule".into(),
            selector: crate::injection::Selector::OdataId(SSE.into()),
            action: crate::injection::Action::JsonMerge(json!({"must_not_apply": true})),
            remaining: Some(1),
        }]);
        let (other_router, other) = self::router(false);
        assert_eq!(
            json_body(request(&router, "GET", "/redfish/v1", None).await).await["EventService"]["@odata.id"],
            ROOT
        );
        let response = request(&router, "GET", SSE, None).await;
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(response.headers()["cache-control"], "no-cache");
        assert!(!response.headers().contains_key("content-length"));
        let mut body = response.into_body();
        let id =
            json_body(request(&router, "POST", "/Mock/EventService/events", Some(event())).await)
                .await["id"]
                .as_str()
                .unwrap()
                .to_string();
        assert!(
            frame(&mut body)
                .await
                .starts_with(format!("id: {id}\n").as_bytes())
        );
        assert_eq!(bmc.injection.list()[0].remaining, Some(1));
        assert_eq!(
            other
                .event_service
                .as_ref()
                .unwrap()
                .stats()
                .retained_frames,
            0
        );
        assert_eq!(
            request(&router, "GET", "/redfish/v1/Systems", None)
                .await
                .status(),
            StatusCode::OK
        );
        let subscriptions = json_body(request(&router, "GET", SUBSCRIPTIONS, None).await).await;
        assert_eq!(subscriptions["Members@odata.count"], 1);
        let path = subscriptions["Members"][0]["@odata.id"].as_str().unwrap();
        assert_eq!(
            json_body(request(&router, "GET", path, None).await).await["SubscriptionType"],
            "SSE"
        );
        assert_eq!(
            request(&router, "DELETE", path, None).await.status(),
            StatusCode::NO_CONTENT
        );
        assert!(body.frame().await.is_none());
        assert_eq!(
            request(&router, "GET", path, None).await.status(),
            StatusCode::NOT_FOUND
        );
        let mut body = request(&router, "GET", SSE, None).await.into_body();
        let _other_body = request(&other_router, "GET", SSE, None).await.into_body();
        drop(router);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), body.frame())
                .await
                .unwrap()
                .is_none(),
            "router removal must close its streams"
        );
        assert_eq!(events.stats().subscribers, 0);
        assert_eq!(other.event_service.as_ref().unwrap().stats().subscribers, 1);
    }

    #[tokio::test]
    async fn admission_failures_leave_scripts_unclaimed_and_replay_preserves_ids() {
        let (router, bmc) = router(false);
        let state = bmc.event_service.as_ref().unwrap();
        assert_eq!(
            request(
                &router,
                "POST",
                "/Mock/EventService/scripts",
                Some(json!([{"kind": "eof"}]))
            )
            .await
            .status(),
            StatusCode::NO_CONTENT
        );
        let head = request(&router, "HEAD", SSE, None).await;
        assert_eq!(head.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(
            !head.headers().contains_key("content-type"),
            "HEAD errors stay bodiless"
        );
        assert_eq!(
            state.stats().queued_scripts,
            1,
            "HEAD must not claim an SSE script"
        );
        check_cases_async(
            [
                Case {
                    scenario: "unsupported query",
                    input: (format!("{SSE}?$expand=*"), "text/event-stream"),
                    expect: Yields(StatusCode::BAD_REQUEST),
                },
                Case {
                    scenario: "incompatible media type",
                    input: (SSE.into(), "application/json"),
                    expect: Yields(StatusCode::NOT_ACCEPTABLE),
                },
                Case {
                    scenario: "explicit exclusion overrides wildcard",
                    input: (SSE.into(), "text/event-stream;q=0, */*;q=1"),
                    expect: Yields(StatusCode::NOT_ACCEPTABLE),
                },
            ],
            |(uri, accept)| {
                let router = router.clone();
                async move {
                    let response = router
                        .oneshot(
                            Request::builder()
                                .uri(uri)
                                .header("accept", accept)
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    let status = response.status();
                    assert_eq!(response.headers()["content-type"], "application/json");
                    assert!(json_body(response).await["error"]["code"].is_string());
                    assert_eq!(state.stats().queued_scripts, 1);
                    assert_eq!(state.stats().subscribers, 0);
                    Ok::<_, std::convert::Infallible>(status)
                }
            },
        )
        .await;
        let mut empty = request(&router, "GET", SSE, None).await.into_body();
        assert!(empty.frame().await.is_none());
        let first = state.publish(event()).unwrap();
        let second = state.publish(metric()).unwrap();
        let response = resume(&router, &first).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            frame(&mut response.into_body())
                .await
                .starts_with(format!("id: {second}\n").as_bytes())
        );
    }

    #[tokio::test]
    async fn https_typed_stream_auth_reset_and_shutdown() {
        use futures::StreamExt;
        use nv_redfish::bmc_http::reqwest::{Client, ClientParams};
        use nv_redfish::bmc_http::{BmcCredentials, CacheSettings, HttpBmc, HttpClient};

        tokio::time::timeout(Duration::from_secs(10), async {
            let (router, bmc) = router(true);
            bmc.account_service_state
                .change_factory_default_password("test-password");
            let state = bmc.event_service.as_ref().unwrap();
            let (mut server, base) = serve_https("sse-https-test", router);
            let params = || ClientParams {
                accept_invalid_certs: true,
                timeout: Some(Duration::from_secs(2)),
                ..Default::default()
            };
            let client = Client::with_params(params()).unwrap();
            let credentials = BmcCredentials::new("root".into(), "test-password".into());
            let headers = HeaderMap::new();
            state.queue_script(vec![StreamStep::Eof]).unwrap();
            let wrong = BmcCredentials::new("root".into(), "wrong".into());
            assert!(
                client
                    .sse::<Value>(base.join(SSE).unwrap(), &wrong, &headers)
                    .await
                    .is_err()
            );
            assert!(
                client
                    .post::<_, Value>(
                        base.join("/Mock/EventService/events").unwrap(),
                        &event(),
                        &wrong,
                        &headers
                    )
                    .await
                    .is_err()
            );
            assert_eq!(state.stats().queued_scripts, 1);
            let mut eof = client
                .sse::<Value>(base.join(SSE).unwrap(), &credentials, &headers)
                .await
                .unwrap();
            assert!(eof.next().await.is_none());

            let root = nv_redfish::ServiceRoot::new(Arc::new(HttpBmc::new(
                Client::with_params(params()).unwrap(),
                base.clone(),
                credentials.clone(),
                CacheSettings::with_capacity(32),
            )))
            .await
            .unwrap();
            let service = root
                .event_service()
                .await
                .unwrap()
                .expect("discover enabled EventService");
            let mut stream = service.events().await.unwrap();
            assert_eq!(state.stats().subscribers, 1);
            state.publish(event()).unwrap();
            state.publish(metric()).unwrap();
            assert!(matches!(
                stream.next().await.unwrap().unwrap(),
                EventStreamPayload::Event(_)
            ));
            assert!(matches!(
                stream.next().await.unwrap().unwrap(),
                EventStreamPayload::MetricReport(_)
            ));
            let _: Value = client
                .get(
                    base.join("/redfish/v1/Systems").unwrap(),
                    &credentials,
                    None,
                    &headers,
                )
                .await
                .unwrap();

            let session = client
                .post_session::<_, Value>(
                    base.join("/redfish/v1/SessionService/Sessions").unwrap(),
                    &json!({"UserName": "root", "Password": "test-password"}),
                    &headers,
                )
                .await
                .unwrap();
            let token = BmcCredentials::token(session.auth_token);
            let mut session_stream = client
                .sse::<Value>(base.join(SSE).unwrap(), &token, &headers)
                .await
                .unwrap();
            let _ = client
                .delete::<Value>(
                    base.join(&session.location.to_string()).unwrap(),
                    &credentials,
                    &headers,
                )
                .await
                .unwrap();
            assert!(
                client
                    .sse::<Value>(base.join(SSE).unwrap(), &token, &headers)
                    .await
                    .is_err()
            );
            state.publish(event()).unwrap();
            assert_eq!(
                session_stream.next().await.unwrap().unwrap(),
                event(),
                "existing stream is authenticated at open"
            );
            drop(session_stream);
            // Wait for the server to observe cancellation, without assuming a fixed scheduling delay.
            while state.stats().subscribers != 1 {
                tokio::task::yield_now().await;
            }
            bmc.reset();
            // A frame sent before reset may already be in the transport buffer.
            while let Some(item) = stream.next().await {
                item.unwrap();
            }
            assert_eq!(state.stats().subscribers, 0);
            assert_eq!(state.stats().retained_frames, 0);
            while bmc.availability.as_ref().unwrap().is_offline() {
                tokio::task::yield_now().await;
            }
            let mut stream = service.events().await.unwrap();
            server.stop().await.unwrap();
            match stream.next().await {
                None | Some(Err(_)) => {}
                Some(Ok(_)) => panic!("shutdown must terminate the stream"),
            }
            while state.stats().subscribers != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("HTTPS stream lifecycle must complete promptly");
    }

    #[tokio::test]
    async fn https_fault_script_exercises_real_parser() {
        use futures::StreamExt;
        use nv_redfish::bmc_http::reqwest::{Client, ClientParams};
        use nv_redfish::bmc_http::{BmcCredentials, HttpClient};

        tokio::time::timeout(Duration::from_secs(5), async {
            let (router, bmc) = router(false);
            let state = bmc.event_service.as_ref().unwrap();
            let (mut server, base) = serve_https("sse-parser-test", router);
            let client = Client::with_params(ClientParams {
                accept_invalid_certs: true,
                ..Default::default()
            })
            .unwrap();
            let credentials = BmcCredentials::new("root".into(), "unused".into());
            let headers = HeaderMap::new();
            state
                .queue_script(vec![
                    StreamStep::Bytes {
                        data: b": comment\r\ndata: {\r\ndata: \"text\":\"caf\xc3".to_vec(),
                    },
                    StreamStep::Delay { millis: 1 },
                    StreamStep::Bytes {
                        data: b"\xa9\"}\r\n\r\n".to_vec(),
                    },
                    StreamStep::Delay { millis: 1 },
                    StreamStep::Bytes {
                        data: b"data: invalid-json\n\n".to_vec(),
                    },
                    StreamStep::Eof,
                ])
                .unwrap();
            let mut stream = client
                .sse::<Value>(base.join(SSE).unwrap(), &credentials, &headers)
                .await
                .unwrap();
            assert_eq!(
                stream.next().await.unwrap().unwrap(),
                json!({"text": "café"})
            );
            assert!(stream.next().await.unwrap().is_err());
            drop(stream);
            server.stop().await.unwrap();
            assert_eq!(
                state.stats().retained_frames,
                0,
                "raw scripts never enter replay"
            );
        })
        .await
        .expect("fault script must complete promptly");
    }

    #[tokio::test]
    async fn https_stalled_http2_transport_releases_admission_without_client_drop() {
        use hyper_util::rt::{TokioExecutor, TokioIo};
        use nv_redfish::bmc_http::reqwest::{Client, ClientParams};
        use nv_redfish::bmc_http::{BmcCredentials, HttpClient};
        use rustls::pki_types::ServerName;

        tokio::time::timeout(Duration::from_secs(5), async {
            let (router, bmc) = router_with(
                EventServiceConfig::try_from(EventServiceLimits {
                    max_subscribers: 1,
                    max_frame_bytes: 4096,
                    max_frames: 2,
                    max_history_bytes: 8192,
                    heartbeat: None,
                    output_stall_timeout: Duration::from_millis(500),
                })
                .unwrap(),
            );
            let state = bmc.event_service.as_ref().unwrap();
            let (mut server, base) = serve_https("sse-stall-test", router);
            let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::aws_lc_rs::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(
                forge_tls::dummy_tls_verifier::DummyTlsVerifier::new_for_tests(),
            ))
            .with_no_client_auth();
            config.alpn_protocols = vec![b"h2".to_vec()];
            let socket = tokio::net::TcpStream::connect(server.address)
                .await
                .unwrap();
            let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
                .connect(ServerName::try_from("localhost").unwrap(), socket)
                .await
                .unwrap();
            let (mut client, connection) =
                hyper::client::conn::http2::Builder::new(TokioExecutor::new())
                    .initial_stream_window_size(1024)
                    .handshake::<_, Body>(TokioIo::new(tls))
                    .await
                    .unwrap();
            let connection = tokio::spawn(connection);
            state
                .queue_script(vec![
                    StreamStep::Bytes {
                        data: vec![b'x'; 16 * 1024]
                    };
                    64
                ])
                .unwrap();
            let url = base.join(SSE).unwrap();
            let mut stalled = client
                .send_request(
                    Request::builder()
                        .uri(url.as_str())
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(stalled.status(), StatusCode::OK);
            assert_eq!(state.stats().streams, 1);
            // One prefix proves the server handed a frame to Hyper; the rest of
            // that frame exceeds the receive window and is never read.
            let prefix = stalled
                .body_mut()
                .frame()
                .await
                .unwrap()
                .unwrap()
                .into_data()
                .unwrap();
            assert!(prefix.len() <= 1024);
            state.close_subscribers();
            // Only the server's output-stall timeout can release this slot: the
            // client still owns the unpolled response.
            while state.stats().streams != 0 {
                tokio::task::yield_now().await;
            }
            let fresh = Client::with_params(ClientParams {
                accept_invalid_certs: true,
                ..Default::default()
            })
            .unwrap();
            let reopened = fresh
                .sse::<Value>(
                    url,
                    &BmcCredentials::new("root".into(), "unused".into()),
                    &Default::default(),
                )
                .await
                .unwrap();
            assert_eq!(state.stats().streams, 1);
            drop(reopened);
            drop(stalled);
            connection.abort();
            server.stop().await.unwrap();
        })
        .await
        .expect("transport timeout must free the stalled subscriber slot");
    }

    #[tokio::test]
    async fn manager_reset_without_outage_still_closes_sse() {
        let (router, bmc) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(NoopCallbacks),
            "reset-test".into(),
            false,
            MachineRouterOptions::default(),
        );
        let state = bmc.event_service.as_ref().unwrap();
        let entries = "/redfish/v1/Systems/System.Embedded.1/LogServices/EventLog/Entries";
        let before = json_body(request(&router, "GET", entries, None).await).await;
        let mut body = request(&router, "GET", SSE, None).await.into_body();
        state.publish(event()).unwrap();
        frame(&mut body).await;
        let reset = manager_reset_target(&router).await;
        assert_eq!(
            request(
                &router,
                "POST",
                &reset,
                Some(json!({"ResetType": "ForceRestart"}))
            )
            .await
            .status(),
            StatusCode::OK
        );
        // The reset is logged but not announced: the stream closes first.
        assert!(body.frame().await.is_none());
        assert_eq!(state.stats().retained_frames, 0);
        let after = json_body(request(&router, "GET", entries, None).await).await;
        assert_eq!(
            after["Members@odata.count"].as_u64().unwrap(),
            before["Members@odata.count"].as_u64().unwrap() + 1
        );
        let logged = after["Members"].as_array().unwrap().last().unwrap();
        assert_eq!(logged["Severity"], "Warning");
        assert!(
            logged["Message"].as_str().unwrap().contains("is resetting"),
            "{logged}"
        );
        assert_eq!(
            request(&router, "GET", ROOT, None).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn lifecycle_actions_record_log_entries_and_publish_events() {
        let (router, bmc) = router(false);
        let system = "/redfish/v1/Systems/System.Embedded.1";
        let entries = format!("{system}/LogServices/EventLog/Entries");
        async fn count(router: &Router, entries: &str) -> u64 {
            json_body(request(router, "GET", entries, None).await).await["Members@odata.count"]
                .as_u64()
                .unwrap()
        }
        let before = count(&router, &entries).await;
        let mut body = request(&router, "GET", SSE, None).await.into_body();

        // An accepted reset action is logged and announced with the LogEntry as origin.
        assert_eq!(
            request(
                &router,
                "POST",
                &format!("{system}/Actions/ComputerSystem.Reset"),
                Some(json!({"ResetType": "ForceRestart"}))
            )
            .await
            .status(),
            StatusCode::OK
        );
        let record = payload(&frame(&mut body).await)["Events"][0].clone();
        assert_eq!(
            record["MessageId"],
            "ResourceEvent.1.3.ResourceStateChanged"
        );
        assert_eq!(record["MessageSeverity"], "OK");
        let origin = record["OriginOfCondition"]["@odata.id"].as_str().unwrap();
        assert!(origin.starts_with(&format!("{entries}/")), "{origin}");
        let entry = json_body(request(&router, "GET", origin, None).await).await;
        assert_eq!(entry["Message"], record["Message"]);
        assert_eq!(entry["MessageId"], record["MessageId"]);
        assert_eq!(entry["Links"]["OriginOfCondition"]["@odata.id"], system);

        // Power events reported by the embedder flow the same way.
        bmc.on_event(&BmcEvent::PowerOn);
        assert_eq!(
            payload(&frame(&mut body).await)["Events"][0]["MessageId"],
            "ResourceEvent.1.3.ResourcePoweredOn"
        );
        assert_eq!(count(&router, &entries).await, before + 2);

        // A profile without a log service still publishes, pointing at the system.
        let (bare, bare_bmc) = machine_router(
            &host_info(HardwareType::GenericAmi),
            Arc::new(NoopCallbacks),
            "bare-log".into(),
            false,
            MachineRouterOptions::default(),
        );
        let mut bare_body = request(&bare, "GET", SSE, None).await.into_body();
        bare_bmc.on_event(&BmcEvent::BootCompleted);
        let origin =
            payload(&frame(&mut bare_body).await)["Events"][0]["OriginOfCondition"]["@odata.id"]
                .as_str()
                .unwrap()
                .to_owned();
        assert!(origin.starts_with("/redfish/v1/Systems/"), "{origin}");
        assert_eq!(
            request(&bare, "GET", &origin, None).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            request(&bare, "GET", &format!("{origin}/LogServices"), None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn https_authority_removal_closes_only_its_bmc_stream() {
        use std::collections::HashMap;

        use futures::StreamExt;
        use nv_redfish::bmc_http::reqwest::{Client, ClientParams};
        use nv_redfish::bmc_http::{BmcCredentials, HttpClient};
        use tokio::sync::RwLock;

        tokio::time::timeout(Duration::from_secs(5), async {
            let (router_a, a) = router(false);
            let (router_b, b) = router(false);
            let routers = Arc::new(RwLock::new(HashMap::from([
                ("bmc-a".into(), router_a),
                ("bmc-b".into(), router_b),
            ])));
            let (mut server, base) = serve_https(
                "sse-authority-test",
                crate::combined_router(routers.clone()),
            );
            let client = Client::with_params(ClientParams {
                accept_invalid_certs: true,
                ..Default::default()
            })
            .unwrap();
            let credentials = BmcCredentials::new("root".into(), "unused".into());
            let mut headers_a = HeaderMap::new();
            headers_a.insert("forwarded", "host=bmc-a".parse().unwrap());
            let mut headers_b = HeaderMap::new();
            headers_b.insert("forwarded", "host=bmc-b".parse().unwrap());
            let mut stream_a = client
                .sse::<Value>(base.join(SSE).unwrap(), &credentials, &headers_a)
                .await
                .unwrap();
            let mut stream_b = client
                .sse::<Value>(base.join(SSE).unwrap(), &credentials, &headers_b)
                .await
                .unwrap();
            let _ = client
                .post::<_, Value>(
                    base.join("/Mock/EventService/events").unwrap(),
                    &event(),
                    &credentials,
                    &headers_a,
                )
                .await
                .unwrap();
            assert_eq!(stream_a.next().await.unwrap().unwrap(), event());
            let stats_b: Value = client
                .get(
                    base.join("/Mock/EventService/stats").unwrap(),
                    &credentials,
                    None,
                    &headers_b,
                )
                .await
                .unwrap();
            assert_eq!(stats_b["retained_frames"], 0);
            routers.write().await.remove("bmc-a");
            assert!(stream_a.next().await.is_none());
            assert_eq!(a.event_service.as_ref().unwrap().stats().subscribers, 0);
            assert_eq!(b.event_service.as_ref().unwrap().stats().subscribers, 1);
            b.event_service.as_ref().unwrap().publish(metric()).unwrap();
            assert_eq!(stream_b.next().await.unwrap().unwrap(), metric());
            let _ = client
                .post::<_, Value>(
                    base.join("/Mock/EventService/close").unwrap(),
                    &json!({}),
                    &credentials,
                    &headers_b,
                )
                .await
                .unwrap();
            assert!(stream_b.next().await.is_none());
            server.stop().await.unwrap();
        })
        .await
        .expect("authority removal must terminate only the selected BMC");
    }
}
