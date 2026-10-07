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

//! Per-object state progress metrics (`docs/design/per-object-state-metrics.md`).
//!
//! The generic state processor records every processed object's current
//! state, resolved SLA, and manual-intervention status as per-object gauges,
//! intended to be served from a dedicated, opt-in metrics endpoint. All
//! series carry `object_type`, `object_id`, `state`, `substate` labels using
//! the existing `metric_state_names` vocabulary, so they join with the
//! aggregate per-state metrics.

use std::sync::Arc;
use std::time::Duration;

use carbide_health_metrics::{PerObjectGauge, PerObjectMetricsRegistry};
use chrono::{DateTime, Utc};

/// The object's manual-intervention status as determined by one iteration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualIntervention {
    /// Manual operator action is required; the token must come from a closed
    /// set — never free text.
    Required(&'static str),
    NotRequired,
    /// This iteration could not determine the status (e.g. a transient
    /// handler error): an existing series is kept alive, not cleared, so a
    /// stuck object doesn't flap out of alerts on one bad iteration.
    Unknown,
}

/// The per-object state gauges, shared by all state controllers. Cheap to
/// clone (every field is `Arc`-backed).
#[derive(Clone, Debug)]
pub struct PerObjectStateMetrics {
    registry: Arc<PerObjectMetricsRegistry>,
    entered: PerObjectGauge,
    sla: PerObjectGauge,
    manual_intervention: PerObjectGauge,
}

impl PerObjectStateMetrics {
    /// Creates the state gauges as native collectors on `prometheus_registry`
    /// (the dedicated per-object endpoint's registry). `hold_period` must
    /// cover the refresh interval of the slowest feeding controller.
    pub fn new(
        registry: &Arc<PerObjectMetricsRegistry>,
        prometheus_registry: &prometheus::Registry,
        hold_period: Duration,
    ) -> prometheus::Result<Self> {
        const STATE_LABELS: &[&str] = &["object_type", "object_id", "state", "substate"];
        Ok(Self {
            registry: registry.clone(),
            entered: registry.gauge(
                prometheus_registry,
                "carbide_object_state_entered_timestamp_seconds",
                "Unix time at which the object entered its current state, taken from the \
                 state's version timestamp. The labels expose the current state as a join \
                 key; state age is time() minus this value.",
                STATE_LABELS,
                hold_period,
            )?,
            sla: registry.gauge(
                prometheus_registry,
                "carbide_object_state_sla_seconds",
                "The object's resolved SLA for its current state. Emitted only for states \
                 that have an SLA; 0 means the object should never be in this state.",
                STATE_LABELS,
                hold_period,
            )?,
            manual_intervention: registry.gauge(
                prometheus_registry,
                "carbide_object_manual_intervention_required",
                "1 while the object requires manual operator action, labeled with a stable \
                 reason token. The series exists only while this is true.",
                &["object_type", "object_id", "state", "substate", "reason"],
                hold_period,
            )?,
        })
    }
}

/// [`PerObjectStateMetrics`] bound to one controller's object type
/// (`machine`, `switch`, ...), as threaded into the controller builder. All
/// writes go through this type so the object type used for series lifecycle
/// and labels can never diverge.
#[derive(Clone, Debug)]
pub struct PerObjectStateRecorder {
    object_type: &'static str,
    metrics: PerObjectStateMetrics,
}

impl PerObjectStateRecorder {
    pub fn new(object_type: &'static str, metrics: PerObjectStateMetrics) -> Self {
        Self {
            object_type,
            metrics,
        }
    }

    /// Records the object's state as of this iteration, replacing all of its
    /// series. `sla == None` (state without an SLA) clears the SLA series.
    pub fn record(
        &self,
        object_id: &str,
        state: &'static str,
        substate: &'static str,
        entered: DateTime<Utc>,
        sla: Option<Duration>,
        manual_intervention: ManualIntervention,
    ) {
        let object_type = self.object_type;
        // Label values in STATE_LABELS order; the gauges apply the names at
        // collection time.
        let label_values = || {
            vec![
                object_type.to_string(),
                object_id.to_string(),
                state.to_string(),
                substate.to_string(),
            ]
        };
        self.metrics.entered.set(
            object_type,
            object_id,
            entered.timestamp() as f64,
            label_values(),
        );
        match sla {
            Some(sla) => {
                self.metrics
                    .sla
                    .set(object_type, object_id, sla.as_secs_f64(), label_values())
            }
            None => self.metrics.sla.clear(object_type, object_id),
        }
        match manual_intervention {
            ManualIntervention::Required(reason) => {
                let mut label_values = label_values();
                label_values.push(reason.to_string());
                self.metrics
                    .manual_intervention
                    .set(object_type, object_id, 1.0, label_values);
            }
            ManualIntervention::NotRequired => self
                .metrics
                .manual_intervention
                .clear(object_type, object_id),
            // Keep the series alive only while it still describes the state
            // the object is in — after an out-of-band state change the kept
            // fact would contradict the entered/sla series just recorded.
            ManualIntervention::Unknown => self.metrics.manual_intervention.touch_if_values(
                object_type,
                object_id,
                &label_values(),
            ),
        }
    }

    /// Refreshes the eviction deadline of all of the object's state/info
    /// series without changing them, for iterations that could not determine
    /// the object's state at all (e.g. a load failure): mid-incident triage
    /// series — including the info series joins depend on, whose recording
    /// handler never ran — must not evict just because the database is
    /// degraded.
    pub fn touch(&self, object_id: &str) {
        self.metrics
            .registry
            .touch_object(self.object_type, object_id);
    }

    /// Removes all of the object's series across every per-object gauge
    /// (state, info, associations, classification), e.g. when the object was
    /// deleted.
    pub fn clear(&self, object_id: &str) {
        self.metrics
            .registry
            .clear_object(self.object_type, object_id);
    }
}
