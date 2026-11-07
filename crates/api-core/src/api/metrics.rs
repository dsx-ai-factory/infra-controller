/*
 * SPDX-FileCopyrightText: Copyright (c) 2025 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
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

use carbide_metrics_utils::OtelView;
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, Meter};
use opentelemetry_sdk::metrics::{Aggregation, InstrumentKind};

/// Metric name for machine reboot duration histogram
const MACHINE_REBOOT_DURATION_METRIC_NAME: &str = "carbide_machine_reboot_duration";

/// Holds all metrics related to the API service
pub struct ApiMetricsEmitter {
    machine_reboot_duration_histogram: Histogram<u64>,
}

impl ApiMetricsEmitter {
    pub fn new(meter: &Meter) -> Self {
        let machine_reboot_duration_histogram = meter
            .u64_histogram(MACHINE_REBOOT_DURATION_METRIC_NAME)
            .with_description("Time taken for machine/host to reboot in seconds")
            .with_unit("s")
            .build();

        Self {
            machine_reboot_duration_histogram,
        }
    }

    /// Creates histogram bucket configuration for machine reboot duration
    ///
    /// Machine reboots typically take 5-20 minutes (300-1200 seconds).
    /// Buckets retain minute-scale resolution and cover slow request-to-completion
    /// intervals up to two days.
    ///
    /// Boundaries: 3min, 5min, 10min, 15min, 30min, 1h, 2h, 4h, 8h, 12h, 1d, 2d.
    pub fn machine_reboot_duration_view() -> carbide_metrics_utils::Result<OtelView> {
        carbide_metrics_utils::new_view(
            MACHINE_REBOOT_DURATION_METRIC_NAME,
            Some(InstrumentKind::Histogram),
            Aggregation::ExplicitBucketHistogram {
                boundaries: vec![
                    180.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0, 14400.0, 28800.0, 43200.0,
                    86400.0, 172800.0,
                ],
                record_min_max: true,
            },
        )
    }

    /// Records machine reboot duration with product information
    pub fn record_machine_reboot_duration(
        &self,
        duration_secs: u64,
        product_name: String,
        vendor: String,
        reboot_mode: String,
    ) {
        let attributes = [
            KeyValue::new("product_name", product_name),
            KeyValue::new("vendor", vendor),
            KeyValue::new("reboot_mode", reboot_mode),
        ];

        self.machine_reboot_duration_histogram
            .record(duration_secs, &attributes);
    }
}
