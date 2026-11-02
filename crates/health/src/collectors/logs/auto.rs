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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use nv_redfish::core::ODataId;

use super::DowngradeReason;
use crate::HealthError;
use crate::config::AutoModeConfig;
use crate::sink::{CollectorEvent, DataSink, EventContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
    SseNotAvailable,
    Transient,
}

impl FailureKind {
    pub(crate) fn classify(err: &HealthError) -> Self {
        match err {
            HealthError::SseNotAvailable(_) => Self::SseNotAvailable,
            _ => Self::Transient,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetDecision {
    Continue,
    Downgrade(DowngradeReason),
}

#[derive(Debug)]
pub(crate) struct AutoFailureBudget {
    cfg: AutoModeConfig,
    sse_not_available_count: u32,
    window_start: Option<Instant>,
    window_failure_count: u32,
}

impl AutoFailureBudget {
    pub(crate) fn new(cfg: AutoModeConfig) -> Self {
        Self {
            cfg,
            sse_not_available_count: 0,
            window_start: None,
            window_failure_count: 0,
        }
    }

    pub(crate) fn record(&mut self, kind: FailureKind, now: Instant) -> BudgetDecision {
        match kind {
            FailureKind::SseNotAvailable => {
                self.sse_not_available_count = self.sse_not_available_count.saturating_add(1);
                if self.sse_not_available_count >= self.cfg.sse_not_available_threshold {
                    BudgetDecision::Downgrade(DowngradeReason::SseNotAvailable)
                } else {
                    BudgetDecision::Continue
                }
            }
            FailureKind::Transient => {
                let outage_started_at = *self.window_start.get_or_insert(now);

                self.window_failure_count = self.window_failure_count.saturating_add(1);

                let threshold_reached =
                    self.window_failure_count >= self.cfg.connect_failure_threshold;

                let outage_sustained = now.saturating_duration_since(outage_started_at)
                    >= self.cfg.connect_failure_window;

                if threshold_reached && outage_sustained {
                    BudgetDecision::Downgrade(DowngradeReason::ConnectFailureBudgetExhausted)
                } else {
                    BudgetDecision::Continue
                }
            }
        }
    }

    pub(crate) fn record_stream_end(
        &mut self,
        connected_for: Duration,
        now: Instant,
    ) -> BudgetDecision {
        if connected_for >= self.cfg.connect_failure_window {
            self.window_start = None;
            self.window_failure_count = 0;
        }

        self.record(FailureKind::Transient, now)
    }
}

/// Sink wrapper that records the latest SSE log-entry cursor accepted by its sink.
pub(crate) struct SseCursorSink {
    inner: Arc<dyn DataSink>,
    last_seen_ids: DashMap<ODataId, i32>,
}

impl SseCursorSink {
    pub(crate) fn new(inner: Arc<dyn DataSink>) -> Self {
        Self {
            inner,
            last_seen_ids: DashMap::new(),
        }
    }

    pub(crate) fn snapshot(&self) -> HashMap<ODataId, i32> {
        self.last_seen_ids
            .iter()
            .map(|entry| (entry.key().clone(), *entry.value()))
            .collect()
    }
}

impl DataSink for SseCursorSink {
    fn sink_type(&self) -> &'static str {
        self.inner.sink_type()
    }

    fn try_handle_event(
        &self,
        context: &EventContext,
        event: &CollectorEvent,
    ) -> Result<(), HealthError> {
        self.inner.try_handle_event(context, event)?;

        let CollectorEvent::Log(record) = event else {
            return Ok(());
        };

        let Some(log_entry_id) = record
            .attributes
            .iter()
            .find(|(key, _)| key.as_ref() == "log_entry_id")
            .map(|(_, value)| value.as_str())
        else {
            return Ok(());
        };

        let Some((service_id, entry_id)) = log_entry_id.rsplit_once("/Entries/") else {
            return Ok(());
        };

        if let Ok(entry_id) = entry_id.parse::<i32>() {
            self.last_seen_ids
                .insert(ODataId::from(service_id.to_string()), entry_id);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::endpoint::test_support::{mac, test_endpoint};
    use crate::sink::{LogRecord, LogSeverity};

    struct TestSink {
        fail: bool,
        calls: AtomicUsize,
    }

    impl DataSink for TestSink {
        fn sink_type(&self) -> &'static str {
            "test_sink"
        }

        fn try_handle_event(
            &self,
            _context: &EventContext,
            _event: &CollectorEvent,
        ) -> Result<(), HealthError> {
            self.calls.fetch_add(1, Ordering::SeqCst);

            if self.fail {
                Err(HealthError::GenericError("sink failed".to_string()))
            } else {
                Ok(())
            }
        }
    }

    fn cfg_with(sse_threshold: u32, window: Duration, transient_threshold: u32) -> AutoModeConfig {
        AutoModeConfig {
            sse_not_available_threshold: sse_threshold,
            connect_failure_window: window,
            connect_failure_threshold: transient_threshold,
            ..AutoModeConfig::default()
        }
    }

    #[test]
    fn test_classify_sse_not_available() {
        let err = HealthError::SseNotAvailable("no EventService".to_string());
        assert_eq!(FailureKind::classify(&err), FailureKind::SseNotAvailable);
    }

    #[test]
    fn test_classify_other_errors_are_transient() {
        let err = HealthError::HttpError("500 Internal".to_string());
        assert_eq!(FailureKind::classify(&err), FailureKind::Transient);
        let err = HealthError::GenericError("tls handshake".to_string());
        assert_eq!(FailureKind::classify(&err), FailureKind::Transient);
    }

    #[test]
    fn test_sse_not_available_downgrades_at_threshold() {
        let now = Instant::now();
        let mut budget = AutoFailureBudget::new(cfg_with(2, Duration::from_secs(60), 10));

        assert_eq!(
            budget.record(FailureKind::SseNotAvailable, now),
            BudgetDecision::Continue
        );
        assert_eq!(
            budget.record(FailureKind::SseNotAvailable, now),
            BudgetDecision::Downgrade(DowngradeReason::SseNotAvailable)
        );
    }

    #[test]
    fn test_sse_not_available_default_threshold_is_one() {
        let now = Instant::now();
        let mut budget = AutoFailureBudget::new(AutoModeConfig::default());

        assert_eq!(
            budget.record(FailureKind::SseNotAvailable, now),
            BudgetDecision::Downgrade(DowngradeReason::SseNotAvailable)
        );
    }

    #[test]
    fn transient_failure_count_does_not_downgrade_before_outage_window() {
        let start = Instant::now();
        let mut budget = AutoFailureBudget::new(cfg_with(10, Duration::from_secs(60), 3));

        assert_eq!(
            budget.record(FailureKind::Transient, start),
            BudgetDecision::Continue
        );
        assert_eq!(
            budget.record(FailureKind::Transient, start + Duration::from_secs(10)),
            BudgetDecision::Continue
        );
        assert_eq!(
            budget.record(FailureKind::Transient, start + Duration::from_secs(20)),
            BudgetDecision::Continue
        );
    }

    #[test]
    fn sustained_transient_outage_downgrades_after_count_and_window() {
        let start = Instant::now();
        let mut budget = AutoFailureBudget::new(cfg_with(10, Duration::from_secs(60), 3));

        assert_eq!(
            budget.record(FailureKind::Transient, start),
            BudgetDecision::Continue
        );

        assert_eq!(
            budget.record(FailureKind::Transient, start + Duration::from_secs(60)),
            BudgetDecision::Continue
        );

        assert_eq!(
            budget.record(FailureKind::Transient, start + Duration::from_secs(61)),
            BudgetDecision::Downgrade(DowngradeReason::ConnectFailureBudgetExhausted)
        );
    }

    #[test]
    fn stream_that_survives_window_starts_a_new_transient_outage() {
        let start = Instant::now();
        let mut budget = AutoFailureBudget::new(cfg_with(10, Duration::from_secs(60), 2));

        assert_eq!(
            budget.record(FailureKind::Transient, start),
            BudgetDecision::Continue
        );

        assert_eq!(
            budget.record(FailureKind::Transient, start + Duration::from_secs(1)),
            BudgetDecision::Continue
        );

        assert_eq!(
            budget.record_stream_end(Duration::from_secs(60), start + Duration::from_secs(6)),
            BudgetDecision::Continue
        );

        assert_eq!(
            budget.record(FailureKind::Transient, start + Duration::from_secs(66)),
            BudgetDecision::Downgrade(DowngradeReason::ConnectFailureBudgetExhausted)
        );
    }

    #[test]
    fn test_sse_not_available_counter_is_independent_of_transient_window() {
        let start = Instant::now();
        let mut budget = AutoFailureBudget::new(cfg_with(2, Duration::from_secs(60), 10));

        assert_eq!(
            budget.record(FailureKind::SseNotAvailable, start),
            BudgetDecision::Continue
        );

        assert_eq!(
            budget.record_stream_end(Duration::from_secs(60), start + Duration::from_secs(1)),
            BudgetDecision::Continue
        );

        assert_eq!(
            budget.record(FailureKind::SseNotAvailable, start + Duration::from_secs(2)),
            BudgetDecision::Downgrade(DowngradeReason::SseNotAvailable)
        );
    }

    #[test]
    fn short_stream_terminations_exhaust_the_transient_budget() {
        let start = Instant::now();
        let mut budget = AutoFailureBudget::new(cfg_with(10, Duration::from_secs(60), 3));

        assert_eq!(
            budget.record_stream_end(Duration::from_secs(1), start),
            BudgetDecision::Continue
        );

        assert_eq!(
            budget.record_stream_end(Duration::from_secs(1), start + Duration::from_secs(30)),
            BudgetDecision::Continue
        );

        assert_eq!(
            budget.record_stream_end(Duration::from_secs(1), start + Duration::from_secs(60)),
            BudgetDecision::Downgrade(DowngradeReason::ConnectFailureBudgetExhausted)
        );
    }

    #[test]
    fn cursor_sink_advances_only_after_successful_dispatch() {
        let inner = Arc::new(TestSink {
            fail: false,
            calls: AtomicUsize::new(0),
        });

        let sink = SseCursorSink::new(inner.clone());

        let context =
            EventContext::from_endpoint(&test_endpoint(mac("00:11:22:33:44:55")), "test_collector");

        let service_id = "/redfish/v1/Systems/1/LogServices/EventLog";

        let event = CollectorEvent::Log(Box::new(LogRecord {
            body: "event".to_string(),
            severity: LogSeverity::Info,
            attributes: vec![(
                Cow::Borrowed("log_entry_id"),
                format!("{service_id}/Entries/42"),
            )],
            diagnostic_record: None,
        }));

        sink.try_handle_event(&context, &event)
            .expect("counting sink should accept the event");

        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);

        assert_eq!(
            sink.snapshot().get(&ODataId::from(service_id.to_string())),
            Some(&42)
        );

        let failing_sink = SseCursorSink::new(Arc::new(TestSink {
            fail: true,
            calls: AtomicUsize::new(0),
        }));

        assert!(failing_sink.try_handle_event(&context, &event).is_err());
        assert!(failing_sink.snapshot().is_empty());
    }
}
