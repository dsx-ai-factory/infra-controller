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

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use carbide_site_health_probe_api::metrics::{CONTRACT_VERSION, selector_problems};
use carbide_site_health_probe_api::{Family, MetricsInfo};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::error::HistoryError;
use crate::samples::is_time;

/// The wall clock, injectable for tests.
pub type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// The probe's `/v1/config` `metrics` block, checked. The caller reads it
/// from the probe and injects it: this crate never calls the probe.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeMetadata {
    info: MetricsInfo,
    scrape_interval: Duration,
    reader_intervals: BTreeMap<Family, Duration>,
}

/// `seconds` as a positive interval, or `None` when no `Duration` holds it.
fn interval(seconds: f64) -> Option<Duration> {
    Duration::try_from_secs_f64(seconds)
        .ok()
        .filter(|d| !d.is_zero())
}

impl ProbeMetadata {
    /// Checks the metadata a probe serves. Values this client cannot read
    /// are unsupported, and the error never repeats them.
    pub fn new(info: MetricsInfo) -> Result<Self, HistoryError> {
        if info.contract_version != CONTRACT_VERSION {
            return Err(HistoryError::Unsupported(format!(
                "the probe publishes metric contract {}, this client reads {CONTRACT_VERSION}",
                info.contract_version
            )));
        }
        if !selector_problems(&info.selector).is_empty() {
            return Err(HistoryError::Unsupported(
                "the probe's series selector cannot select its series".to_string(),
            ));
        }
        let scrape_interval = interval(info.scrape_interval_seconds).ok_or_else(|| {
            HistoryError::Unsupported(
                "the probe's scrape interval is not a positive time".to_string(),
            )
        })?;
        let reader_intervals = info
            .reader_intervals_seconds
            .iter()
            .map(|(family, seconds)| Some((*family, interval(*seconds)?)))
            .collect::<Option<_>>()
            .ok_or_else(|| {
                HistoryError::Unsupported(
                    "a reader interval of the probe is not a positive time".to_string(),
                )
            })?;
        if !is_time(info.process_start_seconds) {
            return Err(HistoryError::Unsupported(
                "the probe's process start is not a Unix time".to_string(),
            ));
        }
        Ok(Self {
            info,
            scrape_interval,
            reader_intervals,
        })
    }

    pub fn info(&self) -> &MetricsInfo {
        &self.info
    }

    /// How often the store scrapes the probe.
    pub fn scrape_interval(&self) -> Duration {
        self.scrape_interval
    }

    /// The reader interval of `family`, or `None` when no enabled reader
    /// sweeps it.
    pub fn reader_interval(&self, family: Family) -> Option<Duration> {
        self.reader_intervals.get(&family).copied()
    }
}

/// What one history call runs with. The deadline and the cancel token bound
/// the whole call: every step checks them before it starts and the reply
/// is checked against them once more.
#[derive(Clone)]
pub struct Context {
    pub probe: ProbeMetadata,
    pub clock: Clock,
    pub deadline: Instant,
    pub cancel: CancellationToken,
}

impl Context {
    pub fn now(&self) -> SystemTime {
        (self.clock)()
    }

    /// `Err` once the call is cancelled or its deadline has passed. Cancel
    /// is checked first.
    pub fn check(&self) -> Result<(), HistoryError> {
        if self.cancel.is_cancelled() {
            Err(HistoryError::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(HistoryError::Timeout)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn info() -> MetricsInfo {
        MetricsInfo {
            contract_version: CONTRACT_VERSION,
            selector: [("job", "j"), ("namespace", "ns")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            scrape_interval_seconds: 5.0,
            reader_intervals_seconds: [(Family::Host, 20.0), (Family::RestTotals, 120.0)]
                .into_iter()
                .collect(),
            process_start_seconds: 1_000.0,
        }
    }

    #[test]
    fn metadata_takes_what_the_probe_serves() {
        let probe = ProbeMetadata::new(info()).expect("valid");
        assert_eq!(probe.scrape_interval(), Duration::from_secs(5));
        assert_eq!(
            probe.reader_interval(Family::Host),
            Some(Duration::from_secs(20))
        );
        assert_eq!(probe.reader_interval(Family::Dpu), None, "not read");
        assert_eq!(probe.info(), &info());

        // The probe sets no longest interval: a slow REST reader is valid.
        let mut slow = info();
        slow.reader_intervals_seconds
            .insert(Family::RestTotals, 7_200.0);
        slow.scrape_interval_seconds = 3_600.5;
        let probe = ProbeMetadata::new(slow).expect("valid");
        assert_eq!(
            probe.reader_interval(Family::RestTotals),
            Some(Duration::from_secs(7_200))
        );
        assert_eq!(probe.scrape_interval(), Duration::from_secs_f64(3_600.5));
    }

    /// A named change to valid metadata.
    type Edit = (&'static str, fn(&mut MetricsInfo));

    #[test]
    fn metadata_this_client_cannot_read_is_unsupported() {
        let cases: [Edit; 11] = [
            ("contract", |i| i.contract_version = CONTRACT_VERSION + 1),
            ("empty selector", |i| i.selector.clear()),
            ("intrinsic label", |i| {
                i.selector.insert("family".to_string(), "x".to_string());
            }),
            ("zero scrape", |i| i.scrape_interval_seconds = 0.0),
            ("negative scrape", |i| i.scrape_interval_seconds = -5.0),
            ("NaN scrape", |i| i.scrape_interval_seconds = f64::NAN),
            ("unrepresentable scrape", |i| {
                i.scrape_interval_seconds = 1e300
            }),
            ("infinite reader", |i| {
                i.reader_intervals_seconds
                    .insert(Family::Dpu, f64::INFINITY);
            }),
            ("process before the epoch", |i| {
                i.process_start_seconds = -1.0;
            }),
            ("process past year 9999", |i| {
                i.process_start_seconds = 1e300;
            }),
            ("NaN process", |i| i.process_start_seconds = f64::NAN),
        ];
        for (case, edit) in cases {
            let mut bad = info();
            edit(&mut bad);
            match ProbeMetadata::new(bad) {
                Err(HistoryError::Unsupported(text)) => {
                    assert!(
                        !text.contains("1e300") && !text.contains("inf"),
                        "{case}: {text}"
                    );
                }
                other => panic!("{case}: {other:?}"),
            }
        }
    }

    fn context(cancel: CancellationToken, deadline: Instant) -> Context {
        Context {
            probe: ProbeMetadata::new(info()).expect("valid"),
            clock: Arc::new(SystemTime::now),
            deadline,
            cancel,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn check_is_cancel_first_then_the_deadline() {
        let deadline = Instant::now() + Duration::from_secs(10);
        let cancel = CancellationToken::new();
        let ctx = context(cancel.clone(), deadline);
        assert_eq!(ctx.check(), Ok(()));
        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!(ctx.check(), Err(HistoryError::Timeout), "at the deadline");
        cancel.cancel();
        assert_eq!(ctx.check(), Err(HistoryError::Cancelled), "cancel wins");
    }
}
