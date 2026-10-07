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

//! Measures the per-machine cost of the model -> RPC `Machine` conversion.
//!
//! `clone_and_convert` times a deep `Machine` clone followed by the
//! conversion (what a borrowing caller pays per machine); `convert_only`
//! times just the by-value conversion on a pre-cloned input (what a caller
//! that moves the snapshot pays). Both benchmarks use `iter_batched` with
//! `BatchSize::PerIteration`, so criterion handles the produced proto's drop
//! identically for both; the measured difference between them is exactly the
//! input deep-clone. Run with `--features model`.

use std::hint::black_box;

use carbide_uuid::machine::MachineIdSubtypeTrait;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use model::machine::Machine;
use model::test_support::alloc_counter::{CountingAllocator, measure_allocs};
use model::test_support::machine_snapshot;

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn clone_and_convert<ID: MachineIdSubtypeTrait>(machine: &Machine<ID>) -> rpc::forge::Machine {
    machine.clone().into()
}

fn bench_machine_convert(c: &mut Criterion) {
    let machine = machine_snapshot::host_machine();

    // Warm up with one complete conversion (result discarded) before either
    // measurement, so one-time lazy initialization on the conversion path
    // (e.g. the storage-model regexes in capability derivation) is excluded
    // from the per-machine allocation numbers.
    drop(clone_and_convert(black_box(&machine)));

    let (clone_convert_allocs, clone_convert_bytes) =
        measure_allocs(|| clone_and_convert(black_box(&machine)));
    let pre_cloned = machine.clone();
    let (convert_allocs, convert_bytes) =
        measure_allocs(move || rpc::forge::Machine::from(black_box(pre_cloned)));
    println!(
        "allocations/machine: clone_and_convert={clone_convert_allocs} ({clone_convert_bytes} B), \
         convert_only={convert_allocs} ({convert_bytes} B)",
    );

    let mut group = c.benchmark_group("machine_to_rpc");
    group.bench_function("clone_and_convert", |b| {
        // The input deep-clone deliberately stays inside the timed routine:
        // clone-then-convert is the code path under measurement.
        b.iter_batched(
            || (),
            |_| clone_and_convert(black_box(&machine)),
            BatchSize::PerIteration,
        )
    });
    group.bench_function("convert_only", |b| {
        // The input deep-clone happens in setup, outside the timed routine:
        // only the by-value conversion is measured.
        b.iter_batched(
            || machine.clone(),
            |owned| rpc::forge::Machine::from(black_box(owned)),
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

criterion_group!(benches, bench_machine_convert);
criterion_main!(benches);
