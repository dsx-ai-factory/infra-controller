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
use std::time::Instant;

use ::carbide_utils::metrics::SharedMetricsHolder;
use carbide_uuid::measured_boot::{MeasurementBundleId, MeasurementSystemProfileId};
use measured_boot::pcr::PcrRegisterValue;
use measured_boot::records::{MeasurementBundleState, MeasurementMachineState};
use opentelemetry::KeyValue;
use opentelemetry::metrics::Meter;

/// MeasuredBootMetricsCollectorMetrics stores metrics that are gathered in
/// one a single `MeasuredBootMetricsCollector` run. These metrics are then
/// emitted into opentelemetry.
#[derive(Clone, Debug)]
pub(in crate::measured_boot) struct MeasuredBootMetricsCollectorMetrics {
    // When we finished recording the metrics.
    pub(in crate::measured_boot) recording_finished_at: std::time::Instant,
    // The number of measured boot profiles.
    pub(in crate::measured_boot) num_profiles: usize,
    // The number of measured boot bundles.
    pub(in crate::measured_boot) num_bundles: usize,
    // Discovered candidate machines, including those without measurements.
    pub(in crate::measured_boot) num_machines: usize,
    // The number of machines per profile.
    pub(in crate::measured_boot) num_machines_per_profile:
        HashMap<MeasurementSystemProfileId, usize>,
    // The number of machines per bundle.
    pub(in crate::measured_boot) num_machines_per_bundle: HashMap<MeasurementBundleId, usize>,
    // The number of machines per bundle state.
    pub(in crate::measured_boot) num_machines_per_bundle_state:
        HashMap<MeasurementBundleState, usize>,
    // The number of machines per machine state.
    pub(in crate::measured_boot) num_machines_per_machine_state:
        HashMap<MeasurementMachineState, usize>,
    // Bundles containing each PCR index/value pair. Only the golden values
    // selected for a bundle contribute; counting every reported measurement
    // would introduce high-cardinality PCR values.
    pub(in crate::measured_boot) num_bundles_per_pcr_value: HashMap<PcrRegisterValue, usize>,
}

impl MeasuredBootMetricsCollectorMetrics {
    pub(in crate::measured_boot) fn new() -> Self {
        Self {
            recording_finished_at: Instant::now(),
            num_profiles: 0,
            num_bundles: 0,
            num_machines: 0,
            num_machines_per_profile: HashMap::new(),
            num_machines_per_bundle: HashMap::new(),
            num_machines_per_bundle_state: HashMap::new(),
            num_machines_per_machine_state: HashMap::new(),
            num_bundles_per_pcr_value: HashMap::new(),
        }
    }
}

