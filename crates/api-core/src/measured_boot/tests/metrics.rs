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
use std::time::Duration;

use measured_boot::pcr::PcrRegisterValue;
use measured_boot::records::{MeasurementBundleState, MeasurementMachineState};
use prometheus_text_parser::ParsedPrometheusMetrics;

use crate::measured_boot::metrics_collector::metrics::{
    MeasuredBootMetricsCollectorMetrics, MetricHolder,
};

#[test]
fn test_metrics_collector() {
    let mut metrics = MeasuredBootMetricsCollectorMetrics::new();
    metrics.num_machines_per_machine_state = HashMap::from([
        (MeasurementMachineState::Discovered, 1),
        (MeasurementMachineState::PendingBundle, 2),
        (MeasurementMachineState::Measured, 3),
        (MeasurementMachineState::MeasuringFailed, 4),
    ]);
    metrics.num_machines = 10;
    metrics.num_bundles = 4;
    metrics.num_machines_per_bundle = HashMap::from([
        ("f567ae32-690c-4108-8ee8-5116c64ff3f0".parse().unwrap(), 1),
        ("53008cc3-988e-459c-a4c1-acb0ea44a884".parse().unwrap(), 2),
        ("3e4ec902-f834-448c-9c0d-aca7da84d5d7".parse().unwrap(), 3),
        ("33be881e-5871-4519-b7dd-84946f3b758a".parse().unwrap(), 4),
    ]);
    metrics.num_profiles = 7;
    metrics.num_machines_per_profile = HashMap::from([
        ("bf13aaeb-c9e6-4e42-9b2f-5aa525c56124".parse().unwrap(), 1),
        ("254542a1-0fb2-4250-91df-b5ecfa52645e".parse().unwrap(), 2),
        ("4ed2dea5-c325-4317-aeb3-e9766956d9fc".parse().unwrap(), 3),
        ("c5c926f3-a454-497c-95ff-d21647590631".parse().unwrap(), 4),
    ]);
    metrics.num_machines_per_bundle_state = HashMap::from([
        (MeasurementBundleState::Pending, 1),
        (MeasurementBundleState::Active, 1),
        (MeasurementBundleState::Obsolete, 1),
        (MeasurementBundleState::Retired, 3),
        (MeasurementBundleState::Revoked, 4),
    ]);
    metrics.num_bundles_per_pcr_value = HashMap::from([
        (
            PcrRegisterValue {
                pcr_register: 0,
                sha_any: "aa".to_string(),
            },
            3,
        ),
        (
            PcrRegisterValue {
                pcr_register: 1,
                sha_any: "bb".to_string(),
            },
            1,
        ),
    ]);

    let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
    let metric_holder = Arc::new(MetricHolder::new(test_meter.meter(), Duration::MAX));
    metric_holder.update_metrics(metrics);
    assert_eq!(
        test_meter
            .export_metrics()
            .parse::<ParsedPrometheusMetrics>()
            .unwrap(),
        include_str!("test_data/test_metrics_collector.txt")
            .parse::<ParsedPrometheusMetrics>()
            .unwrap()
    );
}
