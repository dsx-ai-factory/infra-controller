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

//! An allocation-counting global allocator for benches that report
//! per-operation allocation counts alongside their timings.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

/// A [`System`]-backed allocator that counts every allocation and the bytes
/// it requested, for [`measure_allocs`] to report.
///
/// The `#[global_allocator]` declaration must live in each bench binary
/// itself; only the registration stays per-binary:
///
/// ```ignore
/// #[global_allocator]
/// static GLOBAL: CountingAllocator = CountingAllocator;
/// ```
pub struct CountingAllocator;

// SAFETY: Every allocation operation delegates to `System` without changing pointer,
// layout, size, or return-value semantics. The added atomic bookkeeping neither
// allocates nor unwinds, so `System`'s `GlobalAlloc` guarantees remain intact.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
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

    // Delegate `alloc_zeroed` and `realloc` to `System` rather than
    // inheriting `GlobalAlloc`'s alloc-then-copy/zero fallbacks, so the
    // counted runs use the same optimized paths (calloc/realloc) as
    // production and each call counts as exactly one allocation event.

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: The caller guarantees that `layout` has nonzero size as required by
        // `GlobalAlloc::alloc_zeroed`; it is forwarded unchanged to `System`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // One allocation event for the bytes of the new size; the freed old
        // block is not tracked, matching `dealloc`.
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        // SAFETY: The caller guarantees that `ptr` and `layout` describe a live
        // allocation from this allocator and that `new_size` satisfies
        // `GlobalAlloc::realloc`'s size and overflow requirements. Every allocation
        // originates from `System`, and all arguments are forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Runs `f` once and reports (allocation count, allocated bytes) it performed.
pub fn measure_allocs<T>(f: impl FnOnce() -> T) -> (u64, u64) {
    let allocs_before = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes_before = ALLOCATED_BYTES.load(Ordering::Relaxed);
    let value = f();
    let allocs = ALLOCATIONS.load(Ordering::Relaxed) - allocs_before;
    let bytes = ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes_before;
    drop(value);
    (allocs, bytes)
}
