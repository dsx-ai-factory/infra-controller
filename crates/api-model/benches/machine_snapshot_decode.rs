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

//! Measures the per-row cost of decoding a machine-snapshot JSON column.
//!
//! `via_value_dom` is the double-deserialize shape (bytes -> `serde_json::Value`
//! -> `MachineSnapshotPgJson` -> `Machine`); `direct` deserializes the row
//! straight into `MachineSnapshotPgJson` the way `sqlx::types::Json` does.
//! Run with `--features test-support`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use model::machine::AnyMachine;
use model::machine::json::MachineSnapshotPgJson;
use model::test_support::alloc_counter::{CountingAllocator, measure_allocs};
use model::test_support::machine_snapshot;
use serde::Deserialize;

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// The pre-fix `FromRow for Machine` shape: decode the column into a
/// `serde_json::Value` DOM, then deserialize the struct out of the DOM.
fn decode_via_value_dom(bytes: &[u8]) -> AnyMachine {
    let json: serde_json::Value = serde_json::from_slice(bytes).expect("valid fixture JSON");
    MachineSnapshotPgJson::deserialize(json)
        .expect("fixture deserializes")
        .try_into()
        .expect("fixture converts to Machine")
}

/// The `sqlx::types::Json<T>` shape: deserialize the struct straight from the
/// column bytes.
fn decode_direct(bytes: &[u8]) -> AnyMachine {
    serde_json::from_slice::<MachineSnapshotPgJson>(bytes)
        .expect("fixture deserializes")
        .try_into()
        .expect("fixture converts to Machine")
}

fn bench_snapshot_decode(c: &mut Criterion) {
    let bytes = serde_json::to_vec(&machine_snapshot::machine_snapshot_pg_json(
        machine_snapshot::host_machine_id(),
    ))
    .expect("fixture serializes");

    // Warm up with one complete decode (result discarded) before either
    // measurement, so one-time lazy initialization on the decode path is
    // excluded from the per-row allocation numbers.
    drop(decode_via_value_dom(black_box(&bytes)));

    let (dom_allocs, dom_bytes) = measure_allocs(|| decode_via_value_dom(black_box(&bytes)));
    let (direct_allocs, direct_bytes) = measure_allocs(|| decode_direct(black_box(&bytes)));
    println!(
        "row size: {} bytes | allocations/row: via_value_dom={dom_allocs} ({dom_bytes} B), \
         direct={direct_allocs} ({direct_bytes} B)",
        bytes.len(),
    );

    let mut group = c.benchmark_group("machine_snapshot_decode");
    group.bench_function("via_value_dom", |b| {
        b.iter(|| decode_via_value_dom(black_box(&bytes)))
    });
    group.bench_function("direct", |b| b.iter(|| decode_direct(black_box(&bytes))));
    group.finish();
}

criterion_group!(benches, bench_snapshot_decode);
criterion_main!(benches);
