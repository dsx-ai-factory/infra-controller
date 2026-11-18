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

use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opentelemetry::KeyValue;

/// [`SharedMetricsHolder`] allows wrapping a bag of metrics in a Mutex, while also encapsulating some
/// logic for conditionally emitting them based on a hold_period.
///
/// This is intended to be used within an observable_gauge callback, which may run at any time and
/// thus needs thread-safe access to the metrics object.
///
/// Metrics can be loaded into a SharedMetricsHolder with [`SharedMetricsHolder::update`], after
/// which they will become available to observable gauges until `hold_period` has expired.
/// [`SharedMetricsHolder::update_held_for`] gives one recording a hold period of its own.
///
/// To get the metrics back, callers can use [`SharedMetricsHolder::if_available`], which will call
/// the passed closure if metrics are available and have not expired.
#[derive(Debug)]
pub struct SharedMetricsHolder<T: Debug> {
    inner: Arc<SharedMetricsHolderInner<T>>,
}

// Derived Clone seems to require T to be Clone, which seems wrong. Manually impl'ing Clone fixes this.
impl<T: Debug> Clone for SharedMetricsHolder<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Debug> SharedMetricsHolder<T> {
    /// Construct a SharedMetricsHolder that will only hold metrics for `hold_period`, after which
    /// calls to `if_available` will no longer call back.
    pub fn with_hold_period(hold_period: Duration) -> Self {
        Self {
            inner: Arc::new(SharedMetricsHolderInner {
                metrics: Mutex::new(None),
                hold_period: Some(hold_period),
                fresh_period: None,
            }),
        }
    }

    /// Construct a SharedMetricsHolder that will give metrics back via `if_available` indefinitely,
    /// but will add a `"fresh"` attribute to the [`SharedMetricsHolder::if_available`] callback,
    /// set to true if the metrics are newer than `fresh_period`, false otherwise.
    pub fn with_fresh_period(fresh_period: Duration) -> Self {
        Self {
            inner: Arc::new(SharedMetricsHolderInner {
                metrics: Mutex::new(None),
                hold_period: None,
                fresh_period: Some(fresh_period),
            }),
        }
    }

    /// Calls the passed in closure with the current metrics, if they're available, and a list of
    /// attributes.
    ///
    /// - If no metrics have been loaded with [`SharedMetricsHolder::update`], the callback will not
    ///   be called.
    /// - If `self` was constructed via [`SharedMetricsHolder::with_hold_period`], or the metrics
    ///   were stored with [`SharedMetricsHolder::update_held_for`] (whose period takes precedence
    ///   for that recording), the callback will only be called if the metrics are newer than that
    ///   hold period.
    /// - If `self` was constructed via [`SharedMetricsHolder::with_fresh_period`], the callback
    ///   will include an attribute of  `{"fresh": true}` if the metrics are newer than
    ///   `fresh_period`, or `{"fresh": false}` otherwise.
    pub fn if_available<F>(&self, f: F)
    where
        F: FnOnce(&T, &[KeyValue]),
    {
        let guard = self.inner.metrics.lock().unwrap();

        let Some(MetricsWithInstant {
            metrics,
            recording_finished_at,
            hold_period,
        }) = guard.as_ref()
        else {
            return;
        };

        if let Some(hold_period) = hold_period.as_ref().or(self.inner.hold_period.as_ref()) {
            // If they've expired, don't call back
            if recording_finished_at.elapsed().gt(hold_period) {
                return;
            }
        }

        if let Some(fresh_period) = &self.inner.fresh_period {
            if recording_finished_at.elapsed().lt(fresh_period) {
                f(metrics, &[KeyValue::new("fresh", true)])
            } else {
                f(metrics, &[KeyValue::new("fresh", false)])
            }
        } else {
            f(metrics, &[]);
        }
    }

    /// Set the current metrics, taking note of the current time for freshness/hold checks.
    pub fn update(&self, new_metrics: T) {
        self.store(new_metrics, None);
    }

    /// Set the current metrics and hold them for `hold_period` in place of the
    /// holder's own, for a producer whose iterations can outlast that period.
    pub fn update_held_for(&self, new_metrics: T, hold_period: Duration) {
        self.store(new_metrics, Some(hold_period));
    }

    fn store(&self, new_metrics: T, hold_period: Option<Duration>) {
        *self.inner.metrics.lock().unwrap() = Some(MetricsWithInstant {
            metrics: new_metrics,
            recording_finished_at: Instant::now(),
            hold_period,
        });
    }
}

#[derive(Debug)]
struct SharedMetricsHolderInner<T: Debug> {
    metrics: Mutex<Option<MetricsWithInstant<T>>>,
    hold_period: Option<Duration>,
    fresh_period: Option<Duration>,
}

#[derive(Debug)]
struct MetricsWithInstant<T: Debug> {
    metrics: T,
    recording_finished_at: Instant,
    /// A hold period for this recording in place of the holder's own.
    hold_period: Option<Duration>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(holder: &SharedMetricsHolder<u8>) -> Option<u8> {
        let mut seen = None;
        holder.if_available(|metrics, _| seen = Some(*metrics));
        seen
    }

    #[test]
    fn a_recording_can_outlive_the_holders_hold_period() {
        let hold_period = Duration::from_millis(1);
        let holder = SharedMetricsHolder::with_hold_period(hold_period);

        holder.update(1);
        std::thread::sleep(hold_period * 5);
        assert_eq!(
            observed(&holder),
            None,
            "the holder's period expires a plain update"
        );

        holder.update_held_for(2, Duration::MAX);
        std::thread::sleep(hold_period * 5);
        assert_eq!(
            observed(&holder),
            Some(2),
            "the recording's own period keeps it"
        );

        holder.update(3);
        std::thread::sleep(hold_period * 5);
        assert_eq!(
            observed(&holder),
            None,
            "a plain update is back on the holder's period"
        );
    }
}
