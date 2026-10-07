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

//! Streaming layer of `tracing` log events for the web UI log viewer.
//!
//! [`LogStreamLayer`] is installed in the subscriber registry (see
//! [`crate::logging::setup`]) and forwards to a bounded broadcast channel (plus
//! a small replay ring buffer):
//!   * every `tracing` event, tagged with the `span_id` of its enclosing span, and
//!   * a `SPAN` summary line when each span closes, carrying the span's
//!     accumulated fields and how long it was open.
//!
//! The span summaries are where request outcomes live (URL, gRPC status + error
//! message, SQL counts, timing), mirroring what the `logfmt` formatter writes to
//! stdout on span close.
//!
//! Backpressure is intentional: the broadcast channel is bounded, so a slow or
//! paused viewer falls behind and observes a `Lagged` notification (surfaced to
//! the UI as dropped lines) rather than ever blocking the logging hot path; we
//! are fine dropping/losing lines in this case.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// Number of lines buffered in the broadcast channel. A subscriber that falls
/// this far behind observes a `Lagged` notification and skips ahead rather than
/// blocking the sender.
const BROADCAST_CAPACITY: usize = 1024;

/// Default in-memory byte budget for retained history, used when no config is
/// supplied. Overridden by `CarbideConfig.log_history.max_megabytes`. 128 MiB.
const DEFAULT_MAX_BYTES: usize = 128 * 1024 * 1024;

/// A single structured log line, serialized to the browser as JSON. Produced
/// both for `tracing` events and for span completions (`level == "SPAN"`).
#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    /// Monotonic per-process sequence number, assigned at publish time.
    /// Used as the cursor for scrollback pagination in the viewer.
    pub seq: u64,
    /// RFC 3339 timestamp captured when the line was observed.
    pub timestamp: String,
    /// Log level, e.g. `"INFO"`, or `"SPAN"` for a span-completion summary.
    pub level: &'static str,
    /// Event target (module path / target string).
    pub target: String,
    /// The event's `message`, or the span name for a `SPAN` line.
    pub message: String,
    /// Remaining structured key/value fields, ordered by key.
    pub fields: BTreeMap<String, String>,
    /// Source location `"file:line"`, if available.
    pub location: Option<String>,
    /// `span_id` of the enclosing span (for events) or of the span itself (for
    /// `SPAN` lines), when the span carries one. Drives correlation and
    /// click-to-filter (by span ID) in the viewer.
    pub span_id: Option<String>,
}

/// Shared handle to the live log stream: a broadcast sender plus a ring buffer
/// of recent lines. Cheap to clone fwiw; everything inside is an `Arc`/`Sender`.
#[derive(Debug, Clone)]
pub struct LogStream {
    tx: broadcast::Sender<Arc<LogLine>>,
    recent: Arc<Mutex<Recent>>,
    /// Byte budget for `recent`; the oldest lines are evicted past this.
    max_bytes: usize,
    /// Source of monotonic `LogLine::seq` values.
    next_seq: Arc<AtomicU64>,
}

/// The retained-history ring plus a running byte total, kept together under one
/// lock so eviction stays consistent.
#[derive(Debug, Default)]
struct Recent {
    lines: VecDeque<Arc<LogLine>>,
    bytes: usize,
}

