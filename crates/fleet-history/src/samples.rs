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

//! Stored samples to sweeps. Every scrape carries a count and the end time
//! `E` of the sweep it came from (one snapshot). Samples join per scrape on
//! `(source, timestamp)`, and scrapes collapse to sweeps by
//! `(process start, E)`.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use carbide_site_health_probe_api::MAX_COUNT;
use carbide_site_health_probe_api::metrics::{
    PROCESS_START, SWEEP_COMPLETE, SWEEP_RESULT_INCOMPLETE, SWEEP_START, SWEEPS, TIME_DIMS,
};

use crate::error::HistoryError;

/// The last time, in Unix seconds, a probe sample can have or show: the end
/// of year 9999.
const LAST_TIME_SECS: f64 = 253_402_300_799.0;

/// One series and its raw samples, in Unix milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct RawSeries {
    pub labels: BTreeMap<String, String>,
    pub samples: Vec<(i64, f64)>,
}

/// Whether `secs` is a Unix time a probe sample can have or show: from the
/// epoch to the end of year 9999.
pub fn is_time(secs: f64) -> bool {
    (0.0..=LAST_TIME_SECS).contains(&secs)
}

/// Whether `value` is a number of units: a whole number from 0 to
/// [`MAX_COUNT`].
fn is_count(value: f64) -> bool {
    value.fract() == 0.0 && (0.0..=MAX_COUNT as f64).contains(&value)
}

fn name_of(series: &RawSeries) -> &str {
    series.labels.get("__name__").map_or("", String::as_str)
}

/// A scrape source: the ordered label pairs of a series without the name
/// and its dims. Pairs, not a joined text, so two label sets never share a
/// key.
type Source = Vec<(String, String)>;

/// The scrape source of a series: every label but the name and `dims`.
fn source_of(labels: &BTreeMap<String, String>, dims: &[&str]) -> Source {
    labels
        .iter()
        .filter(|(k, _)| k.as_str() != "__name__" && !dims.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Records `value` at `key`. Two samples of one source and time must agree,
/// or the result would depend on the order they arrive in.
fn put_time(
    map: &mut BTreeMap<PointKey, f64>,
    key: PointKey,
    value: f64,
) -> Result<(), HistoryError> {
    match map.entry(key) {
        Entry::Occupied(seen) if *seen.get() != value => Err(HistoryError::bad_store(
            "two samples of one source and time differ",
        )),
        Entry::Occupied(_) => Ok(()),
        Entry::Vacant(slot) => {
            slot.insert(value);
            Ok(())
        }
    }
}

pub(crate) fn ms_secs(ms: i64) -> f64 {
    ms as f64 / 1000.0
}

/// One scrape joined across series: its count, and the sweep it shows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScrapePoint {
    pub source: Source,
    pub at_ms: i64,
    pub count: f64,
    /// Sweep end, in Unix seconds.
    pub end: f64,
    pub start: f64,
    pub process: f64,
    /// Whether the scrape's capability flags allow the filter: `None` when
    /// a flag sample is missing.
    pub capable: Option<bool>,
}

/// One sweep, as first seen by any scrape.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Sweep {
    pub process: f64,
    pub start: f64,
    pub end: f64,
    pub count: f64,
    pub first_seen_ms: i64,
    pub capable: Option<bool>,
}

type PointKey = (Source, i64);

/// The flag series of one capability and the value that allows the filter.
/// Only the series of the gauge `name` with `label` equal to `value` are
/// the capability's: the others are the flags of other resources or
/// predicates, or other gauges.
pub(crate) struct CapSeries<'a> {
    pub series: &'a [RawSeries],
    pub name: &'a str,
    pub label: &'a str,
    pub value: &'a str,
    pub ok: fn(f64) -> bool,
}

