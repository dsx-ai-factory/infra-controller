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

use std::collections::HashMap;

use carbide_utils::metrics::SharedMetricsHolder;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Meter;
use state_controller::metrics::MetricsEmitter;

#[derive(Debug, Default, Clone)]
pub struct NetworkSegmentMetrics {
    // These are the stats for a particular segment
    pub available_ips: usize,
    pub reserved_ips: usize,
    pub total_ips: usize,
    // These are the attributes of that segment
    pub seg_name: String,
    pub prefix: String,
    pub seg_type: String,
    pub seg_id: String,
}

#[derive(Debug, Default)]
pub struct NetworkSegmentStateControllerIterationMetrics {
    // Hash key is segment uuid string; value is the metrics of that segment
    seg_stats: HashMap<String, NetworkSegmentMetrics>,
}

#[derive(Debug)]
pub struct NetworkSegmentMetricsEmitter {}

impl NetworkSegmentStateControllerIterationMetrics {}

impl MetricsEmitter for NetworkSegmentMetricsEmitter {
    type ObjectMetrics = NetworkSegmentMetrics;
    type IterationMetrics = NetworkSegmentStateControllerIterationMetrics;

    fn new(
        _object_type: &str,
        meter: &Meter,
        shared_metrics: SharedMetricsHolder<Self::IterationMetrics>,
    ) -> Self {
        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_available_ips_count")
                .with_description("Number of available IPs per network segment")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (_seg_id, seg_stats) in metrics.seg_stats.clone() {
                            observer.observe(
                                seg_stats.available_ips as u64,
                                &[
                                    attrs,
                                    &[
                                        KeyValue::new("name", seg_stats.seg_name),
                                        KeyValue::new("type", seg_stats.seg_type),
                                        KeyValue::new("prefix", seg_stats.prefix),
                                    ],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build()
        };

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_reserved_ips_count")
                .with_description("Number of reserved IPs per network segment")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (_seg_id, seg_stats) in metrics.seg_stats.clone() {
                            observer.observe(
                                seg_stats.reserved_ips as u64,
                                &[
                                    attrs,
                                    &[
                                        KeyValue::new("name", seg_stats.seg_name),
                                        KeyValue::new("type", seg_stats.seg_type),
                                        KeyValue::new("prefix", seg_stats.prefix),
                                    ],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build()
        };

        {
            let metrics = shared_metrics;
            meter
                .u64_observable_gauge("carbide_total_ips_count")
                .with_description("Number of IPs per network segment")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (_seg_id, seg_stats) in metrics.seg_stats.clone() {
                            observer.observe(
                                seg_stats.total_ips as u64,
                                &[
                                    attrs,
                                    &[
                                        KeyValue::new("name", seg_stats.seg_name),
                                        KeyValue::new("type", seg_stats.seg_type),
                                        KeyValue::new("prefix", seg_stats.prefix),
                                    ],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build()
        };

        Self {}
    }

    // This routine is called in the context of a single thread.
    // The statecontroller launches multiple threads (upto max_concurrency)
    // Each thread works on one object and records the metrics for that object.
    // Once all the tasks are done, the original thread calls merge object_handling_metrics.
    // No need for mutex when manipulating the seg_stats HashMap.
    fn merge_object_handling_metrics(
        iteration_metrics: &mut Self::IterationMetrics,
        object_metrics: &Self::ObjectMetrics,
    ) {
        let this_seg_id = object_metrics.seg_id.clone();
        if this_seg_id.is_empty() {
            // If the segment state is not READY, the metrics would not
            // have been populated. So there are no stats to include for
            // such a segment.
            return;
        }
        iteration_metrics
            .seg_stats
            .insert(this_seg_id, (*object_metrics).clone());
    }

    fn emit_object_counters_and_histograms(&self, _object_metrics: &Self::ObjectMetrics) {}
}
