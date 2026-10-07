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

use carbide_health_metrics::{HealthIterationMetrics, HealthObjectMetrics, register_health_gauges};
use carbide_utils::metrics::SharedMetricsHolder;
use opentelemetry::metrics::Meter;
use state_controller::metrics::MetricsEmitter;

#[derive(Debug, Default)]
pub struct SwitchMetrics {
    pub health: HealthObjectMetrics,
}

#[derive(Debug, Default)]
pub struct SwitchStateControllerIterationMetrics {
    pub health: HealthIterationMetrics<()>,
}

#[derive(Debug)]
pub struct SwitchMetricsEmitter {}

impl MetricsEmitter for SwitchMetricsEmitter {
    type ObjectMetrics = SwitchMetrics;
    type IterationMetrics = SwitchStateControllerIterationMetrics;

    fn new(
        _object_type: &str,
        meter: &Meter,
        shared_metrics: SharedMetricsHolder<Self::IterationMetrics>,
    ) -> Self {
        register_health_gauges::<_, (), _>(
            "carbide_switches",
            "switch_id",
            "switches",
            meter,
            shared_metrics,
            |m| &m.health,
        );
        Self {}
    }

    fn merge_object_handling_metrics(
        iteration_metrics: &mut Self::IterationMetrics,
        object_metrics: &Self::ObjectMetrics,
    ) {
        iteration_metrics.health.merge((), &object_metrics.health);
    }

    fn emit_object_counters_and_histograms(&self, _object_metrics: &Self::ObjectMetrics) {}
}
