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

//! `/v1/summary`: when a count crossed each target, from the probe's own
//! series in the metrics store.
//!
//! Every scrape carries a count and the end time `E` of the sweep it came
//! from (one snapshot, see the state collector). The summary joins them per
//! scrape, collapses scrapes to sweeps by `(process start, E)`, and times
//! each crossing at the first sweep end that meets the target. A crossing
//! happened at most one sweep gap plus one sweep duration before the `E`
//! that shows it, so the timing error bound is the largest sweep gap, the
//! longest sweep, the client's write window and any scrape gap no sweep gap
//! covers.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use carbide_site_health_probe_api::{
    Crossing, CurvePoint, Direction, ErrorCode, Family, MAX_COUNT, Q_DECIMALS, ScrapeGap,
    SummaryResponse, UnitsPerMinute,
};
use chrono::{DateTime, Utc};

use crate::api::ApiError;
use crate::store::{RawSeries, is_time};

const PREFIX: &str = "carbide_site_health_probe_";
const SWEEP_START: &str = "carbide_site_health_probe_sweep_start_timestamp_seconds";
const SWEEP_END: &str = "carbide_site_health_probe_sweep_complete_timestamp_seconds";
const SWEEPS_TOTAL: &str = "carbide_site_health_probe_sweeps_total";
const PROCESS_START: &str = "carbide_site_health_probe_process_start_timestamp_seconds";
/// The dimension labels of the time series; every other label names the
/// scrape source.
const TIME_DIMS: [&str; 3] = ["reader", "family", "result"];

/// 10^[`Q_DECIMALS`]: every accepted q is a whole number of these parts.
const Q_PARTS: u64 = 10_u64.pow(Q_DECIMALS as u32);

/// The curve keeps at most this many points.
pub(crate) const CURVE_POINTS: usize = 200;
/// Quantiles one summary may ask for: each crossing is a curve point that is
/// always kept, next to the first and the last.
pub(crate) const MAX_QUANTILES: usize = CURVE_POINTS - 2;
/// Scrape gaps listed in a reply; `scrapeGapsTotal` counts them all.
const LISTED_SCRAPE_GAPS: usize = 100;
/// `peak10m` looks at count changes within this window.
const PEAK_WINDOW: f64 = 600.0;