impl LogStream {
    pub fn new(broadcast_capacity: usize, max_bytes: usize) -> Self {
        let (tx, _rx) = broadcast::channel(broadcast_capacity);
        Self {
            tx,
            recent: Arc::new(Mutex::new(Recent::default())),
            max_bytes,
            next_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Construct with the default broadcast capacity and the given history byte
    /// budget (from `CarbideConfig.log_history.max_megabytes`).
    pub fn with_max_bytes(max_bytes: usize) -> Self {
        Self::new(BROADCAST_CAPACITY, max_bytes)
    }

    /// Subscribe to lines published from this point forward.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<LogLine>> {
        self.tx.subscribe()
    }

    /// The newest `limit` lines, oldest-first, for replay when a viewer connects.
    pub fn latest(&self, limit: usize) -> Vec<Arc<LogLine>> {
        match self.recent.lock() {
            Ok(r) => {
                let start = r.lines.len().saturating_sub(limit);
                r.lines.iter().skip(start).cloned().collect()
            }
            Err(_) => Vec::new(),
        }
    }

    /// Up to `limit` lines with `seq < before`, oldest-first — one page of
    /// scrollback history older than the cursor the viewer already holds.
    pub fn history(&self, before: u64, limit: usize) -> Vec<Arc<LogLine>> {
        match self.recent.lock() {
            Ok(r) => {
                let end = r
                    .lines
                    .iter()
                    .position(|line| line.seq >= before)
                    .unwrap_or(r.lines.len());
                let start = end.saturating_sub(limit);
                r.lines
                    .iter()
                    .skip(start)
                    .take(end - start)
                    .cloned()
                    .collect()
            }
            Err(_) => Vec::new(),
        }
    }

    /// Record a line: assign its sequence number, append to the ring buffer
    /// (evicting the oldest lines once over the byte budget), and broadcast it
    /// to live subscribers.
    fn publish(&self, mut line: LogLine) {
        line.seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let size = line_size(&line);
        let line = Arc::new(line);
        if let Ok(mut r) = self.recent.lock() {
            r.lines.push_back(Arc::clone(&line));
            r.bytes += size;
            // Evict oldest until back under budget, but always keep at least the
            // newest line (so one oversized line can't empty the buffer).
            while r.bytes > self.max_bytes && r.lines.len() > 1 {
                if let Some(old) = r.lines.pop_front() {
                    r.bytes = r.bytes.saturating_sub(line_size(&old));
                }
            }
        }
        // `send` errors when there are no subscribers, so we just
        // ignore the error.
        let _ = self.tx.send(line);
    }
}

impl Default for LogStream {
    fn default() -> Self {
        Self::new(BROADCAST_CAPACITY, DEFAULT_MAX_BYTES)
    }
}

/// Per-span data accumulated while a span is open, so we can emit a summary line
/// (with its fields + duration) when the span closes, and surface its `span_id`
/// on the events logged inside it.
struct SpanData {
    fields: BTreeMap<String, String>,
    opened_at: Instant,
}

/// A `tracing` layer that forwards events and span-completion
/// summaries to a [`LogStream`].
pub struct LogStreamLayer {
    stream: LogStream,
}

impl LogStreamLayer {
    pub fn new(stream: LogStream) -> Self {
        Self { stream }
    }
}

impl<S> Layer<S> for LogStreamLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = LogLineVisitor::default();
        attrs.record(&mut visitor);
        span.extensions_mut().insert(SpanData {
            fields: visitor.fields,
            opened_at: Instant::now(),
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = LogLineVisitor::default();
        values.record(&mut visitor);
        let mut extensions = span.extensions_mut();
        if let Some(data) = extensions.get_mut::<SpanData>() {
            data.fields.extend(visitor.fields);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();

        let mut visitor = LogLineVisitor::default();
        event.record(&mut visitor);

        self.stream.publish(LogLine {
            seq: 0, // real value assigned in publish()
            timestamp: now_rfc3339(),
            level: meta.level().as_str(),
            target: meta.target().to_string(),
            message: visitor.message.unwrap_or_default(),
            fields: visitor.fields,
            location: location_of(meta),
            span_id: enclosing_span_id(&ctx, event),
        });
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };

        // The span is closing and this layer is the only reader of its
        // `SpanData`, so take the data out of the extensions rather than
        // cloning the whole field map.
        let Some(data) = span.extensions_mut().remove::<SpanData>() else {
            return;
        };
        let SpanData {
            mut fields,
            opened_at,
        } = data;
        let elapsed_ms = opened_at.elapsed().as_millis();

        // Put `span_id` to its own slot (for span click-to-filter)
        // and record how long the span was open.
        let span_id = fields.remove("span_id");
        fields.insert("elapsed_ms".to_string(), elapsed_ms.to_string());

        let meta = span.metadata();
        self.stream.publish(LogLine {
            seq: 0, // real value assigned in publish()
            timestamp: now_rfc3339(),
            level: "SPAN",
            target: meta.target().to_string(),
            message: meta.name().to_string(),
            fields,
            location: location_of(meta),
            span_id,
        });
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn location_of(meta: &tracing::Metadata<'_>) -> Option<String> {
    match (meta.file(), meta.line()) {
        (Some(file), Some(line)) => Some(format!("{file}:{line}")),
        (Some(file), None) => Some(file.to_string()),
        _ => None,
    }
}

/// Rough in-memory footprint of a line, for the history byte budget: the text it
/// carries plus a small fixed overhead per line and per field (Arc, struct, and
/// map-node bookkeeping).
fn line_size(line: &LogLine) -> usize {
    const PER_LINE_OVERHEAD: usize = 64;
    const PER_FIELD_OVERHEAD: usize = 16;
    let mut bytes = PER_LINE_OVERHEAD
        + line.level.len()
        + line.timestamp.len()
        + line.target.len()
        + line.message.len()
        + line.location.as_deref().map_or(0, str::len)
        + line.span_id.as_deref().map_or(0, str::len);
    for (key, value) in &line.fields {
        bytes += PER_FIELD_OVERHEAD + key.len() + value.len();
    }
    bytes
}

/// Walk from the event's span up through its ancestors and
/// return the first `span_id` field found.
fn enclosing_span_id<S>(ctx: &Context<'_, S>, event: &Event<'_>) -> Option<String>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let span = ctx.event_span(event)?;
    span.scope().find_map(|s| {
        s.extensions()
            .get::<SpanData>()
            .and_then(|data| data.fields.get("span_id").cloned())
    })
}

/// Collects a `message` and remaining fields from an event or span,
/// mirroring the field handling in the crate's logfmt layer (the `message`
/// field is special-cased; span attributes have no message and leave
/// it `None`).
#[derive(Default)]
struct LogLineVisitor {
    message: Option<String>,
    fields: BTreeMap<String, String>,
}

impl LogLineVisitor {
    fn insert(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = Some(value);
        } else {
            self.fields.insert(field.name().to_string(), value);
        }
    }
}

impl Visit for LogLineVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.insert(field, format!("{value:?}"));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, value.to_string());
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.insert(field, value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::prelude::*;

    use super::*;

    #[test]
    fn captures_event_level_target_message_and_fields() {
        let stream = LogStream::new(16, 64 * 1024);
        let mut rx = stream.subscribe();
        let subscriber = tracing_subscriber::registry().with(LogStreamLayer::new(stream.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "test_target", answer = 42, "hello world");
        });

        let line = rx.try_recv().expect("a line should have been broadcast");
        assert_eq!(line.level, "INFO");
        assert_eq!(line.target, "test_target");
        assert_eq!(line.message, "hello world");
        assert_eq!(line.fields.get("answer").map(String::as_str), Some("42"));
        assert_eq!(line.span_id, None);
        assert_eq!(line.seq, 0);

        // The same line is retained for replay-on-connect.
        let recent = stream.latest(10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].message, "hello world");
    }

    #[test]
    fn ring_evicts_oldest_when_over_byte_budget() {
        // A budget small enough to hold only a handful of the lines below.
        let stream = LogStream::new(64, 512);
        let subscriber = tracing_subscriber::registry().with(LogStreamLayer::new(stream.clone()));

        tracing::subscriber::with_default(subscriber, || {
            for i in 0..50 {
                tracing::info!(target: "t", "line {i}");
            }
        });

        let recent = stream.latest(1000);
        assert!(
            !recent.is_empty(),
            "buffer should retain the most recent lines"
        );
        assert!(
            recent.len() < 50,
            "older lines should have been evicted past the byte budget"
        );
        // Newest line kept, oldest evicted.
        assert_eq!(recent.last().unwrap().message, "line 49");
        assert_ne!(recent.first().unwrap().message, "line 0");
        // Sequence numbers are contiguous and increasing.
        let seqs: Vec<u64> = recent.iter().map(|l| l.seq).collect();
        assert!(
            seqs.windows(2).all(|w| w[1] == w[0] + 1),
            "retained seqs should be contiguous"
        );
    }

    #[test]
    fn carries_span_id_on_events_and_emits_span_summary() {
        let stream = LogStream::new(32, 64 * 1024);
        let mut rx = stream.subscribe();
        let subscriber = tracing_subscriber::registry().with(LogStreamLayer::new(stream));

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request", span_id = "0xabc", http_url = "/x");
            let _enter = span.enter();
            tracing::info!("inside span");
        });

        // The event inside the span carries the span's id.
        let event_line = rx.try_recv().expect("event line");
        assert_eq!(event_line.message, "inside span");
        assert_eq!(event_line.span_id.as_deref(), Some("0xabc"));
        assert_eq!(event_line.seq, 0);

        // Closing the span produces a SPAN summary line carrying its fields.
        let span_line = rx.try_recv().expect("span summary line");
        assert_eq!(span_line.level, "SPAN");
        assert_eq!(span_line.message, "request");
        assert_eq!(span_line.span_id.as_deref(), Some("0xabc"));
        assert_eq!(span_line.seq, 1);
        assert_eq!(
            span_line.fields.get("http_url").map(String::as_str),
            Some("/x")
        );
        assert!(span_line.fields.contains_key("elapsed_ms"));
        assert!(!span_line.fields.contains_key("span_id"));
    }

    #[test]
    fn paginates_history_by_seq() {
        let stream = LogStream::new(64, 64 * 1024);
        let subscriber = tracing_subscriber::registry().with(LogStreamLayer::new(stream.clone()));

        tracing::subscriber::with_default(subscriber, || {
            for i in 0..10 {
                tracing::info!(target: "t", "line {i}");
            }
        });

        // Newest page.
        let latest = stream.latest(3);
        assert_eq!(latest.len(), 3);
        assert_eq!(latest.first().unwrap().message, "line 7");
        assert_eq!(latest.last().unwrap().message, "line 9");

        // The page just older than the oldest line we already hold.
        let cursor = latest.first().unwrap().seq;
        let older = stream.history(cursor, 3);
        assert_eq!(older.len(), 3);
        assert_eq!(older.first().unwrap().message, "line 4");
        assert_eq!(older.last().unwrap().message, "line 6");
        assert!(older.iter().all(|line| line.seq < cursor));
    }
}