fn hydrate_meter(
    meter: Meter,
    shared_metrics: SharedMetricsHolder<MeasuredBootMetricsCollectorMetrics>,
) {
    // These gauges can decrease. Add `_count` names while preserving the
    // original names, values, and HELP text for existing queries.
    for name in [
        "carbide_measured_boot_profiles_total",
        "carbide_measured_boot_profiles_count",
    ] {
        let metrics = shared_metrics.clone();
        meter
            .u64_observable_gauge(name)
            .with_description("Number of measured boot profiles.")
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    observer.observe(metrics.num_profiles as u64, attrs);
                });
            })
            .build();
    }

    for name in [
        "carbide_measured_boot_bundles_total",
        "carbide_measured_boot_bundles_count",
    ] {
        let metrics = shared_metrics.clone();
        meter
            .u64_observable_gauge(name)
            .with_description("Number of measured boot bundles.")
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    observer.observe(metrics.num_bundles as u64, attrs);
                });
            })
            .build();
    }

    for (name, description) in [
        (
            "carbide_measured_boot_machines_total",
            "Number of machines reporting measurements.",
        ),
        (
            "carbide_measured_boot_machines_count",
            "Number of discovered candidate machines, including machines without measurements.",
        ),
    ] {
        let metrics = shared_metrics.clone();
        meter
            .u64_observable_gauge(name)
            .with_description(description)
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    observer.observe(metrics.num_machines as u64, attrs);
                })
            })
            .build();
    }

    for name in [
        "carbide_measured_boot_machines_per_profile_total",
        "carbide_measured_boot_machines_per_profile_count",
    ] {
        let metrics = shared_metrics.clone();
        meter
            .u64_observable_gauge(name)
            .with_description("Number of machines per measured boot system profile.")
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    for (profile_id, total) in metrics.num_machines_per_profile.iter() {
                        observer.observe(
                            *total as u64,
                            &[
                                attrs,
                                &[KeyValue::new("profile_id", profile_id.to_string())],
                            ]
                            .concat(),
                        );
                    }
                });
            })
            .build();
    }

    for name in [
        "carbide_measured_boot_machines_per_bundle_total",
        "carbide_measured_boot_machines_per_bundle_count",
    ] {
        let metrics = shared_metrics.clone();
        meter
            .u64_observable_gauge(name)
            .with_description("Number of machines per measured boot bundle.")
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    for (bundle_id, total) in metrics.num_machines_per_bundle.iter() {
                        observer.observe(
                            *total as u64,
                            &[attrs, &[KeyValue::new("bundle_id", bundle_id.to_string())]].concat(),
                        );
                    }
                });
            })
            .build();
    }

    for name in [
        "carbide_measured_boot_machines_per_bundle_state_total",
        "carbide_measured_boot_machines_per_bundle_state_count",
    ] {
        let metrics = shared_metrics.clone();
        meter
            .u64_observable_gauge(name)
            .with_description("Number of machines per measured boot bundle state.")
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    for (bundle_state, total) in metrics.num_machines_per_bundle_state.iter() {
                        observer.observe(
                            *total as u64,
                            &[
                                attrs,
                                &[KeyValue::new("bundle_state", bundle_state.to_string())],
                            ]
                            .concat(),
                        );
                    }
                })
            })
            .build();
    }

    for name in [
        "carbide_measured_boot_machines_per_machine_state_total",
        "carbide_measured_boot_machines_per_machine_state_count",
    ] {
        let metrics = shared_metrics.clone();
        meter
            .u64_observable_gauge(name)
            .with_description("Number of machines per measured boot machine state.")
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    for (machine_state, total) in metrics.num_machines_per_machine_state.iter() {
                        observer.observe(
                            *total as u64,
                            &[
                                attrs,
                                &[KeyValue::new("machine_state", machine_state.to_string())],
                            ]
                            .concat(),
                        );
                    }
                });
            })
            .build();
    }

    // The producer counts bundles. Keep the legacy machine name and HELP
    // for compatibility, and describe the actual entity in the new alias.
    for (name, description) in [
        (
            "carbide_measured_boot_machines_per_pcr_value_total",
            "Number of machines with a given PCR value at a given PCR index.",
        ),
        (
            "carbide_measured_boot_bundles_per_pcr_value_count",
            "Number of measured boot bundles containing a given PCR value at a given PCR index.",
        ),
    ] {
        let metrics = shared_metrics.clone();
        meter
            .u64_observable_gauge(name)
            .with_description(description)
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    for (pcr_register, total) in metrics.num_bundles_per_pcr_value.iter() {
                        observer.observe(
                            *total as u64,
                            &[
                                attrs,
                                &[
                                    KeyValue::new(
                                        "pcr_index",
                                        pcr_register.pcr_register.to_string(),
                                    ),
                                    KeyValue::new("pcr_value", pcr_register.sha_any.clone()),
                                ],
                            ]
                            .concat(),
                        );
                    }
                });
            })
            .build();
    }
}

/// Stores Metric data shared between the Fabric Monitor and the OpenTelemetry background task
pub(in crate::measured_boot) struct MetricHolder {
    last_iteration_metrics: SharedMetricsHolder<MeasuredBootMetricsCollectorMetrics>,
}

impl MetricHolder {
    pub(in crate::measured_boot) fn new(meter: Meter, hold_period: std::time::Duration) -> Self {
        let last_iteration_metrics = SharedMetricsHolder::with_hold_period(hold_period);
        hydrate_meter(meter, last_iteration_metrics.clone());
        Self {
            last_iteration_metrics,
        }
    }

    /// Updates the most recent metrics
    pub(in crate::measured_boot) fn update_metrics(
        &self,
        mut metrics: MeasuredBootMetricsCollectorMetrics,
    ) {
        metrics.recording_finished_at = Instant::now();
        self.last_iteration_metrics.update(metrics)
    }
}