fn secs(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// The date of Unix time `secs`. Times come from store samples, so any
/// value is possible: one before the epoch (or NaN) is the epoch, one past
/// the last date chrono holds is that date.
fn at_secs(secs: f64) -> DateTime<Utc> {
    Duration::try_from_secs_f64(secs.max(0.0))
        .ok()
        .and_then(|d| DateTime::from_timestamp(i64::try_from(d.as_secs()).ok()?, d.subsec_nanos()))
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

fn ms_secs(ms: i64) -> f64 {
    ms as f64 / 1000.0
}

/// One scrape joined across series: its count, and the sweep it shows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScrapePoint {
    pub source: String,
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

/// The scrape source of a series: every label but the name and `dims`.
fn source_of(labels: &BTreeMap<String, String>, dims: &[&str]) -> String {
    labels
        .iter()
        .filter(|(k, _)| k.as_str() != "__name__" && !dims.contains(&k.as_str()))
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

type PointKey = (String, i64);

/// A capability flag series and the value that allows the filter.
pub(crate) struct CapSeries<'a> {
    pub series: &'a [RawSeries],
    pub dims: &'static [&'static str],
    pub ok: fn(f64) -> bool,
}

/// Joins count, time and capability samples on `(source, timestamp)`. A
/// point needs a sweep end, a sweep start and a process start; a missing
/// count series is 0. Counts add up in integers: every sample, and every
/// sum, is a whole number from 0 to [`MAX_COUNT`], or the store is not
/// showing the probe's series.
pub(crate) fn join_points(
    counts: &[RawSeries],
    count_dims: &[&str],
    times: &[RawSeries],
    caps: &[CapSeries<'_>],
) -> Result<Vec<ScrapePoint>, ApiError> {
    let mut count: BTreeMap<PointKey, u64> = BTreeMap::new();
    for series in counts {
        let source = source_of(&series.labels, count_dims);
        for (at, value) in &series.samples {
            let total = count.entry((source.clone(), *at)).or_default();
            *total = (value.fract() == 0.0 && (0.0..=MAX_COUNT as f64).contains(value))
                .then_some(*value as u64)
                .and_then(|value| total.checked_add(value))
                .filter(|sum| *sum <= MAX_COUNT)
                .ok_or_else(|| bad_store("a count is not a number of units"))?;
        }
    }
    let mut by_name: BTreeMap<&str, BTreeMap<PointKey, f64>> = BTreeMap::new();
    for series in times {
        let name = series.labels.get("__name__").map_or("", String::as_str);
        let source = source_of(&series.labels, &TIME_DIMS);
        let values = by_name.entry(name).or_default();
        for (at, value) in &series.samples {
            values.insert((source.clone(), *at), *value);
        }
    }
    let flags: Vec<BTreeMap<PointKey, bool>> = caps
        .iter()
        .map(|cap| {
            let mut ok: BTreeMap<PointKey, bool> = BTreeMap::new();
            for series in cap.series {
                let source = source_of(&series.labels, cap.dims);
                for (at, value) in &series.samples {
                    let entry = ok.entry((source.clone(), *at)).or_insert(true);
                    *entry = *entry && (cap.ok)(*value);
                }
            }
            ok
        })
        .collect();
    let empty = BTreeMap::new();
    let ends = by_name.get(SWEEP_END).unwrap_or(&empty);
    let starts = by_name.get(SWEEP_START).unwrap_or(&empty);
    let processes = by_name.get(PROCESS_START).unwrap_or(&empty);
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

/// 502 `store_unavailable`: the store's series are not what the probe
/// publishes.
fn bad_store(message: &str) -> ApiError {
    ApiError::new(
        axum::http::StatusCode::BAD_GATEWAY,
        ErrorCode::StoreUnavailable,
        None,
        message.to_string(),
    )
}

/// Collapses scrapes to sweeps: one per `(process start, sweep end)`, as
/// its first scrape saw it, sorted by sweep end. Monitoring replicas at equal
/// or offset times collapse, and the capability of every scrape of a sweep
/// counts; two scrapes that disagree on one sweep's count mean the store
/// mixes probes.
pub(crate) fn sweeps(points: &[ScrapePoint]) -> Result<Vec<Sweep>, ApiError> {
    let mut sorted: Vec<&ScrapePoint> = points.iter().collect();
    sorted.sort_by_key(|p| p.at_ms);
    let mut seen: BTreeMap<(u64, u64), Sweep> = BTreeMap::new();
    for point in sorted {
        // Each sample is finite, but one can be out of range and series
        // summed into one count can overflow.
        if !(0.0..=MAX_COUNT as f64).contains(&point.count) {
            return Err(bad_store("a count is not a number of units"));
        }
        if ![point.end, point.start, point.process]
            .into_iter()
            .all(is_time)
        {
            return Err(bad_store("a sweep or process time is out of range"));
        }
        let key = (point.process.to_bits(), point.end.to_bits());
        match seen.get_mut(&key) {
            Some(sweep) if sweep.count != point.count => {
                return Err(bad_store("inconsistent sources: one sweep has two counts"));
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

/// The request, in seconds.
#[derive(Debug, Clone)]
pub(crate) struct SummaryInput {
    pub family: Family,
    pub start: f64,
    pub end: f64,
    pub base: u64,
    pub n: u64,
    pub direction: Direction,
    pub qs: Vec<f64>,
    pub add_window_seconds: f64,
    pub scrape_interval: f64,
}

impl SummaryInput {
    /// ceil(q·n), or `None` for a `q` with more than six decimal places.
    /// In integers: q·n in f64 can land just above the whole number it is
    /// (0.55 × 100 is 55.00000000000001), and n above 2^53 loses units as
    /// f64.
    fn delta(&self, q: f64) -> Option<u64> {
        let parts = u128::from(q_parts(q)?);
        u64::try_from((parts * u128::from(self.n)).div_ceil(u128::from(Q_PARTS))).ok()
    }

    /// The count that crosses `q`, or `None` when no count can: above the
    /// largest count, below 0 going down, or a `q` with more than six
    /// decimal places.
    fn target(&self, q: f64) -> Option<u64> {
        let delta = self.delta(q)?;
        match self.direction {
            Direction::Up => self.base.checked_add(delta),
            Direction::Down => self.base.checked_sub(delta),
        }
        .filter(|target| *target <= MAX_COUNT)
    }

    /// The target of every requested quantile, or the parameter at fault.
    fn targets(&self) -> Result<Vec<(f64, u64)>, ApiError> {
        self.qs
            .iter()
            .map(|q| match self.target(*q) {
                Some(target) => Ok((*q, target)),
                None => match (self.delta(*q), self.direction) {
                    (None, _) => Err(ApiError::bad_param(
                        "q",
                        format!("q entry {q} has more than six decimal places"),
                    )),
                    (Some(delta), Direction::Down) if delta > self.base => {
                        Err(ApiError::bad_param(
                            "n",
                            "a down target would be below 0: n is larger than base".to_string(),
                        ))
                    }
                    (Some(_), Direction::Up) => Err(ApiError::bad_param(
                        "base",
                        "an up target would exceed the largest count: base + n is too large"
                            .to_string(),
                    )),
                    (Some(_), Direction::Down) => Err(ApiError::bad_param(
                        "base",
                        "a down target would exceed the largest count: base is too large"
                            .to_string(),
                    )),
                },
            })
            .collect()
    }

    fn meets(&self, count: f64, target: u64) -> bool {
        match self.direction {
            Direction::Up => count >= target as f64,
            Direction::Down => count <= target as f64,
        }
    }

    /// Whether `a` is a better count than `b` for the direction.
    fn better(&self, a: f64, b: f64) -> bool {
        match self.direction {
            Direction::Up => a > b,
            Direction::Down => a < b,
        }
    }
}

/// `q` in millionths, or `None` when it has more than six decimal places:
/// when it is not the nearest f64 of a whole number of millionths, as a
/// decimal with at most six places parses to.
fn q_parts(q: f64) -> Option<u64> {
    let parts = (q * Q_PARTS as f64).round();
    ((0.0..=Q_PARTS as f64).contains(&parts) && parts / Q_PARTS as f64 == q).then_some(parts as u64)
}

/// One scrape's time and the sweep end it showed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScrapeMark {
    pub at_ms: i64,
    pub end: f64,
}

/// What the store showed about the window's end and its processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TailState {
    pub final_scrape_seen: bool,
    pub probe_restarts: u32,
    pub incomplete_sweeps: u64,
}

/// The first sweep at or after `start` that meets `target`, with its index
/// in `window`.
fn crossing<'a>(
    input: &SummaryInput,
    window: &[&'a Sweep],
    target: u64,
) -> Option<(usize, &'a Sweep)> {
    window
        .iter()
        .enumerate()
        .find(|(_, s)| input.meets(s.count, target))
        .map(|(i, s)| (i, *s))
}

/// Total length of `[from, to]` minus the parts inside `covered`.
fn uncovered(from: f64, to: f64, covered: (f64, f64)) -> f64 {
    let (lo, hi) = covered;
    let inside = (to.min(hi) - from.max(lo)).max(0.0);
    (to - from).max(0.0) - inside
}

/// Keeps the first, the last and the `keep` indices, and spreads the rest of
/// `max` points evenly by index.
pub(crate) fn thin_curve(sweeps: &[&Sweep], keep: &[usize], max: usize) -> Vec<CurvePoint> {
    let point = |s: &Sweep| CurvePoint {
        at: at_secs(s.end),
        count: s.count.max(0.0) as u64,
    };
    if sweeps.len() <= max {
        return sweeps.iter().map(|s| point(s)).collect();
    }
    let mut chosen: BTreeSet<usize> = keep.iter().copied().filter(|i| *i < sweeps.len()).collect();
    chosen.insert(0);
    chosen.insert(sweeps.len() - 1);
    // Spread the rest evenly; a pass that lands on taken indices leaves
    // room, so the next pass spreads twice as densely.
    let mut stride = sweeps.len() as f64 / max as f64;
    while chosen.len() < max {
        let mut at = 0.0;
        while at < sweeps.len() as f64 && chosen.len() < max {
            chosen.insert(at as usize);
            at += stride;
        }
        stride /= 2.0;
    }
    // Mandatory points are never dropped: requests are admitted only while
    // they fit (see `MAX_QUANTILES`).
    chosen.into_iter().map(|i| point(sweeps[i])).collect()
}

/// The summary of `sweeps` over the request's window.
pub(crate) fn compute(
    input: &SummaryInput,
    sweeps: &[Sweep],
    scrapes: &[ScrapeMark],
    tail: TailState,
) -> SummaryResponse {
    let usable: Vec<&Sweep> = sweeps.iter().filter(|s| s.end <= input.end).collect();
    let window: Vec<&Sweep> = usable
        .iter()
        .copied()
        .filter(|s| s.end >= input.start)
        .collect();
    let before = usable.iter().rev().find(|s| s.end < input.start).copied();

    // Scrape gaps: consecutive scrapes further apart than two intervals that
    // overlap the window, and a missing tail when the final scrape never
    // came.
    let mut marks: Vec<ScrapeMark> = scrapes.to_vec();
    marks.sort_by_key(|m| m.at_ms);
    marks.dedup_by_key(|m| m.at_ms);
    let limit = 2.0 * input.scrape_interval;
    let mut gaps: Vec<ScrapeGap> = marks
        .windows(2)
        .filter(|pair| {
            let (from, to) = (ms_secs(pair[0].at_ms), ms_secs(pair[1].at_ms));
            to - from > limit && to > input.start && from < input.end
        })
        .map(|pair| ScrapeGap {
            from: at_secs(ms_secs(pair[0].at_ms)),
            to: at_secs(ms_secs(pair[1].at_ms)),
            seconds: ms_secs(pair[1].at_ms) - ms_secs(pair[0].at_ms),
            new_sweep_across: pair[0].end != pair[1].end,
        })
        .collect();
    if !tail.final_scrape_seen {
        let last = marks.last().map_or(input.start, |m| ms_secs(m.at_ms));
        if input.end - last > limit {
            gaps.push(ScrapeGap {
                from: at_secs(last),
                to: at_secs(input.end),
                seconds: input.end - last,
                new_sweep_across: false,
            });
        }
    }

    // Requests are validated with `targets`; a direct caller's unreachable
    // target simply never crosses.
    let unreachable = match input.direction {
        Direction::Up => u64::MAX,
        Direction::Down => 0,
    };
    let targets: Vec<(f64, u64)> = input
        .qs
        .iter()
        .map(|q| (*q, input.target(*q).unwrap_or(unreachable)))
        .collect();
    let mut crossings: Vec<Crossing> = targets
        .iter()
        .map(|(q, target)| Crossing {
            q: *q,
            target: *target,
            reached_at: None,
            seconds: None,
            worst_count_after: None,
            scrape_gap_before: false,
        })
        .collect();
    let mut response = SummaryResponse {
        family: input.family,
        start: at_secs(input.start),
        end: at_secs(input.end),
        base: input.base,
        n: input.n,
        direction: input.direction,
        crossings: Vec::new(),
        final_count: None,
        final_sweep_at: None,
        units_per_minute: UnitsPerMinute {
            sustained: None,
            peak_10m: None,
        },
        max_zero_progress_seconds: None,
        max_sweep_gap_seconds: None,
        max_sweep_duration_seconds: None,
        incomplete_sweeps: tail.incomplete_sweeps,
        scrape_gaps_total: gaps.len() as u32,
        scrape_gaps: Vec::new(),
        uncovered_scrape_gap_seconds: 0.0,
        add_window_seconds: input.add_window_seconds,
        timing_error_seconds: None,
        final_scrape_seen: tail.final_scrape_seen,
        probe_restarts: tail.probe_restarts,
        sweeps_seen: window.len() as u32,
        curve: Vec::new(),
        worst_count_after_first_crossing: None,
    };
    let Some(last) = usable.last() else {
        // No usable sweep: every crossing keeps its entry, and every value
        // stays null, never 0.
        response.crossings = crossings;
        response.scrape_gaps = gaps.into_iter().take(LISTED_SCRAPE_GAPS).collect();
        return response;
    };

    let mut crossed: Vec<usize> = Vec::new();
    for entry in &mut crossings {
        let Some((index, sweep)) = crossing(input, &window, entry.target) else {
            continue;
        };
        crossed.push(index);
        entry.reached_at = Some(at_secs(sweep.end));
        entry.seconds = Some(sweep.end - input.start);
        let after = window[index..].iter().map(|s| s.count);
        entry.worst_count_after = match input.direction {
            Direction::Up => after.fold(None, |w: Option<f64>, c| Some(w.map_or(c, |w| w.min(c)))),
            Direction::Down => {
                after.fold(None, |w: Option<f64>, c| Some(w.map_or(c, |w| w.max(c))))
            }
        }
        .map(|c| c.max(0.0) as u64);
        entry.scrape_gap_before = gaps.iter().any(|gap| {
            let (from, to) = (secs_of(gap.from), secs_of(gap.to));
            to > input.start && from < sweep.end
        });
    }
    response.worst_count_after_first_crossing = crossings
        .iter()
        .find(|c| c.reached_at.is_some())
        .and_then(|c| c.worst_count_after);

    // Sweep gaps tile the window: from the last sweep before it (or its
    // start) through every sweep end in it, to its end.
    let first_gap_start = before.map_or(input.start, |s| s.end);
    let mut edges = vec![first_gap_start];
    edges.extend(window.iter().map(|s| s.end));
    edges.push(input.end);
    let max_gap = edges
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .fold(0.0, f64::max);
    let max_duration = before
        .into_iter()
        .chain(window.iter().copied())
        .map(|s| s.end - s.start)
        .fold(0.0, f64::max);
    // The sweep gaps cover [first_gap_start, end], so a scrape gap inside the
    // window lies inside some sweep gap: a sweep it hid only widens the gap
    // between the sweep ends seen. Only a scrape gap outside that span (none
    // while a sweep precedes the window) adds to the timing error. Folded
    // from +0.0: an empty f64 `sum` is -0.0, which serializes as "-0.0".
    let uncovered_seconds = gaps.iter().fold(0.0, |total, gap| {
        let from = secs_of(gap.from).max(input.start);
        let to = secs_of(gap.to).min(input.end);
        total + uncovered(from, to, (first_gap_start, input.end))
    });

    response.final_count = Some(last.count.max(0.0) as u64);
    response.final_sweep_at = Some(at_secs(last.end));
    response.max_sweep_gap_seconds = Some(max_gap);
    response.max_sweep_duration_seconds = Some(max_duration);
    response.uncovered_scrape_gap_seconds = uncovered_seconds;
    response.timing_error_seconds =
        Some(max_gap + max_duration + input.add_window_seconds + uncovered_seconds);

    let time_of = |q: f64| {
        input
            .target(q)
            .and_then(|target| crossing(input, &window, target))
            .map(|(_, s)| s.end)
    };
    response.units_per_minute.sustained = match (time_of(0.1), time_of(0.9)) {
        (Some(t1), Some(t9)) if t9 > t1 => Some(0.8 * input.n as f64 / ((t9 - t1) / 60.0)),
        _ => None,
    };
    let span: Vec<&Sweep> = before.into_iter().chain(window.iter().copied()).collect();
    let mut peak: Option<f64> = None;
    for (i, a) in span.iter().enumerate() {
        for b in &span[i + 1..] {
            if b.end - a.end > PEAK_WINDOW {
                break;
            }
            let change = match input.direction {
                Direction::Up => b.count - a.count,
                Direction::Down => a.count - b.count,
            };
            peak = Some(peak.map_or(change, |p: f64| p.max(change)));
        }
    }
    response.units_per_minute.peak_10m = peak.map(|p| p.max(0.0) / 10.0);

    // Zero progress: the longest time without a new best count, from the
    // window's start to the last crossing (or its end).
    let all_reached = crossings.iter().all(|c| c.reached_at.is_some()) && !crossings.is_empty();
    let stop = if all_reached {
        crossings
            .iter()
            .filter_map(|c| c.seconds)
            .fold(0.0, f64::max)
            + input.start
    } else {
        input.end
    };
    // Without a sweep before the window, the count it starts from is `base`.
    let mut best = before.map_or(input.base as f64, |s| s.count);
    let mut last_progress = input.start;
    let mut longest: f64 = 0.0;
    for sweep in window.iter().filter(|s| s.end <= stop) {
        if input.better(sweep.count, best) {
            longest = longest.max(sweep.end - last_progress);
            last_progress = sweep.end;
            best = sweep.count;
        }
    }
    longest = longest.max(stop - last_progress);
    response.max_zero_progress_seconds = Some(longest);

    response.curve = thin_curve(&window, &crossed, CURVE_POINTS);
    response.crossings = crossings;
    response.scrape_gaps = gaps.into_iter().take(LISTED_SCRAPE_GAPS).collect();
    response
}

fn secs_of(at: DateTime<Utc>) -> f64 {
    at.timestamp_millis() as f64 / 1000.0
}

/// Incomplete sweeps in the run `(start, end]`: per probe process, the
/// counter's rise over the run's scrapes, from its last scrape at or before
/// `start` (0 for a process that started inside the run). Monitoring
/// replicas report the same counter of a process at their own times; the
/// latest sample at or before `start` and the highest in the run count,
/// whichever replica took them.
///
/// Attribution is by scrape time, so it is approximate by the bracketing
/// scrape gap at each edge, normally one scrape interval, never exact: an
/// incomplete sweep between the last scrape before `start` and `start`
/// shows first in the run and is counted, and one between the last scrape
/// before `end` and `end` shows only after it and is left out. Scrape gaps
/// are reported separately.
pub(crate) fn incomplete_sweeps(times: &[RawSeries], start: f64, end: f64) -> u64 {
    let mut process_at: BTreeMap<PointKey, f64> = BTreeMap::new();
    for series in times {
        if series.labels.get("__name__").map(String::as_str) == Some(PROCESS_START) {
            let source = source_of(&series.labels, &TIME_DIMS);
            for (at, value) in &series.samples {
                process_at.insert((source.clone(), *at), *value);
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
        let is_incomplete = series.labels.get("__name__").map(String::as_str) == Some(SWEEPS_TOTAL)
            && series.labels.get("result").map(String::as_str) == Some("incomplete");
        if !is_incomplete {
            continue;
        }
        let source = source_of(&series.labels, &TIME_DIMS);
        for (at, value) in &series.samples {
            let Some(process) = process_at.get(&(source.clone(), *at)) else {
                continue;
            };
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
        .sum()
}

/// Probe restarts in the run `(start, end]`: processes that started in it
/// after an earlier process of the same probe. Read from the raw process
/// start samples, so a process that never completed a sweep still counts.
/// A restart before the run or after it is not the run's; replicas report
/// the same start, so each process counts once.
pub(crate) fn probe_restarts(times: &[RawSeries], start: f64, end: f64) -> u32 {
    let processes: BTreeSet<u64> = times
        .iter()
        .filter(|s| s.labels.get("__name__").map(String::as_str) == Some(PROCESS_START))
        .flat_map(|s| s.samples.iter().map(|(_, value)| value.to_bits()))
        .collect();
    let mut starts: Vec<f64> = processes.into_iter().map(f64::from_bits).collect();
    starts.sort_by(f64::total_cmp);
    starts
        .iter()
        .skip(1)
        .filter(|p| **p > start && **p <= end)
        .count() as u32
}

#[cfg(test)]
mod tests {
    use carbide_site_health_probe_api::{DEFAULT_Q, parse_q};

    use super::*;

    const T0: f64 = 1_000_000.0;

    fn sweep(end: f64, count: f64) -> Sweep {
        Sweep {
            process: T0 - 10_000.0,
            start: end - 10.0,
            end,
            count,
            first_seen_ms: ((end + 2.0) * 1000.0) as i64,
            capable: Some(true),
        }
    }

    fn input(
        start: f64,
        end: f64,
        base: u64,
        n: u64,
        direction: Direction,
        qs: &[f64],
    ) -> SummaryInput {
        SummaryInput {
            family: Family::Host,
            start,
            end,
            base,
            n,
            direction,
            qs: qs.to_vec(),
            add_window_seconds: 0.0,
            scrape_interval: 5.0,
        }
    }

    /// One scrape every 5 s from `from` to `to`, each showing the newest
    /// sweep that ended before it.
    fn scrapes_of(sweeps: &[Sweep], from: f64, to: f64) -> Vec<ScrapeMark> {
        let mut out = Vec::new();
        let mut at = from;
        while at <= to {
            if let Some(s) = sweeps.iter().rev().find(|s| s.end <= at) {
                out.push(ScrapeMark {
                    at_ms: (at * 1000.0) as i64,
                    end: s.end,
                });
            }
            at += 5.0;
        }
        out
    }

    fn seen() -> TailState {
        TailState {
            final_scrape_seen: true,
            ..TailState::default()
        }
    }

    /// Sweeps every 20 s from T0, counting `f(i)`.
    fn ramp(len: usize, f: impl Fn(usize) -> f64) -> Vec<Sweep> {
        (0..len)
            .map(|i| sweep(T0 + 20.0 * i as f64, f(i)))
            .collect()
    }

    #[test]
    fn summary_up_crossings() {
        let sweeps = ramp(12, |i| (i * 10) as f64);
        let input = input(
            T0 - 5.0,
            T0 + 240.0,
            0,
            100,
            Direction::Up,
            &[0.5, 0.9, 1.0],
        );
        let reply = compute(
            &input,
            &sweeps,
            &scrapes_of(&sweeps, T0 - 5.0, T0 + 240.0),
            seen(),
        );
        let reached: Vec<_> = reply
            .crossings
            .iter()
            .map(|c| (c.target, c.seconds))
            .collect();
        assert_eq!(
            reached,
            vec![(50, Some(105.0)), (90, Some(185.0)), (100, Some(205.0))]
        );
        assert_eq!(reply.final_count, Some(110));
        assert_eq!(reply.sweeps_seen, 12);
        assert_eq!(reply.crossings[0].reached_at, Some(at_secs(T0 + 100.0)));
    }

    #[test]
    fn summary_down_crossings() {
        let sweeps = ramp(12, |i| 100.0 - (i * 10) as f64);
        let input = input(T0 - 5.0, T0 + 240.0, 100, 100, Direction::Down, &[0.5, 1.0]);
        let reply = compute(
            &input,
            &sweeps,
            &scrapes_of(&sweeps, T0 - 5.0, T0 + 240.0),
            seen(),
        );
        assert_eq!(reply.crossings[0].target, 50);
        assert_eq!(reply.crossings[0].reached_at, Some(at_secs(T0 + 100.0)));
        assert_eq!(reply.crossings[1].target, 0);
        assert_eq!(reply.crossings[1].reached_at, Some(at_secs(T0 + 200.0)));
        assert_eq!(reply.crossings[1].worst_count_after, Some(0));
    }

    #[test]
    fn summary_worst_count_after_crossing() {
        // Crosses 50 at sweep 5, dips to 30, recovers.
        let counts = [0.0, 10.0, 20.0, 30.0, 40.0, 50.0, 30.0, 60.0, 70.0];
        let sweeps = ramp(counts.len(), |i| counts[i]);
        let input = input(T0, T0 + 200.0, 0, 100, Direction::Up, &[0.5, 0.7]);
        let reply = compute(
            &input,
            &sweeps,
            &scrapes_of(&sweeps, T0, T0 + 200.0),
            seen(),
        );
        assert_eq!(reply.crossings[0].worst_count_after, Some(30));
        assert_eq!(reply.crossings[1].worst_count_after, Some(70));
        assert_eq!(reply.worst_count_after_first_crossing, Some(30));
    }

    #[test]
    fn summary_timing_error_components() {
        // A sweep 50 s before the window, then ends at +10, +30, +90 (a 60 s
        // gap), the window ends at +100. The longest sweep took 25 s.
        let mut sweeps = vec![
            sweep(T0 - 50.0, 0.0),
            sweep(T0 + 10.0, 5.0),
            sweep(T0 + 30.0, 9.0),
            sweep(T0 + 90.0, 10.0),
        ];
        sweeps[2].start = T0 + 5.0;
        let mut input = input(T0, T0 + 100.0, 0, 10, Direction::Up, &[1.0]);
        input.add_window_seconds = 12.0;
        let reply = compute(
            &input,
            &sweeps,
            &scrapes_of(&sweeps, T0 - 60.0, T0 + 100.0),
            seen(),
        );
        assert_eq!(
            reply.max_sweep_gap_seconds,
            Some(60.0),
            "the first gap starts at -50"
        );
        assert_eq!(reply.max_sweep_duration_seconds, Some(25.0));
        assert_eq!(reply.uncovered_scrape_gap_seconds, 0.0);
        assert_eq!(reply.timing_error_seconds, Some(60.0 + 25.0 + 12.0));
        assert_eq!(reply.add_window_seconds, 12.0);
    }

    #[test]
    fn no_scrape_gap_is_positive_zero() {
        let sweeps = ramp(10, |i| i as f64);
        let input = input(T0, T0 + 200.0, 0, 9, Direction::Up, &[1.0]);
        let reply = compute(
            &input,
            &sweeps,
            &scrapes_of(&sweeps, T0 - 20.0, T0 + 200.0),
            seen(),
        );
        assert_eq!(reply.scrape_gaps_total, 0);
        assert!(
            reply.uncovered_scrape_gap_seconds.is_sign_positive(),
            "-0.0 with no gaps"
        );
        let json = serde_json::to_value(&reply).expect("serializes");
        assert_eq!(
            json["uncoveredScrapeGapSeconds"].to_string(),
            "0.0",
            "the wire value"
        );
    }

    #[test]
    fn summary_flags_scrape_gap_before_crossing() {
        let sweeps = ramp(10, |i| (i * 10) as f64);
        let mut scrapes = scrapes_of(&sweeps, T0, T0 + 200.0);
        // No scrape between +100 and +130.
        scrapes.retain(|m| {
            !(m.at_ms > ((T0 + 100.0) * 1000.0) as i64 && m.at_ms < ((T0 + 130.0) * 1000.0) as i64)
        });
        let input = input(T0, T0 + 200.0, 0, 100, Direction::Up, &[0.3, 0.8]);
        let reply = compute(&input, &sweeps, &scrapes, seen());
        assert_eq!(reply.scrape_gaps_total, 1);
        let gap = &reply.scrape_gaps[0];
        assert_eq!(gap.seconds, 30.0);
        assert!(gap.new_sweep_across, "sweep +120 ended inside the gap");
        assert!(
            !reply.crossings[0].scrape_gap_before,
            "30 crossed at +60, before the gap"
        );
        assert!(
            reply.crossings[1].scrape_gap_before,
            "80 crossed at +160, after it"
        );
    }

    fn series(name: &str, labels: &[(&str, &str)], samples: &[(i64, f64)]) -> RawSeries {
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

    /// The count, sweep and process series of `scrapes` from one source:
    /// `(at_ms, count, sweep end, process start)`.
    fn source_series(
        instance: &str,
        scrapes: &[(i64, f64, f64, f64)],
    ) -> (Vec<RawSeries>, Vec<RawSeries>) {
        let source = [("instance", instance), ("job", "j")];
        let count_labels = [
            ("type", "host"),
            ("stage", "ready"),
            ("state", "Ready"),
            source[0],
            source[1],
        ];
        let time_labels = [
            ("reader", "devices"),
            ("family", "host"),
            source[0],
            source[1],
        ];
        let counts = vec![series(
            "carbide_site_health_probe_units",
            &count_labels,
            &scrapes.iter().map(|s| (s.0, s.1)).collect::<Vec<_>>(),
        )];
        let times = vec![
            series(
                SWEEP_END,
                &time_labels,
                &scrapes.iter().map(|s| (s.0, s.2)).collect::<Vec<_>>(),
            ),
            series(
                SWEEP_START,
                &time_labels,
                &scrapes
                    .iter()
                    .map(|s| (s.0, s.2 - 10.0))
                    .collect::<Vec<_>>(),
            ),
            series(
                PROCESS_START,
                &source,
                &scrapes.iter().map(|s| (s.0, s.3)).collect::<Vec<_>>(),
            ),
        ];
        (counts, times)
    }

    /// One source's scrapes: `(at_ms, count, sweep end, process start)`.
    type Scrapes<'a> = &'a [(i64, f64, f64, f64)];

    fn join(sources: &[(&str, Scrapes<'_>)]) -> Vec<ScrapePoint> {
        let mut counts = Vec::new();
        let mut times = Vec::new();
        for (instance, scrapes) in sources {
            let (c, t) = source_series(instance, scrapes);
            counts.extend(c);
            times.extend(t);
        }
        join_points(&counts, &["type", "stage", "state"], &times, &[]).expect("joins")
    }

    #[test]
    fn summary_drops_repeated_sweep_times() {
        // Sweep ends 100 and 120, each scraped four times.
        let scrapes: Vec<(i64, f64, f64, f64)> = (0..8)
            .map(|i| {
                let at = 101_000 + i * 5_000;
                let end = if at < 121_000 { 100.0 } else { 120.0 };
                (at, if end == 100.0 { 4.0 } else { 6.0 }, end, 1.0)
            })
            .collect();
        let points = join(&[("a", &scrapes)]);
        assert_eq!(points.len(), 8);
        let found = sweeps(&points).expect("consistent");
        assert_eq!(
            found
                .iter()
                .map(|s| (s.end, s.count, s.first_seen_ms))
                .collect::<Vec<_>>(),
            vec![(100.0, 4.0, 101_000), (120.0, 6.0, 121_000)]
        );
    }

    #[test]
    fn summary_two_processes_never_summed() {
        // Process 1 counts to 8, restarts, and process 2 counts from 5.
        let scrapes = [
            (100_000, 7.0, 95.0, 1.0),
            (105_000, 8.0, 104.0, 1.0),
            (130_000, 5.0, 125.0, 120.0),
            (135_000, 9.0, 133.0, 120.0),
        ];
        let points = join(&[("a", &scrapes)]);
        let found = sweeps(&points).expect("consistent");
        assert_eq!(
            found.iter().map(|s| s.count).collect::<Vec<_>>(),
            vec![7.0, 8.0, 5.0, 9.0],
            "merged by time, never summed"
        );
        let input = input(90.0, 140.0, 0, 9, Direction::Up, &[1.0]);
        let marks: Vec<_> = points
            .iter()
            .map(|p| ScrapeMark {
                at_ms: p.at_ms,
                end: p.end,
            })
            .collect();
        let reply = compute(&input, &found, &marks, seen());
        assert_eq!(reply.crossings[0].reached_at, Some(at_secs(133.0)));
        assert_eq!(reply.final_count, Some(9));
    }

    #[test]
    fn summary_ha_scrapers_equal_and_offset_not_double_counted() {
        let scrapes_a = [(100_000, 3.0, 95.0, 1.0), (105_000, 4.0, 104.0, 1.0)];
        // Replica b scrapes the same probe 2 s later, and once at the same
        // time.
        let scrapes_b = [(102_000, 3.0, 95.0, 1.0), (105_000, 4.0, 104.0, 1.0)];
        let points = join(&[("a", &scrapes_a), ("b", &scrapes_b)]);
        assert_eq!(points.len(), 4);
        let found = sweeps(&points).expect("consistent");
        assert_eq!(
            found.iter().map(|s| s.count).collect::<Vec<_>>(),
            vec![3.0, 4.0]
        );
        assert_eq!(found[0].first_seen_ms, 100_000);

        let disagree = [(102_000, 99.0, 95.0, 1.0)];
        let points = join(&[("a", &scrapes_a), ("b", &disagree)]);
        let err = sweeps(&points).expect_err("two counts for one sweep");
        assert_eq!(err.status(), axum::http::StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn sweep_capability_merges_every_scrape() {
        // Replicas a and b scrape one sweep, each with its own flag sample.
        let flag = |instance: &str, value: Option<f64>| -> Vec<RawSeries> {
            value
                .map(|v| {
                    series(
                        "carbide_site_health_probe_rest_totals_available",
                        &[("resource", "rack"), ("instance", instance), ("job", "j")],
                        &[(if instance == "a" { 100_000 } else { 102_000 }, v)],
                    )
                })
                .into_iter()
                .collect()
        };
        for (a, b, want) in [
            (Some(1.0), Some(1.0), Some(true)),
            (Some(1.0), Some(0.0), Some(false)),
            (Some(0.0), Some(1.0), Some(false)),
            (Some(1.0), None, None),
            (None, Some(1.0), None),
        ] {
            let (mut counts, mut times) = source_series("a", &[(100_000, 3.0, 95.0, 1.0)]);
            let (c, t) = source_series("b", &[(102_000, 3.0, 95.0, 1.0)]);
            counts.extend(c);
            times.extend(t);
            let mut flags = flag("a", a);
            flags.extend(flag("b", b));
            let points = join_points(
                &counts,
                &["type", "stage", "state"],
                &times,
                &[CapSeries {
                    series: &flags,
                    dims: &["resource"],
                    ok: |v| v == 1.0,
                }],
            )
            .expect("joins");
            let found = sweeps(&points).expect("one count");
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].capable, want, "a {a:?}, b {b:?}");
        }
    }

    #[test]
    fn summary_ha_counters_not_double_counted() {
        let start = 200.0;
        let counter = |instance: &str, samples: &[(i64, f64, f64)]| {
            let labels = [("instance", instance), ("job", "j")];
            vec![
                series(
                    SWEEPS_TOTAL,
                    &[
                        ("reader", "devices"),
                        ("family", "host"),
                        ("result", "incomplete"),
                        labels[0],
                        labels[1],
                    ],
                    &samples.iter().map(|s| (s.0, s.1)).collect::<Vec<_>>(),
                ),
                series(
                    PROCESS_START,
                    &labels,
                    &samples.iter().map(|s| (s.0, s.2)).collect::<Vec<_>>(),
                ),
            ]
        };
        // Process 1 (started at 1): 2 incomplete before the window, 5 by
        // its end. Process 2 starts inside the window and reaches 2. Two
        // replicas see both.
        let samples = [
            (190_000, 2.0, 1.0),
            (210_000, 4.0, 1.0),
            (220_000, 5.0, 1.0),
            (260_000, 1.0, 250.0),
            (270_000, 2.0, 250.0),
        ];
        let mut times = counter("a", &samples);
        times.extend(counter("b", &samples));
        assert_eq!(incomplete_sweeps(&times, start, 300.0), 3 + 2);

        // The edges: scrapes at 90 (0) and 105 (1) around a start at 100.
        // An incomplete sweep at 95, before the run, shows first at 105 and
        // is counted. One at 140, before an end at 142, shows first at 145
        // and is left out.
        let edges = counter(
            "a",
            &[(90_000, 0.0, 1.0), (105_000, 1.0, 1.0), (145_000, 2.0, 1.0)],
        );
        assert_eq!(incomplete_sweeps(&edges, 100.0, 142.0), 1);
    }

    #[test]
    fn summary_curve_keeps_crossings_max_200() {
        let sweeps = ramp(1000, |i| i as f64);
        let input = input(T0, T0 + 20_000.0, 0, 1000, Direction::Up, &[0.333, 0.777]);
        let reply = compute(
            &input,
            &sweeps,
            &scrapes_of(&sweeps, T0, T0 + 20_000.0),
            seen(),
        );
        assert_eq!(reply.curve.len(), CURVE_POINTS);
        assert_eq!(reply.curve.first().unwrap().count, 0);
        assert_eq!(reply.curve.last().unwrap().count, 999);
        for crossing in &reply.crossings {
            let at = crossing.reached_at.expect("reached");
            assert!(
                reply.curve.iter().any(|p| p.at == at),
                "crossing at {at} kept"
            );
        }
        let short = ramp(5, |i| i as f64);
        let reply = compute(&input, &short, &scrapes_of(&short, T0, T0 + 100.0), seen());
        assert_eq!(reply.curve.len(), 5);
    }

    #[test]
    fn thin_curve_never_drops_a_mandatory_point() {
        let sweeps = ramp(50, |i| i as f64);
        let window: Vec<&Sweep> = sweeps.iter().collect();
        let keep: Vec<usize> = (0..12).map(|i| i * 4).collect();
        let curve = thin_curve(&window, &keep, 10);
        assert_eq!(curve.len(), 13, "12 kept points and the last");
        for i in keep {
            assert!(curve.iter().any(|p| p.count == i as u64), "point {i} kept");
        }
    }

    #[test]
    fn summary_units_per_minute() {
        // One more unit every 60 s for 100 minutes.
        let sweeps: Vec<Sweep> = (0..=100)
            .map(|i| sweep(T0 + 60.0 * i as f64, i as f64))
            .collect();
        let input = input(T0, T0 + 6000.0, 0, 100, Direction::Up, &[0.5]);
        let reply = compute(
            &input,
            &sweeps,
            &scrapes_of(&sweeps, T0, T0 + 6000.0),
            seen(),
        );
        // t(0.1) = 10 min, t(0.9) = 90 min.
        assert_eq!(reply.units_per_minute.sustained, Some(1.0));
        assert_eq!(reply.units_per_minute.peak_10m, Some(1.0));
    }

    #[test]
    fn summary_zero_progress_gap() {
        // Progress every 20 s, then nothing new from +100 to +400.
        let counts = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mut sweeps = ramp(counts.len(), |i| counts[i]);
        for i in 0..14 {
            sweeps.push(sweep(T0 + 120.0 + 20.0 * i as f64, 6.0));
        }
        sweeps.push(sweep(T0 + 400.0, 10.0));
        let input = input(T0, T0 + 500.0, 0, 10, Direction::Up, &[1.0]);
        let reply = compute(
            &input,
            &sweeps,
            &scrapes_of(&sweeps, T0, T0 + 500.0),
            seen(),
        );
        assert_eq!(reply.max_zero_progress_seconds, Some(300.0));
    }

    #[test]
    fn summary_only_incomplete_sweeps_is_null() {
        let input = input(T0, T0 + 100.0, 0, 10, Direction::Up, &[0.5, 1.0]);
        let tail = TailState {
            final_scrape_seen: true,
            probe_restarts: 0,
            incomplete_sweeps: 6,
        };
        let reply = compute(&input, &[], &[], tail);
        assert_eq!(reply.crossings.len(), 2, "every crossing keeps its entry");
        assert!(reply.crossings.iter().all(|c| c.reached_at.is_none()
            && c.seconds.is_none()
            && c.worst_count_after.is_none()));
        assert_eq!(reply.final_count, None);
        assert_eq!(reply.max_sweep_gap_seconds, None);
        assert_eq!(reply.timing_error_seconds, None);
        assert!(reply.curve.is_empty());
        assert_eq!(reply.incomplete_sweeps, 6);
    }

    #[test]
    fn summary_uncovered_window_is_null() {
        // The only sweeps end after the window.
        let sweeps = vec![sweep(T0 + 200.0, 10.0)];
        let input = input(T0, T0 + 100.0, 0, 10, Direction::Up, &[1.0]);
        let reply = compute(
            &input,
            &sweeps,
            &scrapes_of(&sweeps, T0 + 200.0, T0 + 210.0),
            seen(),
        );
        assert_eq!(reply.timing_error_seconds, None);
        assert_eq!(reply.max_sweep_gap_seconds, None);
        assert_eq!(reply.crossings[0].reached_at, None);
    }

    #[test]
    fn uncovered_scrape_gap_zero_when_covered() {
        let sweeps = ramp(10, |i| i as f64);
        let mut scrapes = scrapes_of(&sweeps, T0 - 20.0, T0 + 200.0);
        scrapes.retain(|m| {
            !(m.at_ms > ((T0 + 40.0) * 1000.0) as i64 && m.at_ms < ((T0 + 90.0) * 1000.0) as i64)
        });
        let input = input(T0 - 10.0, T0 + 200.0, 0, 9, Direction::Up, &[1.0]);
        let reply = compute(&input, &sweeps, &scrapes, seen());
        assert_eq!(reply.scrape_gaps_total, 1);
        assert_eq!(
            reply.uncovered_scrape_gap_seconds, 0.0,
            "inside the sweep gaps"
        );
        assert_eq!(uncovered(10.0, 30.0, (20.0, 100.0)), 10.0);
        assert_eq!(uncovered(30.0, 40.0, (20.0, 100.0)), 0.0);
    }

    #[test]
    fn zero_progress_counts_from_base() {
        // No sweep before the window: the first sweeps show `base`, which
        // is no progress.
        let up = vec![
            sweep(T0 + 20.0, 0.0),
            sweep(T0 + 40.0, 0.0),
            sweep(T0 + 60.0, 1.0),
        ];
        let input_up = input(T0, T0 + 100.0, 0, 1, Direction::Up, &[1.0]);
        let reply = compute(&input_up, &up, &scrapes_of(&up, T0, T0 + 100.0), seen());
        assert_eq!(reply.max_zero_progress_seconds, Some(60.0));

        let down = vec![
            sweep(T0 + 20.0, 10.0),
            sweep(T0 + 40.0, 10.0),
            sweep(T0 + 60.0, 9.0),
        ];
        let input_down = input(T0, T0 + 100.0, 10, 1, Direction::Down, &[1.0]);
        let reply = compute(
            &input_down,
            &down,
            &scrapes_of(&down, T0, T0 + 100.0),
            seen(),
        );
        assert_eq!(reply.max_zero_progress_seconds, Some(60.0));
    }

    #[test]
    fn count_out_of_range_is_store_error() {
        let times = [
            series(SWEEP_END, &[], &[(1_000, 1.0)]),
            series(SWEEP_START, &[], &[(1_000, 0.5)]),
            series(PROCESS_START, &[], &[(1_000, 0.1)]),
        ];
        // Count series of one source, by state.
        let join = |values: &[f64]| {
            let counts: Vec<RawSeries> = values
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    series(
                        "carbide_site_health_probe_units",
                        &[("state", &i.to_string())],
                        &[(1_000, *v)],
                    )
                })
                .collect();
            join_points(&counts, &["state"], &times, &[])
        };
        let max = MAX_COUNT as f64;
        let points = join(&[max - 1.0, 1.0]).expect("2^53 is a count");
        assert_eq!(points[0].count, max);
        // 2^53 + 1 rounds to 2^53 in f64, so the sum is checked in integers.
        assert_eq!(max + 1.0, max);
        for values in [
            &[max, 1.0][..],
            &[1e308, 1e308],
            &[1e308],
            &[max + 2.0],
            &[-1.0],
            &[0.5],
        ] {
            let err = join(values).expect_err("not a count");
            assert_eq!(
                err.status(),
                axum::http::StatusCode::BAD_GATEWAY,
                "{values:?}"
            );
        }

        // Points built some other way are checked again.
        let mut points = join(&[1.0]).expect("joins");
        points[0].count = 1e308;
        let err = sweeps(&points).expect_err("not a count");
        assert_eq!(err.status(), axum::http::StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn out_of_range_sweep_time_is_store_error() {
        let counts = [series(
            "carbide_site_health_probe_units",
            &[],
            &[(1_000, 1.0)],
        )];
        let times = |end: f64, start: f64, process: f64| {
            [
                series(SWEEP_END, &[], &[(1_000, end)]),
                series(SWEEP_START, &[], &[(1_000, start)]),
                series(PROCESS_START, &[], &[(1_000, process)]),
            ]
        };
        let good = join_points(&counts, &[], &times(1.0, 0.5, 0.1), &[]).expect("joins");
        assert!(sweeps(&good).is_ok());
        for (end, start, process) in [
            (1e308, 0.5, 0.1),
            (-1.0, 0.5, 0.1),
            (1.0, 1e13, 0.1),
            (1.0, 0.5, -5.0),
            (1.0, 0.5, 1e308),
        ] {
            let points =
                join_points(&counts, &[], &times(end, start, process), &[]).expect("joins");
            let err = sweeps(&points).expect_err("not a time");
            assert_eq!(
                err.status(),
                axum::http::StatusCode::BAD_GATEWAY,
                "{end} {start} {process}"
            );
        }
    }

    #[test]
    fn target_is_exact_ceil() {
        // q·n in f64 lands just above a whole number for these.
        let up = |q: f64, n: u64| input(T0, T0 + 1.0, 0, n, Direction::Up, &[q]).target(q);
        assert_eq!(up(0.55, 100), Some(55));
        assert_eq!(up(0.56, 4500), Some(2520));
        assert_eq!(up(0.07, 4500), Some(315));
        assert_eq!(up(0.07, 100), Some(7));
        let down = |q: f64, base: u64, n: u64| {
            input(T0, T0 + 1.0, base, n, Direction::Down, &[q]).target(q)
        };
        assert_eq!(down(0.55, 4500, 100), Some(4445));
        assert_eq!(down(0.07, 100, 100), Some(93));

        // The crossing is at the exact target, not one past it.
        let ramp_up = ramp(10, |i| i as f64);
        let input_up = input(T0 - 5.0, T0 + 200.0, 0, 100, Direction::Up, &[0.07]);
        let reply = compute(
            &input_up,
            &ramp_up,
            &scrapes_of(&ramp_up, T0, T0 + 200.0),
            seen(),
        );
        assert_eq!(reply.crossings[0].target, 7);
        assert_eq!(reply.crossings[0].reached_at, Some(at_secs(T0 + 140.0)));

        // Against integer math, for the default quantiles and the ones
        // above, all whole thousandths.
        let mut milli: Vec<u64> = parse_q(DEFAULT_Q)
            .unwrap()
            .iter()
            .map(|q| (q * 1000.0).round() as u64)
            .collect();
        milli.extend([550, 560, 70, 1]);
        for n in 0..=20_000 {
            for m in &milli {
                let q = *m as f64 / 1000.0;
                assert_eq!(up(q, n), Some((m * n).div_ceil(1000)), "q={q} n={n}");
            }
        }
        // Just above a whole number is the next count, at six places.
        assert_eq!(up(0.500001, 100), Some(51));
        assert_eq!(down(0.500001, 100, 100), Some(49));
        assert_eq!(up(0.500001, 1_000_000), Some(500_001));

        // More places are refused, never rounded to a nearby target.
        assert_eq!(up(0.500000000000001, 100), None);
        assert_eq!(down(0.5000001, 100, 100), None);
        let precise = input(T0, T0 + 1.0, 0, 100, Direction::Up, &[0.5000001]);
        let err = precise.targets().expect_err("too precise");
        assert_eq!(err.status(), axum::http::StatusCode::BAD_REQUEST);

        // n past 2^53 keeps every unit; no target is past it.
        let huge: u64 = 9_007_199_254_740_993;
        assert_eq!(down(1.0, huge, huge), Some(0));
        assert_eq!(up(0.5, huge), Some(4_503_599_627_370_497));
        assert_eq!(up(1.0, huge), None);
        assert_eq!(up(1.0, MAX_COUNT), Some(MAX_COUNT));
        let past = input(T0, T0 + 1.0, MAX_COUNT + 1, 1, Direction::Down, &[1.0]);
        assert_eq!(past.target(1.0), Some(MAX_COUNT));
        let past = input(T0, T0 + 1.0, MAX_COUNT + 2, 1, Direction::Down, &[1.0]);
        assert_eq!(past.target(1.0), None);
        let err = past.targets().expect_err("too large");
        assert_eq!(err.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn at_secs_never_panics() {
        assert_eq!(
            at_secs(1_761_213_605.5).timestamp_millis(),
            1_761_213_605_500
        );
        for past in [-1.0, f64::NEG_INFINITY, f64::NAN] {
            assert_eq!(at_secs(past), DateTime::<Utc>::UNIX_EPOCH, "{past}");
        }
        for future in [1e13, 1.8e19, 1e300, f64::INFINITY] {
            assert_eq!(at_secs(future), DateTime::<Utc>::MAX_UTC, "{future}");
        }

        // A stray scrape far in the future is a scrape gap, not a panic.
        let sweeps = ramp(5, |i| i as f64);
        let mut scrapes = scrapes_of(&sweeps, T0, T0 + 100.0);
        scrapes.push(ScrapeMark {
            at_ms: i64::MAX,
            end: T0 + 80.0,
        });
        let input = input(T0, T0 + 150.0, 0, 4, Direction::Up, &[1.0]);
        let reply = compute(&input, &sweeps, &scrapes, seen());
        assert_eq!(reply.scrape_gaps_total, 1);
        assert_eq!(reply.scrape_gaps[0].to, DateTime::<Utc>::MAX_UTC);
    }
}
