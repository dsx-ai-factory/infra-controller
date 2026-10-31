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

//! Recorded samples for tests: series as the probe publishes them and a
//! store returns them.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use carbide_site_health_probe_api::{Direction, Filter, SummaryParams};
use chrono::{DateTime, Utc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::context::{Context, ProbeMetadata};
use crate::samples::RawSeries;
use crate::summary::Samples;

pub(crate) const UNITS: &str = "carbide_site_health_probe_units";
pub(crate) const SWEEP_START: &str = "carbide_site_health_probe_sweep_start_timestamp_seconds";
pub(crate) const SWEEP_END: &str = "carbide_site_health_probe_sweep_complete_timestamp_seconds";
pub(crate) const SWEEPS_TOTAL: &str = "carbide_site_health_probe_sweeps_total";
pub(crate) const PROCESS_START: &str = "carbide_site_health_probe_process_start_timestamp_seconds";
pub(crate) const REST_TOTALS: &str = "carbide_site_health_probe_rest_totals";
pub(crate) const REST_TOTALS_AVAILABLE: &str = "carbide_site_health_probe_rest_totals_available";
pub(crate) const HOSTS_UPDATE_ELIGIBLE: &str = "carbide_site_health_probe_hosts_update_eligible";
pub(crate) const HOST_PREDICATES_KNOWN: &str = "carbide_site_health_probe_host_predicates_known";

const TIME_NAMES: [&str; 4] = [SWEEP_START, SWEEP_END, SWEEPS_TOTAL, PROCESS_START];

/// The labels that select the probe's series.
pub(crate) const SELECTOR: [(&str, &str); 2] = [("job", "j"), ("namespace", "ns")];

pub(crate) fn series(name: &str, labels: &[(&str, &str)], samples: &[(i64, f64)]) -> RawSeries {
    let mut map: BTreeMap<String, String> = labels
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    map.insert("__name__".to_string(), name.to_string());
    RawSeries {
        labels: map,
        samples: samples.to_vec(),
    }
}

/// A context whose wall clock stands at `now`, with an hour to run.
pub(crate) fn context(now: SystemTime) -> Context {
    Context {
        probe: ProbeMetadata::new(crate::context::tests::info()).expect("valid"),
        clock: Arc::new(move || now),
        deadline: Instant::now() + Duration::from_secs(3600),
        cancel: CancellationToken::new(),
    }
}

pub(crate) fn filter(query: &str) -> Filter {
    serde_json::from_value(
        query
            .split('&')
            .map(|pair| {
                let (k, v) = pair.split_once('=').expect("key=value");
                let value = match v {
                    "true" | "false" => serde_json::Value::Bool(v == "true"),
                    _ => serde_json::Value::String(v.to_string()),
                };
                (k.to_string(), value)
            })
            .collect::<serde_json::Map<_, _>>()
            .into(),
    )
    .expect("filter")
}

pub(crate) const READY: &str = "type=host&stage=ready";

/// Samples a store holds, merged by series as a store client merges its
/// replies.
#[derive(Default)]
pub(crate) struct Recorder {
    series: BTreeMap<BTreeMap<String, String>, BTreeMap<i64, f64>>,
}

impl Recorder {
    pub(crate) fn add(&mut self, name: &str, labels: &[(&str, &str)], at_ms: i64, value: f64) {
        let mut key: BTreeMap<String, String> = labels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        key.insert("__name__".to_string(), name.to_string());
        self.series.entry(key).or_default().insert(at_ms, value);
    }

    fn named(&self, keep: impl Fn(&str) -> bool) -> Vec<RawSeries> {
        self.series
            .iter()
            .filter(|(labels, _)| keep(labels["__name__"].as_str()))
            .map(|(labels, samples)| RawSeries {
                labels: labels.clone(),
                samples: samples.iter().map(|(t, v)| (*t, *v)).collect(),
            })
            .collect()
    }

    /// The samples of a request counting `count` with the flag series
    /// `caps`, one per need.
    pub(crate) fn samples(&self, count: &str, caps: &[&str]) -> Samples {
        Samples {
            counts: self.named(|name| name == count),
            times: self.named(|name| TIME_NAMES.contains(&name)),
            caps: caps
                .iter()
                .map(|cap| self.named(|name| name == *cap))
                .collect(),
        }
    }
}

/// A capability flag sample: metric, labels, value.
pub(crate) type Flag<'a> = (&'a str, &'a [(&'a str, &'a str)], f64);

/// A site whose clock stands at `base`, its samples recorded at offsets
/// from it in seconds.
pub(crate) struct Site {
    pub base: SystemTime,
    pub store: Recorder,
}

pub(crate) fn site() -> Site {
    Site {
        base: UNIX_EPOCH + Duration::from_secs(1_000_000),
        store: Recorder::default(),
    }
}

impl Site {
    pub(crate) fn at(&self, offset: f64) -> SystemTime {
        if offset >= 0.0 {
            self.base + Duration::from_secs_f64(offset)
        } else {
            self.base - Duration::from_secs_f64(-offset)
        }
    }

    pub(crate) fn ms(&self, offset: f64) -> i64 {
        self.at(offset)
            .duration_since(UNIX_EPOCH)
            .expect("after the epoch")
            .as_millis() as i64
    }

    pub(crate) fn secs(&self, offset: f64) -> f64 {
        self.at(offset)
            .duration_since(UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs_f64()
    }

    pub(crate) fn context(&self) -> Context {
        context(self.base)
    }

    /// One scrape at `at` of a probe whose current host snapshot ended at
    /// `end` with `count` ready hosts, and optional capability flags.
    pub(crate) fn scrape(&mut self, at: f64, count: f64, end: f64, flags: &[Flag<'_>]) {
        let ms = self.ms(at);
        let labels = |extra: &[(&'static str, &'static str)]| -> Vec<(&'static str, &'static str)> {
            extra.iter().copied().chain(SELECTOR).collect()
        };
        let (end_secs, start_secs, process) =
            (self.secs(end), self.secs(end - 10.0), self.secs(-100_000.0));
        let s = &mut self.store;
        s.add(
            UNITS,
            &labels(&[("type", "host"), ("stage", "ready"), ("state", "Ready")]),
            ms,
            count,
        );
        let times = labels(&[("reader", "devices"), ("family", "host")]);
        s.add(SWEEP_END, &times, ms, end_secs);
        s.add(SWEEP_START, &times, ms, start_secs);
        s.add(PROCESS_START, &labels(&[]), ms, process);
        for (name, extra, value) in flags {
            let mut l: Vec<(&str, &str)> = extra.to_vec();
            l.extend(SELECTOR);
            s.add(name, &l, ms, *value);
        }
    }

    /// One scrape by `instance` of a REST totals sweep that ended at `end`
    /// with one rack, with its availability flag when `flag` is set.
    pub(crate) fn rack_scrape(&mut self, at: f64, end: f64, instance: &str, flag: Option<f64>) {
        self.rest_totals_scrape(at, end, instance, Some(1.0), flag);
    }

    /// One scrape by `instance` of a REST totals sweep that ended at `end`,
    /// with its rack total and availability flag when set.
    pub(crate) fn rest_totals_scrape(
        &mut self,
        at: f64,
        end: f64,
        instance: &str,
        rack: Option<f64>,
        flag: Option<f64>,
    ) {
        let ms = self.ms(at);
        let labels = |extra: &[(&'static str, &'static str)]| -> Vec<(String, String)> {
            extra
                .iter()
                .copied()
                .chain(SELECTOR)
                .chain([("instance", instance)])
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let (end_secs, start_secs, process) =
            (self.secs(end), self.secs(end - 3.0), self.secs(-100_000.0));
        let mut add = |name: &str, labels: Vec<(String, String)>, value: f64| {
            let labels: Vec<(&str, &str)> = labels
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            self.store.add(name, &labels, ms, value);
        };
        if let Some(rack) = rack {
            add(REST_TOTALS, labels(&[("resource", "rack")]), rack);
        }
        if let Some(flag) = flag {
            add(REST_TOTALS_AVAILABLE, labels(&[("resource", "rack")]), flag);
        }
        let times = labels(&[("reader", "rest_inventory"), ("family", "rest_totals")]);
        add(SWEEP_END, times.clone(), end_secs);
        add(SWEEP_START, times, start_secs);
        add(PROCESS_START, labels(&[]), process);
    }

    pub(crate) fn params(
        &self,
        start: f64,
        end: f64,
        base: u64,
        n: u64,
        direction: Direction,
    ) -> SummaryParams {
        SummaryParams {
            start: DateTime::<Utc>::from(self.at(start)),
            end: DateTime::<Utc>::from(self.at(end)),
            base,
            n,
            direction,
            q: Some("1".to_string()),
            add_window_seconds: 0.0,
        }
    }
}
