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

use std::sync::Arc;
use std::time::Instant;

use super::{CollectorEvent, DataSink, EventContext};
use crate::HealthError;
use crate::metrics::{ComponentKind, ComponentMetrics, MetricsManager};

pub struct CompositeDataSink {
    sinks: Vec<Arc<dyn DataSink>>,
    component_metrics: Arc<ComponentMetrics>,
    share_metrics: bool,
}

impl CompositeDataSink {
    pub fn new(sinks: Vec<Arc<dyn DataSink>>, metrics_manager: Arc<MetricsManager>) -> Self {
        let share_metrics = sinks
            .iter()
            .filter(|sink| sink.accepts_shared_metric())
            .nth(1)
            .is_some();

        Self {
            sinks,
            component_metrics: metrics_manager.component_metrics(),
            share_metrics,
        }
    }

    fn record_sink_operation(
        &self,
        sink: &dyn DataSink,
        duration: std::time::Duration,
        success: bool,
    ) {
        self.component_metrics.record_operation(
            ComponentKind::Sink,
            sink.sink_type(),
            duration,
            success,
        );
    }
}

impl DataSink for CompositeDataSink {
    fn sink_type(&self) -> &'static str {
        "composite_sink"
    }

    /// Fans the event out to every sink, recording each sink's duration and
    /// outcome. A failing sink never blocks the others: its error is fully
    /// reported here (the sink logs its own detail, the composite meters the
    /// failure), so the fanout itself always succeeds.
    fn try_handle_event(
        &self,
        context: &EventContext,
        event: &CollectorEvent,
    ) -> Result<(), HealthError> {
        let sample = match event {
            CollectorEvent::Metric(sample) if self.share_metrics => Some(sample.as_ref()),
            _ => None,
        };

        let mut shared_metric = None;

        for sink in &self.sinks {
            let start = Instant::now();

            let result = if let Some(sample) = sample.filter(|_| sink.accepts_shared_metric()) {
                // Charge the first copy to the first target's timed dispatch.
                // Later targets retain the same immutable observation.
                let shared_metric = shared_metric
                    .get_or_insert_with(|| Arc::new((context.clone(), (*sample).clone())));

                sink.try_handle_shared_metric(context, event, shared_metric)
            } else {
                sink.try_handle_event(context, event)
            };

            self.record_sink_operation(sink.as_ref(), start.elapsed(), result.is_ok());
        }
        Ok(())
    }
}
