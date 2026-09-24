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
}

impl CompositeDataSink {
    pub fn new(sinks: Vec<Arc<dyn DataSink>>, metrics_manager: Arc<MetricsManager>) -> Self {
        Self {
            sinks,
            component_metrics: metrics_manager.component_metrics(),
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

    fn prune_metrics(
        &self,
        context: &EventContext,
        metric_type: Option<&str>,
        labels: &[crate::metrics::MetricLabel],
        unit: Option<&str>,
        label_names: Option<&[&str]>,
    ) {
        for sink in &self.sinks {
            sink.prune_metrics(context, metric_type, labels, unit, label_names);
        }
    }

    fn prune_metric_key(&self, context: &EventContext, key: &str, metric_type: &str, unit: &str) {
        for sink in &self.sinks {
            sink.prune_metric_key(context, key, metric_type, unit);
        }
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
        for sink in &self.sinks {
            let start = Instant::now();
            let result = sink.try_handle_event(context, event);
            self.record_sink_operation(sink.as_ref(), start.elapsed(), result.is_ok());
        }
        Ok(())
    }
}
