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

//! Benchmarks the logfmt layer's per-line cost on the event hot path, plus the
//! value-quoting scan in isolation.
//!
//! Run with:
//!   cargo bench -p logfmt --features bench-hooks
//!
//! Before the criterion benches run, `main` reports allocations per formatted
//! line, counted by a wrapping `#[global_allocator]` (bench binaries have their
//! own allocator, so this never affects the library or its users).

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{Criterion, criterion_group};
use tracing_subscriber::prelude::*;

/// Counts every allocation entry point (`alloc`, `realloc`, `alloc_zeroed`) so
/// the bench can report allocations per log line.
struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

// SAFETY: Every allocation operation delegates to `System` without changing pointer,
// layout, size, or return-value semantics. The added atomic bookkeeping neither
// allocates nor unwinds, so `System`'s `GlobalAlloc` guarantees remain intact.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: The caller guarantees that `layout` has nonzero size as required by
        // `GlobalAlloc::alloc`; it is forwarded unchanged to `System`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: The caller guarantees that `ptr` is a live allocation from this
        // allocator and that `layout` matches it. Every allocation originates from
        // `System`, so forwarding both unchanged preserves provenance.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: The caller guarantees that `ptr` and `layout` describe a live
        // allocation from this allocator and that `new_size` satisfies
        // `GlobalAlloc::realloc`'s size and overflow requirements. Every allocation
        // originates from `System`, and all arguments are forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: The caller guarantees that `layout` has nonzero size as required by
        // `GlobalAlloc::alloc_zeroed`; it is forwarded unchanged to `System`.
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// A dispatch with just the logfmt layer, writing to `std::io::sink()` (a ZST,
/// so boxing the writer per line does not allocate): the measurement is the
/// layer's formatting work, not stdout.
fn logfmt_subscriber() -> impl tracing::Subscriber {
    tracing_subscriber::registry()
        .with(logfmt::layer().with_writer(Arc::new(|| Box::new(std::io::sink()))))
}

/// A realistic INFO line: a message plus 10 mixed fields (strings - one needing
/// quotes, debug values, integers, a float, a bool), all inside the same event.
fn emit_line() {
    tracing::info!(
        machine_id = "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0",
        machine_state = "waiting_for_dpu_up",
        previous_state = "dpu_reprovision",
        attempt = 3_i64,
        port = 8443_u64,
        progress = 0.75_f64,
        forced = false,
        elapsed = ?std::time::Duration::from_millis(15_734),
        interface = "enp1s0f0np0",
        note = "boot order re-asserted after NIC de-enumeration",
        "machine state transition applied",
    );
}

fn bench_event_line(c: &mut Criterion) {
    let _guard = tracing::subscriber::set_default(logfmt_subscriber());
    c.bench_function("logfmt_event_line_10_fields", |b| b.iter(emit_line));
}

fn bench_quoting_scan(c: &mut Criterion) {
    // Typical clean ASCII values (no quoting needed - the common case, where
    // the whole value must be scanned to prove it).
    let long = black_box("fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0");
    c.bench_function("value_needs_quoting_clean_ascii_59b", |b| {
        b.iter(|| logfmt::bench_hooks::value_needs_quoting(black_box(long)))
    });
    let short = black_box("waiting_for_dpu_up");
    c.bench_function("value_needs_quoting_clean_ascii_18b", |b| {
        b.iter(|| logfmt::bench_hooks::value_needs_quoting(black_box(short)))
    });
    // A value that needs quoting: the scan short-circuits at the first
    // quote-triggering byte (the space), the contrasting case to the
    // full-length clean scans above.
    let quoted = black_box("machine came back from reboot");
    c.bench_function("value_needs_quoting_spaced_ascii_29b", |b| {
        b.iter(|| logfmt::bench_hooks::value_needs_quoting(black_box(quoted)))
    });
}

/// Reports allocations per line, measured outside criterion so the harness's
/// own allocations don't pollute the count.
fn report_allocs_per_line() {
    let _guard = tracing::subscriber::set_default(logfmt_subscriber());
    // Warm up the thread-local format buffer to its steady-state capacity.
    for _ in 0..256 {
        emit_line();
    }
    const LINES: u64 = 10_000;
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..LINES {
        emit_line();
    }
    let delta = ALLOCATIONS.load(Ordering::Relaxed) - before;
    eprintln!(
        "allocs/line: {:.2} ({delta} allocations over {LINES} lines)",
        delta as f64 / LINES as f64
    );
}

criterion_group!(benches, bench_event_line, bench_quoting_scan);

fn main() {
    report_allocs_per_line();
    benches();
    Criterion::default().configure_from_args().final_summary();
}
