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

//! Defines custom metrics that are collected and emitted by the Machine State Controller

use ::carbide_utils::metrics::SharedMetricsHolder;
use opentelemetry::metrics::Meter;
use state_controller::metrics::MetricsEmitter;

#[derive(Debug, Default, Clone)]
pub struct AttestationMetrics {}

#[derive(Debug, Default)]
pub struct AttestationStateControllerIterationMetrics {}

#[derive(Debug)]
pub struct SpdmMetricsEmitter {}

impl AttestationStateControllerIterationMetrics {}

impl MetricsEmitter for SpdmMetricsEmitter {
    type ObjectMetrics = AttestationMetrics;
    type IterationMetrics = AttestationStateControllerIterationMetrics;

    fn new(
        _object_type: &str,
        _meter: &Meter,
        _shared_metrics: SharedMetricsHolder<Self::IterationMetrics>,
    ) -> Self {
        Self {}
    }

    // This routine is called in the context of a single thread.
    // The statecontroller launches multiple threads (upto max_concurrency)
    // Each thread works on one object and records the metrics for that object.
    // Once all the tasks are done, the original thread calls merge object_handling_metrics.
    // No need for mutex when manipulating the seg_stats HashMap.
    fn merge_object_handling_metrics(
        _iteration_metrics: &mut Self::IterationMetrics,
        _object_metrics: &Self::ObjectMetrics,
    ) {
    }

    fn emit_object_counters_and_histograms(&self, _object_metrics: &Self::ObjectMetrics) {}
}
