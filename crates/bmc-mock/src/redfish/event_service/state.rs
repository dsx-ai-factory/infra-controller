// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Event configuration, publication, bounded history, and subscriber admission.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use nv_redfish::event_service::EventStreamPayload;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;

use super::stream::{Delivery, Subscriber};
use super::{
    MAX_QUEUED_BYTES, MAX_SCRIPT_BYTES, MAX_SCRIPT_DELAY_MS, MAX_SCRIPT_STEPS, MAX_SCRIPTS,
};
use crate::redfish::session_service::generate_token;

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
    pub(super) limits: EventServiceLimits,
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

/// One raw fault-stream operation. Claimed scripts run once per connection,
/// bypass normal event history, and are cancelled when their response is dropped.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamStep {
    /// Send exact bytes, without SSE validation; JSON encodes bytes as integers.
    Bytes {
        /// Raw response bytes.
        data: Vec<u8>,
    },
    /// Wait this many milliseconds; a whole script may delay at most 60 seconds.
    Delay {
        /// Delay in milliseconds.
        millis: u64,
    },
    /// End the response cleanly. Must be the last step.
    Eof,
    /// End the response with an I/O error. Must be the last step.
    Error,
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
pub(super) enum ScriptStep {
    /// The subscription was closed or deleted.
    Closed,
    /// The script has no remaining steps: clean EOF.
    Finished,
    Step(StreamStep),
}

/// Outcome of asking for a live subscriber's next frame.
pub(super) enum LiveFrame {
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
    pub(super) config: EventServiceConfig,
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
    pub(super) fn unsubscribe_on_drop(&self, id: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.subscribers.remove(&id);
            inner.streams = inner.streams.saturating_sub(1);
        }
    }

    pub(super) fn close_on_drop(&self) {
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
    pub(super) fn subscription_ids(&self) -> Vec<u64> {
        self.lock().subscribers.keys().copied().collect()
    }

    /// Opaque Context of an active subscription. Its shape differs from frame
    /// IDs so echoing it as a `Last-Event-ID` is rejected.
    pub(super) fn subscription_context(&self, id: u64) -> Option<String> {
        let inner = self.lock();
        inner
            .subscribers
            .contains_key(&id)
            .then(|| format!("subscription:{}:{id}", inner.generation))
    }

    /// End one subscription. Its response body keeps the admission slot until dropped.
    pub(super) fn delete_subscription(&self, id: u64) -> bool {
        let mut inner = self.lock();
        if inner.subscribers.remove(&id).is_none() {
            return false;
        }
        inner.closed = inner.closed.saturating_add(1);
        self.changed.send_replace(());
        true
    }

    pub(super) fn is_subscribed(&self, id: u64) -> bool {
        self.lock().subscribers.contains_key(&id)
    }

    pub(super) fn pop_script_step(&self, id: u64) -> ScriptStep {
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

    pub(super) fn next_frame(&self, id: u64, next_seq: u64) -> LiveFrame {
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
    pub(super) fn subscribe(
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
                    .filter(|seq| {
                        inner.frames.front().is_some_and(|f| seq + 1 >= f.seq)
                            && *seq < inner.next_seq
                    })
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::fixtures::{event, limits, metric, state};
    use super::*;
    use crate::test_support::{NoopCallbacks, host_info};
    use crate::{HardwareType, MachineRouterOptions, machine_router};

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

    use serde_json::json;
}
