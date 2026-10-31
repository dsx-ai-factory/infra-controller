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

//! Fleet history: when a probe count crossed each target, from the probe's
//! own series in the metrics store.
//!
//! The site health probe publishes its counts and sweep times as metrics and
//! never reads them back. This crate holds the math a client runs on the
//! samples it read from the store: the source and sweep join, the targets
//! and crossings, the timing error, the rates and the longest stall. It
//! never calls the probe or NICo. A caller injects the probe's metadata
//! (its `/v1/config` `metrics` block), a wall clock, a deadline and a
//! cancel token through a [`Context`].
//!
//! A call is [`prepare`]d from a filter and the summary parameters before
//! any store read, then [`summarize`]d from the [`Samples`] read for it.

mod context;
mod error;
mod samples;
mod summary;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod recorded_tests;

pub use context::{Clock, Context, ProbeMetadata};
pub use error::HistoryError;
pub use samples::{RawSeries, is_time};
pub use summary::{
    CURVE_POINTS, MAX_QUANTILES, Request, Samples, final_scrape_seen, prepare, summarize,
};
