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

//! The global alias has its own test binary so provider setup cannot race
//! with unrelated emitters. The provider is replaced before any metric emits.

use carbide_instrument::testing::capture_logs;
use carbide_instrument::{Event, Instrumentation, emit, emit_count, initialize_counter_series};
use opentelemetry_sdk::metrics::SdkMeterProvider;

#[derive(Event)]
#[event(
    event_name = "test_global_alias_count",
    metric_name = "carbide_test_global_alias_total",
    component = "instrument-test",
    metric = counter,
    describe = "Number of global alias test events",
    log = off,
)]
struct Count;

#[derive(Event)]
#[event(
    event_name = "test_global_alias_log",
    component = "instrument-test",
    message = "log before metrics setup"
)]
struct LogOnly;

#[test]
fn global_alias_resolves_lazily_and_shares_all_free_function_instruments() {
    let global = Instrumentation::global();
    let clone = global.clone();
    let logs = capture_logs(|| {
        global.emit(LogOnly);
        emit(LogOnly);
    });
    assert_eq!(logs.len(), 2);

    let registry = prometheus::Registry::new();
    let exporter = opentelemetry_prometheus::exporter()
        .with_registry(registry.clone())
        .without_scope_info()
        .without_target_info()
        .build()
        .unwrap();
    let provider = SdkMeterProvider::builder().with_reader(exporter).build();
    let owned = Instrumentation::from_meter_provider(&provider);
    // Explicit recording must not initialize the global compatibility meter.
    owned.emit_count(Count, 10);
    opentelemetry::global::set_meter_provider(provider);

    assert!(global.initialize_counter_series(&Count));
    assert!(initialize_counter_series(&Count));
    let value = || {
        registry
            .gather()
            .into_iter()
            .find(|family| family.name() == "carbide_test_global_alias_total")
            .unwrap()
            .get_metric()[0]
            .get_counter()
            .value()
    };
    assert_eq!(value(), 10.0);
    clone.emit(Count);
    Instrumentation::global().emit_count(Count, 2);
    emit(Count);
    emit_count(Count, 3);
    assert_eq!(value(), 17.0);
}