/// Joins count, time and capability samples on `(source, timestamp)`. A
/// point needs a sweep end, a sweep start and a process start; a missing
/// count series is 0, as a store client reads counts after times. Counts
/// add up in integers: every sample, and every sum, is a whole number from
/// 0 to [`MAX_COUNT`], or the store is not showing the probe's series.
pub(crate) fn join_points(
    counts: &[RawSeries],
    count_dims: &[&str],
    times: &[RawSeries],
    caps: &[CapSeries<'_>],
) -> Result<Vec<ScrapePoint>, HistoryError> {
    let mut count: BTreeMap<PointKey, u64> = BTreeMap::new();
    for series in counts {
        let source = source_of(&series.labels, count_dims);
        for (at, value) in &series.samples {
            let total = count.entry((source.clone(), *at)).or_default();
            *total = is_count(*value)
                .then_some(*value as u64)
                .and_then(|value| total.checked_add(value))
                .filter(|sum| *sum <= MAX_COUNT)
                .ok_or_else(|| HistoryError::bad_store("a count is not a number of units"))?;
        }
    }
    let (end_name, start_name, process_name) = (
        SWEEP_COMPLETE.metric_name(),
        SWEEP_START.metric_name(),
        PROCESS_START.metric_name(),
    );
    let mut by_name: BTreeMap<&str, BTreeMap<PointKey, f64>> = BTreeMap::new();
    for series in times {
        let source = source_of(&series.labels, &TIME_DIMS);
        let values = by_name.entry(name_of(series)).or_default();
        for (at, value) in &series.samples {
            put_time(values, (source.clone(), *at), *value)?;
        }
    }
    let flags: Vec<BTreeMap<PointKey, bool>> = caps
        .iter()
        .map(|cap| {
            let mut ok: BTreeMap<PointKey, bool> = BTreeMap::new();
            for series in cap.series.iter().filter(|s| {
                name_of(s) == cap.name
                    && s.labels.get(cap.label).map(String::as_str) == Some(cap.value)
            }) {
                let source = source_of(&series.labels, &[cap.label]);
                for (at, value) in &series.samples {
                    let entry = ok.entry((source.clone(), *at)).or_insert(true);
                    *entry = *entry && (cap.ok)(*value);
                }
            }
            ok
        })
        .collect();
    let empty = BTreeMap::new();
    let ends = by_name.get(end_name.as_str()).unwrap_or(&empty);
    let starts = by_name.get(start_name.as_str()).unwrap_or(&empty);
    let processes = by_name.get(process_name.as_str()).unwrap_or(&empty);
    Ok(ends
        .iter()
        .filter_map(|(key, end)| {
            let start = starts.get(key)?;
            let process = processes.get(key)?;
            let capable = flags
                .iter()
                .map(|flag| flag.get(key).copied())
                .collect::<Option<Vec<bool>>>()
                .map(|all| all.into_iter().all(|ok| ok));
            Some(ScrapePoint {
                source: key.0.clone(),
                at_ms: key.1,
                count: count.get(key).copied().unwrap_or(0) as f64,
                end: *end,
                start: *start,
                process: *process,
                capable,
            })
        })
        .collect())
}

/// What two scrapes of one sweep show about its capability: any false
/// stands, and missing evidence is never read as true.
fn merge_capable(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

/// Collapses scrapes to sweeps: one per `(process start, sweep end)`, as
/// its first scrape saw it, sorted by sweep end. Monitoring replicas at equal
/// or offset times collapse, and the capability of every scrape of a sweep
/// counts; two scrapes that disagree on one sweep's count mean the store
/// mixes probes.
pub(crate) fn sweeps(points: &[ScrapePoint]) -> Result<Vec<Sweep>, HistoryError> {
    let mut sorted: Vec<&ScrapePoint> = points.iter().collect();
    sorted.sort_by_key(|p| p.at_ms);
    let mut seen: BTreeMap<(u64, u64), Sweep> = BTreeMap::new();
    for point in sorted {
        // Each sample is finite, but one can be out of range and series
        // summed into one count can overflow.
        if !(0.0..=MAX_COUNT as f64).contains(&point.count) {
            return Err(HistoryError::bad_store("a count is not a number of units"));
        }
        if ![point.end, point.start, point.process]
            .into_iter()
            .all(is_time)
        {
            return Err(HistoryError::bad_store(
                "a sweep or process time is out of range",
            ));
        }
        let key = (point.process.to_bits(), point.end.to_bits());
        match seen.get_mut(&key) {
            Some(sweep) if sweep.count != point.count => {
                return Err(HistoryError::bad_store(
                    "inconsistent sources: one sweep has two counts",
                ));
            }
            Some(sweep) => sweep.capable = merge_capable(sweep.capable, point.capable),
            None => {
                seen.insert(
                    key,
                    Sweep {
                        process: point.process,
                        start: point.start,
                        end: point.end,
                        count: point.count,
                        first_seen_ms: point.at_ms,
                        capable: point.capable,
                    },
                );
            }
        }
    }
    let mut out: Vec<Sweep> = seen.into_values().collect();
    out.sort_by(|a, b| a.end.total_cmp(&b.end));
    Ok(out)
}

/// Incomplete sweeps in the run `(start, end]`: per probe process, the
/// counter's rise over the run's scrapes, from its last scrape at or before
/// `start` (0 for a process that started inside the run). Monitoring
/// replicas report the same counter of a process at their own times; the
/// latest sample at or before `start` and the highest in the run count,
/// whichever replica took them. A counter sample that is not a count means
/// the store is not showing the probe's series.
///
/// Attribution is by scrape time, so it is approximate by the bracketing
/// scrape gap at each edge, normally one scrape interval, never exact: an
/// incomplete sweep between the last scrape before `start` and `start`
/// shows first in the run and is counted, and one between the last scrape
/// before `end` and `end` shows only after it and is left out. Scrape gaps
/// are reported separately.
pub(crate) fn incomplete_sweeps(
    times: &[RawSeries],
    start: f64,
    end: f64,
) -> Result<u64, HistoryError> {
    let (process_name, sweeps_name) = (PROCESS_START.metric_name(), SWEEPS.exported_name());
    let mut process_at: BTreeMap<PointKey, f64> = BTreeMap::new();
    for series in times {
        if name_of(series) == process_name {
            let source = source_of(&series.labels, &TIME_DIMS);
            for (at, value) in &series.samples {
                put_time(&mut process_at, (source.clone(), *at), *value)?;
            }
        }
    }
    #[derive(Default)]
    struct Counter {
        /// The latest sample at or before the run's start.
        before: Option<(i64, f64)>,
        /// The highest sample in the run.
        highest: Option<f64>,
    }
    let mut per_process: BTreeMap<u64, Counter> = BTreeMap::new();
    for series in times {
        let is_incomplete = name_of(series) == sweeps_name
            && series.labels.get("result").map(String::as_str) == Some(SWEEP_RESULT_INCOMPLETE);
        if !is_incomplete {
            continue;
        }
        let source = source_of(&series.labels, &TIME_DIMS);
        for (at, value) in &series.samples {
            let Some(process) = process_at.get(&(source.clone(), *at)) else {
                continue;
            };
            if !is_count(*value) {
                return Err(HistoryError::bad_store("a sweep counter is not a count"));
            }
            let counter = per_process.entry(process.to_bits()).or_default();
            let at_secs = ms_secs(*at);
            if at_secs <= start {
                if counter
                    .before
                    .is_none_or(|(t, v)| *at > t || (*at == t && *value > v))
                {
                    counter.before = Some((*at, *value));
                }
            } else if at_secs <= end {
                counter.highest = Some(counter.highest.map_or(*value, |h| h.max(*value)));
            }
        }
    }
    per_process
        .into_iter()
        .filter_map(|(process, counter)| {
            let highest = counter.highest?;
            let base = if f64::from_bits(process) > start {
                0.0
            } else {
                // Without a sample before the run, its first increments are
                // unseen: count from 0 only for a process that started in it.
                counter.before.map_or(highest, |(_, v)| v)
            };
            Some((highest - base).max(0.0) as u64)
        })
        .try_fold(0_u64, u64::checked_add)
        .ok_or_else(|| HistoryError::bad_store("incomplete sweeps add up past any count"))
}

/// Probe restarts in the run `(start, end]`: processes that started in it
/// after an earlier process of the same probe. Read from the raw process
/// start samples, so a process that never completed a sweep still counts.
/// A restart before the run or after it is not the run's; replicas report
/// the same start, so each process counts once. A process start that is no
/// Unix time means the store is not showing the probe's series.
pub(crate) fn probe_restarts(
    times: &[RawSeries],
    start: f64,
    end: f64,
) -> Result<u32, HistoryError> {
    let process_name = PROCESS_START.metric_name();
    let mut processes: BTreeSet<u64> = BTreeSet::new();
    for series in times.iter().filter(|s| name_of(s) == process_name) {
        for (_, value) in &series.samples {
            if !is_time(*value) {
                return Err(HistoryError::bad_store(
                    "a sweep or process time is out of range",
                ));
            }
            processes.insert(value.to_bits());
        }
    }
    let mut starts: Vec<f64> = processes.into_iter().map(f64::from_bits).collect();
    starts.sort_by(f64::total_cmp);
    let restarts = starts
        .iter()
        .skip(1)
        .filter(|p| **p > start && **p <= end)
        .count();
    Ok(u32::try_from(restarts).unwrap_or(u32::MAX))
}
