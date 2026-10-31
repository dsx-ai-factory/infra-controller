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

//! When a count crossed each target, and how far off that time can be.
//!
//! Each crossing is timed at the first sweep end that meets the target. A
//! crossing happened at most one sweep gap plus one sweep duration before
//! the `E` that shows it, so the timing error bound is the largest sweep
//! gap, the longest sweep, the client's write window and any scrape gap no
//! sweep gap covers.

use std::collections::{BTreeSet, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use carbide_site_health_probe_api::metrics::{Capabilities, SeriesSpec, gauge_labels, series_spec};
use carbide_site_health_probe_api::{
    Crossing, CurvePoint, Direction, Family, Filter, MAX_COUNT, Q_DECIMALS, ScrapeGap,
    SummaryParams, SummaryResponse, UnitsPerMinute,
};
use chrono::{DateTime, Utc};

use crate::context::Context;
use crate::error::HistoryError;
use crate::samples::{
    CapSeries, RawSeries, ScrapePoint, Sweep, incomplete_sweeps, join_points, ms_secs,
    probe_restarts, sweeps,
};

/// 10^[`Q_DECIMALS`]: every accepted q is a whole number of these parts.
const Q_PARTS: u64 = 10_u64.pow(Q_DECIMALS as u32);

/// The curve keeps at most this many points.
pub const CURVE_POINTS: usize = 200;
/// Quantiles one summary may ask for: each crossing is a curve point that is
/// always kept, next to the first and the last.
pub const MAX_QUANTILES: usize = CURVE_POINTS - 2;
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
pub(crate) fn at_secs(secs: f64) -> DateTime<Utc> {
    Duration::try_from_secs_f64(secs.max(0.0))
        .ok()
        .and_then(|d| DateTime::from_timestamp(i64::try_from(d.as_secs()).ok()?, d.subsec_nanos()))
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

fn millis(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn secs_of(at: DateTime<Utc>) -> f64 {
    at.timestamp_millis() as f64 / 1000.0
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
    fn targets(&self) -> Result<Vec<(f64, u64)>, HistoryError> {
        self.qs
            .iter()
            .map(|q| match self.target(*q) {
                Some(target) => Ok((*q, target)),
                None => match (self.delta(*q), self.direction) {
                    (None, _) => Err(HistoryError::invalid(
                        "q",
                        format!("q entry {q} has more than six decimal places"),
                    )),
                    (Some(delta), Direction::Down) if delta > self.base => {
                        Err(HistoryError::invalid(
                            "n",
                            "a down target would be below 0: n is larger than base",
                        ))
                    }
                    (Some(_), Direction::Up) => Err(HistoryError::invalid(
                        "base",
                        "an up target would exceed the largest count: base + n is too large",
                    )),
                    (Some(_), Direction::Down) => Err(HistoryError::invalid(
                        "base",
                        "a down target would exceed the largest count: base is too large",
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

    /// The count change from `a` to `b` in the direction of progress.
    fn progress(&self, a: f64, b: f64) -> f64 {
        match self.direction {
            Direction::Up => b - a,
            Direction::Down => a - b,
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

/// The sweeps a summary from `start` reads out of `usable` (sorted, none
/// past the end): the last one before the window, which it counts from, and
/// the window's own. Older history is not read.
fn read_sweeps<'a>(usable: &[&'a Sweep], start: f64) -> (Option<&'a Sweep>, Vec<&'a Sweep>) {
    let window = usable.iter().copied().filter(|s| s.end >= start).collect();
    let before = usable.iter().rev().find(|s| s.end < start).copied();
    (before, window)
}

/// The largest progress from one sweep of `span` (sorted by end) to a later
/// one at most [`PEAK_WINDOW`] after it, or `None` without such a pair. The
/// best later count of each sweep comes from a sliding window, so the work
/// is linear even when many sweeps end close together.
fn peak_progress(input: &SummaryInput, span: &[&Sweep]) -> Option<f64> {
    let mut peak: Option<f64> = None;
    // Indices of the later sweeps in reach, best count first.
    let mut best: VecDeque<usize> = VecDeque::new();
    let mut next = 0;
    for (i, a) in span.iter().enumerate() {
        while best.front().is_some_and(|k| *k <= i) {
            best.pop_front();
        }
        next = next.max(i + 1);
        while next < span.len() && span[next].end - a.end <= PEAK_WINDOW {
            let count = span[next].count;
            while best
                .back()
                .is_some_and(|k| !input.better(span[*k].count, count))
            {
                best.pop_back();
            }
            best.push_back(next);
            next += 1;
        }
        if let Some(k) = best.front() {
            let change = input.progress(a.count, span[*k].count);
            peak = Some(peak.map_or(change, |p: f64| p.max(change)));
        }
    }
    peak
}

pub(crate) fn compute(
    input: &SummaryInput,
    sweeps: &[Sweep],
    scrapes: &[ScrapeMark],
    tail: TailState,
) -> SummaryResponse {
    let usable: Vec<&Sweep> = sweeps.iter().filter(|s| s.end <= input.end).collect();
    let (before, window) = read_sweeps(&usable, input.start);

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
        scrape_gaps_total: u32::try_from(gaps.len()).unwrap_or(u32::MAX),
        scrape_gaps: Vec::new(),
        uncovered_scrape_gap_seconds: 0.0,
        add_window_seconds: input.add_window_seconds,
        timing_error_seconds: None,
        final_scrape_seen: tail.final_scrape_seen,
        probe_restarts: tail.probe_restarts,
        sweeps_seen: u32::try_from(window.len()).unwrap_or(u32::MAX),
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
    response.units_per_minute.peak_10m = peak_progress(input, &span).map(|p| p.max(0.0) / 10.0);

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

/// A checked summary request: the series it reads and its window. Built by
/// [`prepare`] before any store read, and never changed after.
#[derive(Debug, Clone)]
pub struct Request {
    spec: SeriesSpec,
    start: SystemTime,
    end: SystemTime,
    input: SummaryInput,
}

impl Request {
    /// The count series, and the capabilities every sweep read must show.
    pub fn spec(&self) -> &SeriesSpec {
        &self.spec
    }

    pub fn start(&self) -> SystemTime {
        self.start
    }

    pub fn end(&self) -> SystemTime {
        self.end
    }
}

/// The samples a store client read for one [`Request`]: the count series,
/// the sweep time and counter series of the request's family, and the flag
/// series of each of its needs in order. A need without flag series has no
/// flag samples, so no sweep meets it.
#[derive(Debug, Clone, Default)]
pub struct Samples {
    pub counts: Vec<RawSeries>,
    pub times: Vec<RawSeries>,
    pub caps: Vec<Vec<RawSeries>>,
}

/// Checks a summary of `filter` over `params` before any store read: the
/// window ends by now, starts after the epoch and spans at most
/// `max_window`, and every quantile has a target that is a count.
pub fn prepare(
    ctx: &Context,
    filter: &Filter,
    params: &SummaryParams,
    max_window: Duration,
) -> Result<Request, HistoryError> {
    ctx.check()?;
    // History has no current host read: a compact host field is a need,
    // checked in every sweep read.
    let spec = series_spec(
        filter,
        &Capabilities {
            full_host_read: false,
        },
    )
    .map_err(|err| HistoryError::Unsupported(err.to_string()))?;
    let start = SystemTime::from(params.start);
    let end = SystemTime::from(params.end);
    if end > ctx.now() {
        return Err(HistoryError::invalid("end", "end is in the future"));
    }
    if start >= end {
        return Err(HistoryError::invalid("start", "start must be before end"));
    }
    if start < UNIX_EPOCH {
        return Err(HistoryError::invalid(
            "start",
            "start is before the Unix epoch",
        ));
    }
    if end.duration_since(start).unwrap_or_default() > max_window {
        return Err(HistoryError::invalid(
            "start",
            format!("the window is longer than {max_window:?}"),
        ));
    }
    let qs = params
        .quantiles()
        .map_err(|reason| HistoryError::invalid("q", reason))?;
    if qs.len() > MAX_QUANTILES {
        return Err(HistoryError::invalid(
            "q",
            format!(
                "at most {MAX_QUANTILES} quantiles: each is a curve point kept in a curve of \
                 {CURVE_POINTS}"
            ),
        ));
    }
    // A write window longer than the longest run window is no write window,
    // and a huge one would make the timing error infinite.
    if !(params.add_window_seconds.is_finite()
        && (0.0..=max_window.as_secs_f64()).contains(&params.add_window_seconds))
    {
        return Err(HistoryError::invalid(
            "addWindowSeconds",
            "addWindowSeconds must be from 0 to the longest window",
        ));
    }
    let input = SummaryInput {
        family: spec.family,
        start: secs(start),
        end: secs(end),
        base: params.base,
        n: params.n,
        direction: params.direction,
        qs,
        add_window_seconds: params.add_window_seconds,
        scrape_interval: ctx.probe.scrape_interval().as_secs_f64(),
    };
    // Every target must be a count before any store work.
    input.targets()?;
    // A request checked past the deadline, or after a cancel, is not taken.
    ctx.check()?;
    Ok(Request {
        spec,
        start,
        end,
        input,
    })
}

/// The flag series of each need: met only where the flag is exactly 1.
fn points(request: &Request, samples: &Samples) -> Result<Vec<ScrapePoint>, HistoryError> {
    let capabilities: Vec<_> = request
        .spec
        .needs
        .iter()
        .map(|need| need.capability())
        .collect();
    let names: Vec<String> = capabilities
        .iter()
        .map(|capability| capability.gauge.metric_name())
        .collect();
    let caps: Vec<CapSeries<'_>> = capabilities
        .iter()
        .zip(&names)
        .enumerate()
        .map(|(i, (capability, name))| CapSeries {
            series: samples.caps.get(i).map_or(&[], Vec::as_slice),
            name,
            label: capability.label,
            value: &capability.value,
            ok: |v| v == 1.0,
        })
        .collect();
    join_points(
        &samples.counts,
        gauge_labels(request.spec.metric),
        &samples.times,
        &caps,
    )
}

/// Whether `samples` hold a whole scrape at or after the request's end. Until
/// one lands, sweeps that ended before the end may not have been scraped
/// yet; a store client polls for it.
pub fn final_scrape_seen(
    ctx: &Context,
    request: &Request,
    samples: &Samples,
) -> Result<bool, HistoryError> {
    ctx.check()?;
    let end_ms = millis(request.end);
    let seen = points(request, samples)?.iter().any(|p| p.at_ms >= end_ms);
    // A reply made past the deadline, or after a cancel, is not taken.
    ctx.check()?;
    Ok(seen)
}

/// The summary of `request` from the `samples` read for it. Every sweep
/// the summary reads must show the capabilities its filter needs; a
/// missing or false flag in any of them is unavailable, never proof.
pub fn summarize(
    ctx: &Context,
    request: &Request,
    samples: &Samples,
) -> Result<SummaryResponse, HistoryError> {
    ctx.check()?;
    let input = &request.input;
    let points = points(request, samples)?;
    let end_ms = millis(request.end);
    let final_scrape_seen = points.iter().any(|p| p.at_ms >= end_ms);
    let found = sweeps(&points)?;
    ctx.check()?;
    let usable: Vec<&Sweep> = found.iter().filter(|s| s.end <= input.end).collect();
    let (before, window) = read_sweeps(&usable, input.start);
    if !request.spec.needs.is_empty()
        && before
            .into_iter()
            .chain(window)
            .any(|s| s.capable != Some(true))
    {
        return Err(HistoryError::Unavailable(
            "a sweep in the window could not answer this filter (REST total or compact host read)"
                .to_string(),
        ));
    }
    let tail = TailState {
        final_scrape_seen,
        probe_restarts: probe_restarts(&samples.times, input.start, input.end)?,
        incomplete_sweeps: incomplete_sweeps(&samples.times, input.start, input.end)?,
    };
    let scrapes: Vec<ScrapeMark> = points
        .iter()
        .map(|p| ScrapeMark {
            at_ms: p.at_ms,
            end: p.end,
        })
        .collect();
    let reply = compute(input, &found, &scrapes, tail);
    // A reply made past the deadline, or after a cancel, is not taken.
    ctx.check()?;
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use carbide_site_health_probe_api::{DEFAULT_Q, parse_q};

    use super::*;
    use crate::fixtures::{
        Flag, HOST_PREDICATES_KNOWN, HOSTS_UPDATE_ELIGIBLE, PROCESS_START, READY, REST_TOTALS,
        REST_TOTALS_AVAILABLE, SWEEP_END, SWEEP_START, SWEEPS_TOTAL, UNITS, context, filter,
        series, site,
    };

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

    fn is_bad_store(err: &HistoryError) -> bool {
        matches!(err, HistoryError::Unavailable(text) if text.starts_with("the stored series"))
    }

    fn field_of(err: HistoryError) -> &'static str {
        match err {
            HistoryError::InvalidInput { field, .. } => field,
            other => panic!("not invalid input: {other:?}"),
        }
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
            UNITS,
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
        assert!(is_bad_store(&err), "{err:?}");
    }

    #[test]
    fn sweep_capability_merges_every_scrape() {
        // Replicas a and b scrape one sweep, each with its own flag sample.
        let flag = |instance: &str, value: Option<f64>| -> Vec<RawSeries> {
            value
                .map(|v| {
                    series(
                        REST_TOTALS_AVAILABLE,
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
                    name: REST_TOTALS_AVAILABLE,
                    label: "resource",
                    value: "rack",
                    ok: |v| v == 1.0,
                }],
            )
            .expect("joins");
            let found = sweeps(&points).expect("one count");
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].capable, want, "a {a:?}, b {b:?}");
        }
    }

    fn counter(instance: &str, samples: &[(i64, f64, f64)]) -> Vec<RawSeries> {
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
    }

    #[test]
    fn summary_ha_counters_not_double_counted() {
        let start = 200.0;
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
        assert_eq!(incomplete_sweeps(&times, start, 300.0), Ok(3 + 2));

        // The edges: scrapes at 90 (0) and 105 (1) around a start at 100.
        // An incomplete sweep at 95, before the run, shows first at 105 and
        // is counted. One at 140, before an end at 142, shows first at 145
        // and is left out.
        let edges = counter(
            "a",
            &[(90_000, 0.0, 1.0), (105_000, 1.0, 1.0), (145_000, 2.0, 1.0)],
        );
        assert_eq!(incomplete_sweeps(&edges, 100.0, 142.0), Ok(1));
    }

    #[test]
    fn incomplete_sweeps_rejects_a_counter_that_is_no_count() {
        // Two processes whose counters each read far past any count: their
        // sum overflowed u64 before the counter was checked.
        let mut times = counter("a", &[(210_000, 1e300, 1.0)]);
        times.extend(counter("b", &[(260_000, 1e300, 250.0)]));
        let err = incomplete_sweeps(&times, 200.0, 300.0).expect_err("not a count");
        assert!(is_bad_store(&err), "{err:?}");
        for value in [-1.0, 0.5, f64::NAN, f64::INFINITY] {
            let times = counter("a", &[(210_000, value, 1.0)]);
            assert!(incomplete_sweeps(&times, 200.0, 300.0).is_err(), "{value}");
        }
        // Every process's rise adds up, and a sum past u64 is no count:
        // 2,048 processes started in the run with 2^53 each make 2^64.
        let max = MAX_COUNT as f64;
        let mut times = counter("a", &[(210_000, max, 250.0)]);
        times.extend(counter("b", &[(260_000, max, 260.0)]));
        assert_eq!(incomplete_sweeps(&times, 200.0, 300.0), Ok(2 * MAX_COUNT));
        let times: Vec<RawSeries> = (0..2_048)
            .flat_map(|i| {
                let process = 200.0 + f64::from(i + 1) * 0.01;
                counter(&format!("p{i}"), &[(260_000, max, process)])
            })
            .collect();
        let err = incomplete_sweeps(&times, 200.0, 300.0).expect_err("past any count");
        assert!(is_bad_store(&err), "{err:?}");
    }

    #[test]
    fn probe_restarts_rejects_a_process_start_that_is_no_time() {
        // Processes 1 and 250, and a stray start sample before the epoch
        // with no sweep: it sorted first, so 1 counted as a restart.
        let mut times = counter("a", &[(190_000, 0.0, 1.0), (260_000, 0.0, 250.0)]);
        assert_eq!(probe_restarts(&times, 200.0, 300.0), Ok(1));
        times.push(series(
            PROCESS_START,
            &[("instance", "c")],
            &[(195_000, -5.0)],
        ));
        let err = probe_restarts(&times, 0.0, 300.0).expect_err("not a time");
        assert!(is_bad_store(&err), "{err:?}");
        for value in [f64::NAN, 1e300] {
            let mut times = counter("a", &[(190_000, 0.0, 1.0)]);
            times.push(series(
                PROCESS_START,
                &[("instance", "c")],
                &[(195_000, value)],
            ));
            assert!(probe_restarts(&times, 0.0, 300.0).is_err(), "{value}");
        }
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
    fn summary_rejects_more_quantiles_than_the_curve_keeps() {
        let mut site = site();
        let ctx = site.context();
        let mut params = site.params(-60.0, 0.0, 0, 10_000, Direction::Up);
        let qs: Vec<String> = (1..=MAX_QUANTILES + 1)
            .map(|i| format!("{}", i as f64 / 1_000.0))
            .collect();
        params.q = Some(qs.join(","));
        let err = prepare(&ctx, &filter(READY), &params, Duration::from_secs(86_400))
            .expect_err("more crossings than curve points");
        assert_eq!(field_of(err), "q");
        params.q = Some(qs[..MAX_QUANTILES].join(","));
        site.scrape(0.0, 1.0, -5.0, &[]);
        let request = prepare(&ctx, &filter(READY), &params, Duration::from_secs(86_400))
            .expect("the largest admitted request");
        let reply = summarize(&ctx, &request, &site.store.samples(UNITS, &[]))
            .expect("the largest admitted request");
        assert_eq!(reply.crossings.len(), MAX_QUANTILES);
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

    /// `peak_progress` by every pair of sweeps, as the probe computed it.
    fn peak_by_pairs(input: &SummaryInput, span: &[&Sweep]) -> Option<f64> {
        let mut peak: Option<f64> = None;
        for (i, a) in span.iter().enumerate() {
            for b in &span[i + 1..] {
                if b.end - a.end > PEAK_WINDOW {
                    break;
                }
                let change = input.progress(a.count, b.count);
                peak = Some(peak.map_or(change, |p: f64| p.max(change)));
            }
        }
        peak
    }

    #[test]
    fn peak_progress_matches_every_pair() {
        // Pseudo-random ends and counts, with ties in both, in both
        // directions and at every density.
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = |modulo: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % modulo
        };
        for round in 0..400 {
            let len = (next(60) + 1) as usize;
            let step = [1, 30, 200, 700][round % 4];
            let mut end = T0;
            let sweeps: Vec<Sweep> = (0..len)
                .map(|_| {
                    end += next(step) as f64;
                    sweep(end, next(20) as f64)
                })
                .collect();
            let span: Vec<&Sweep> = sweeps.iter().collect();
            for direction in [Direction::Up, Direction::Down] {
                let input = input(T0, end, 10, 10, direction, &[1.0]);
                assert_eq!(
                    peak_progress(&input, &span),
                    peak_by_pairs(&input, &span),
                    "round {round} {direction:?}"
                );
            }
        }
        let input = input(T0, T0 + 1.0, 0, 1, Direction::Up, &[1.0]);
        assert_eq!(peak_progress(&input, &[]), None);
        let one = sweep(T0, 1.0);
        assert_eq!(peak_progress(&input, &[&one]), None, "no pair");
        let far = sweep(T0 + PEAK_WINDOW + 1.0, 5.0);
        assert_eq!(peak_progress(&input, &[&one, &far]), None, "too far apart");
    }

    #[test]
    fn peak_progress_is_linear_in_close_sweeps() {
        // 300,000 sweeps in ten minutes, as many processes or a store that
        // mixes probes could show: every pair was 45 billion steps.
        let sweeps: Vec<Sweep> = (0..300_000)
            .map(|i| sweep(T0 + f64::from(i) * 0.002, f64::from(i % 1_000)))
            .collect();
        let span: Vec<&Sweep> = sweeps.iter().collect();
        let input = input(T0, T0 + 600.0, 0, 1_000, Direction::Up, &[1.0]);
        let started = std::time::Instant::now();
        assert_eq!(peak_progress(&input, &span), Some(999.0));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
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
    fn a_scrape_gap_before_the_window_is_not_timing_error() {
        // No sweep precedes the window, so the count at its start is
        // `base`, the caller-provided starting count, not a measured
        // sample. A scrape gap reaching back past the start hides no sweep
        // the run reads. Only its 20 s inside the window count, and the
        // sweep gaps already hold them.
        let sweeps: Vec<Sweep> = [10.0, 30.0, 50.0, 70.0, 90.0]
            .iter()
            .enumerate()
            .map(|(i, at)| sweep(T0 + at, 2.0 * (i + 1) as f64))
            .collect();
        let mut scrapes = vec![ScrapeMark {
            at_ms: ((T0 - 30.0) * 1000.0) as i64,
            end: T0 - 35.0,
        }];
        scrapes.extend(scrapes_of(&sweeps, T0 + 20.0, T0 + 100.0));
        let input = input(T0, T0 + 100.0, 0, 10, Direction::Up, &[0.5, 1.0]);
        let reply = compute(&input, &sweeps, &scrapes, seen());
        assert_eq!(reply.scrape_gaps_total, 1, "T0 - 30 to T0 + 20");
        assert_eq!(reply.uncovered_scrape_gap_seconds, 0.0);
        // The longest sweep gap (20 s) plus the longest sweep (10 s).
        assert_eq!(reply.timing_error_seconds, Some(30.0));
        assert_eq!(reply.crossings[0].reached_at, Some(at_secs(T0 + 50.0)));
        assert_eq!(reply.crossings[0].seconds, Some(50.0));
        assert_eq!(reply.crossings[1].reached_at, Some(at_secs(T0 + 90.0)));
        assert_eq!(reply.crossings[1].seconds, Some(90.0));
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
                .map(|(i, v)| series(UNITS, &[("state", &i.to_string())], &[(1_000, *v)]))
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
            &[f64::NAN],
            &[f64::INFINITY],
        ] {
            let err = join(values).expect_err("not a count");
            assert!(is_bad_store(&err), "{values:?}: {err:?}");
            assert!(!err.to_string().contains("1e308"), "{err}");
        }

        // Points built some other way are checked again.
        let mut points = join(&[1.0]).expect("joins");
        points[0].count = 1e308;
        let err = sweeps(&points).expect_err("not a count");
        assert!(is_bad_store(&err), "{err:?}");
    }

    #[test]
    fn out_of_range_sweep_time_is_store_error() {
        let counts = [series(UNITS, &[], &[(1_000, 1.0)])];
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
            (f64::NAN, 0.5, 0.1),
        ] {
            let points =
                join_points(&counts, &[], &times(end, start, process), &[]).expect("joins");
            let err = sweeps(&points).expect_err("not a time");
            assert!(is_bad_store(&err), "{end} {start} {process}: {err:?}");
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
        assert_eq!(field_of(err), "q");

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
        assert_eq!(field_of(err), "base");
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

    const DAY: Duration = Duration::from_secs(86_400);

    #[test]
    fn summary_missing_capability_flag_is_unavailable() {
        // REST rack totals whose availability flag never reached the store.
        let mut site = site();
        site.rack_scrape(-20.0, -25.0, "a", None);
        site.rack_scrape(0.0, -5.0, "a", None);
        let ctx = site.context();
        let request = prepare(
            &ctx,
            &filter("type=rest_total&resource=rack"),
            &site.params(-60.0, 0.0, 0, 1, Direction::Up),
            DAY,
        )
        .expect("valid");
        let samples = site.store.samples(REST_TOTALS, &[REST_TOTALS_AVAILABLE]);
        let err = summarize(&ctx, &request, &samples).expect_err("no flag sample");
        assert!(matches!(err, HistoryError::Unavailable(_)), "{err:?}");
        // A store client that read no flag series at all is no proof
        // either.
        let mut none = samples;
        none.caps.clear();
        let err = summarize(&ctx, &request, &none).expect_err("no flag series");
        assert!(matches!(err, HistoryError::Unavailable(_)), "{err:?}");
    }

    /// The summary of REST rack totals whose availability flag is `rack`,
    /// next to a tray flag of `tray` on the same gauge, as the probe
    /// publishes them.
    fn rack_summary(rack: Option<f64>, tray: Option<f64>) -> Result<SummaryResponse, HistoryError> {
        let mut site = site();
        site.rack_scrape(-20.0, -25.0, "a", rack);
        site.rack_scrape(0.0, -5.0, "a", rack);
        if let Some(tray) = tray {
            for at in [site.ms(-20.0), site.ms(0.0)] {
                let labels = [
                    ("resource", "tray"),
                    ("job", "j"),
                    ("namespace", "ns"),
                    ("instance", "a"),
                ];
                site.store.add(REST_TOTALS_AVAILABLE, &labels, at, tray);
            }
        }
        let ctx = site.context();
        let request = prepare(
            &ctx,
            &filter("type=rest_total&resource=rack"),
            &site.params(-60.0, 0.0, 0, 1, Direction::Up),
            DAY,
        )
        .expect("valid");
        summarize(
            &ctx,
            &request,
            &site.store.samples(REST_TOTALS, &[REST_TOTALS_AVAILABLE]),
        )
    }

    #[test]
    fn a_tray_flag_of_0_does_not_unmake_the_rack() {
        let reply = rack_summary(Some(1.0), Some(0.0)).expect("rack is available");
        assert!(reply.crossings[0].reached_at.is_some());
    }

    #[test]
    fn a_tray_flag_of_1_is_no_proof_for_the_rack() {
        let err = rack_summary(None, Some(1.0)).expect_err("only the tray is known");
        assert!(matches!(err, HistoryError::Unavailable(_)), "{err:?}");
    }

    #[test]
    fn a_flag_of_another_gauge_is_no_proof_for_the_rack() {
        // The count gauge carries resource="rack" at 1 too. Read as the
        // flag series, it must not make the rack available.
        let mut site = site();
        site.rack_scrape(-20.0, -25.0, "a", None);
        site.rack_scrape(0.0, -5.0, "a", None);
        let ctx = site.context();
        let request = prepare(
            &ctx,
            &filter("type=rest_total&resource=rack"),
            &site.params(-60.0, 0.0, 0, 1, Direction::Up),
            DAY,
        )
        .expect("valid");
        let samples = site.store.samples(REST_TOTALS, &[REST_TOTALS]);
        let err = summarize(&ctx, &request, &samples).expect_err("not the flag gauge");
        assert!(matches!(err, HistoryError::Unavailable(_)), "{err:?}");
    }

    #[test]
    fn summary_keeps_any_false_capability_of_one_sweep() {
        // Replicas a and b scrape the sweep that ended at -25 with the same
        // count; the sweep that ended at -5 is fully available.
        for (a, b, answers) in [
            (Some(1.0), Some(1.0), true),
            (Some(1.0), Some(0.0), false),
            (Some(0.0), Some(1.0), false),
            (Some(1.0), None, false),
        ] {
            let mut site = site();
            site.rack_scrape(-20.0, -25.0, "a", a);
            site.rack_scrape(-18.0, -25.0, "b", b);
            site.rack_scrape(0.0, -5.0, "a", Some(1.0));
            let ctx = site.context();
            let request = prepare(
                &ctx,
                &filter("type=rest_total&resource=rack"),
                &site.params(-60.0, 0.0, 0, 1, Direction::Up),
                DAY,
            )
            .expect("valid");
            let result = summarize(
                &ctx,
                &request,
                &site.store.samples(REST_TOTALS, &[REST_TOTALS_AVAILABLE]),
            );
            match result {
                Ok(_) => assert!(answers, "a {a:?}, b {b:?} answered"),
                Err(err) => {
                    assert!(!answers, "a {a:?}, b {b:?}: {err:?}");
                    assert!(matches!(err, HistoryError::Unavailable(_)), "{err:?}");
                }
            }
        }
    }

    #[test]
    fn capability_counts_only_the_sweeps_read() {
        // A sweep without the total long before the window, then the
        // baseline the summary counts from, then the window's sweeps. A
        // sweep without the total publishes no total and a 0 flag.
        for (baseline, readable) in [(Some(0.0), true), (None, false)] {
            let mut site = site();
            for (end, rack) in [
                (-320.0, None),
                (-200.0, baseline),
                (-100.0, Some(1.0)),
                (-20.0, Some(3.0)),
            ] {
                site.rest_totals_scrape(
                    end + 5.0,
                    end,
                    "replica-a",
                    rack,
                    Some(rack.map_or(0.0, |_| 1.0)),
                );
            }
            site.rest_totals_scrape(0.0, -20.0, "replica-a", Some(3.0), Some(1.0));
            let ctx = site.context();
            let request = prepare(
                &ctx,
                &filter("type=rest_total&resource=rack"),
                &site.params(-120.0, 0.0, 0, 3, Direction::Up),
                DAY,
            )
            .expect("valid");
            let result = summarize(
                &ctx,
                &request,
                &site.store.samples(REST_TOTALS, &[REST_TOTALS_AVAILABLE]),
            );
            if readable {
                let reply = result.expect("older history is not read");
                assert_eq!(
                    reply.crossings[0].reached_at,
                    Some(DateTime::<Utc>::from(site.at(-20.0)))
                );
            } else {
                let err = result.expect_err("the baseline has no total");
                assert!(matches!(err, HistoryError::Unavailable(_)), "{err:?}");
            }
        }
    }

    /// One scrape of a host sweep that ended at `end` with `eligible`
    /// update-eligible hosts, read in `mode`: the full read publishes no
    /// predicate flag, the compact read publishes it as known (1) or lost
    /// (0).
    fn eligible_scrape(site: &mut crate::fixtures::Site, at: f64, end: f64, flag: Option<f64>) {
        let eligible: Flag<'_> = (
            HOSTS_UPDATE_ELIGIBLE,
            &[("eligible", "true"), ("model", "m")],
            3.0,
        );
        let known: Vec<Flag<'_>> = flag
            .map(|v| {
                (
                    HOST_PREDICATES_KNOWN,
                    &[("predicate", "updateEligible")][..],
                    v,
                )
            })
            .into_iter()
            .collect();
        site.scrape(at, 0.0, end, &[&[eligible][..], &known[..]].concat());
    }

    #[test]
    fn compact_host_predicates_fold_over_the_window() {
        // Full read (no flag), compact read (known in every sweep), and a
        // compact read that lost the answer in one sweep.
        for (flags, answers) in [
            ([None, None, None], false),
            ([Some(1.0), Some(1.0), Some(1.0)], true),
            ([Some(1.0), Some(0.0), Some(1.0)], false),
            ([Some(1.0), Some(1.0), None], false),
        ] {
            let mut site = site();
            for (i, flag) in flags.into_iter().enumerate() {
                let end = -60.0 + 20.0 * i as f64;
                eligible_scrape(&mut site, end + 2.0, end, flag);
            }
            let ctx = site.context();
            let request = prepare(
                &ctx,
                &filter("type=host&updateEligible=true"),
                &site.params(-50.0, -10.0, 0, 3, Direction::Up),
                DAY,
            )
            .expect("a need, not an error, without a current host read");
            let samples = site
                .store
                .samples(HOSTS_UPDATE_ELIGIBLE, &[HOST_PREDICATES_KNOWN]);
            match summarize(&ctx, &request, &samples) {
                Ok(reply) => {
                    assert!(answers, "{flags:?} answered");
                    assert_eq!(reply.final_count, Some(3));
                }
                Err(err) => {
                    assert!(!answers, "{flags:?}: {err:?}");
                    assert!(matches!(err, HistoryError::Unavailable(_)), "{err:?}");
                }
            }
        }
    }

    #[test]
    fn summary_stalled_reader_tail_counts() {
        let mut site = site();
        // The reader stalled: every scrape shows the sweep that ended at
        // -100.
        for i in 0..=20 {
            site.scrape(-100.0 + 5.0 * i as f64, 4.0, -100.0, &[]);
        }
        let ctx = site.context();
        let request = prepare(
            &ctx,
            &filter(READY),
            &site.params(-90.0, 0.0, 0, 4, Direction::Up),
            DAY,
        )
        .expect("valid");
        let reply = summarize(&ctx, &request, &site.store.samples(UNITS, &[])).expect("summary");
        assert_eq!(
            reply.max_sweep_gap_seconds,
            Some(100.0),
            "fresh scrapes of an old sweep close no gap"
        );
        assert_eq!(reply.sweeps_seen, 0);
        assert_eq!(reply.final_count, Some(4));
        assert!(reply.final_scrape_seen, "the scrape at the end");
    }

    #[test]
    fn summary_rejects_end_in_future_and_negative_target() {
        let site = site();
        let ctx = site.context();
        let check = |params: &SummaryParams| prepare(&ctx, &filter(READY), params, DAY);
        let err = check(&site.params(-60.0, 10.0, 0, 1, Direction::Up)).expect_err("future");
        assert_eq!(field_of(err), "end");
        let err = check(&site.params(-60.0, 0.0, 10, 100, Direction::Down))
            .expect_err("a target below 0");
        assert_eq!(field_of(err), "n");
        let mut params = site.params(-60.0, 0.0, 0, 1, Direction::Up);
        params.q = Some("0,1".to_string());
        let err = check(&params).expect_err("q 0");
        assert_eq!(field_of(err), "q");
    }

    #[test]
    fn unrepresentable_targets_are_invalid_input() {
        let site = site();
        let ctx = site.context();
        for (base, n, direction, field) in [
            (u64::MAX, 1, Direction::Up, "base"),
            (1, u64::MAX, Direction::Up, "base"),
            (10, 100, Direction::Down, "n"),
        ] {
            let err = prepare(
                &ctx,
                &filter(READY),
                &site.params(-600.0, 0.0, base, n, direction),
                DAY,
            )
            .expect_err("no count is the target");
            assert_eq!(field_of(err), field, "{base} {n} {direction:?}");
        }
    }

    #[test]
    fn prepare_checks_the_window() {
        let site = site();
        let ctx = site.context();
        let check = |params: &SummaryParams, max: Duration| {
            prepare(&ctx, &filter(READY), params, max).map(|r| (r.start, r.end))
        };
        let params = site.params(-600.0, 0.0, 0, 1, Direction::Up);
        assert_eq!(
            check(&params, Duration::from_secs(600)),
            Ok((site.at(-600.0), site.at(0.0))),
            "a window of exactly the longest"
        );
        let err = check(&params, Duration::from_secs(599)).expect_err("too long");
        assert_eq!(field_of(err), "start");
        let err = check(&site.params(0.0, 0.0, 0, 1, Direction::Up), DAY).expect_err("empty");
        assert_eq!(field_of(err), "start");

        // A start before the epoch is no time a sample can have; it read as
        // the epoch itself.
        let mut early = params.clone();
        early.start = DateTime::<Utc>::UNIX_EPOCH - chrono::Duration::seconds(10);
        early.end = DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::seconds(10);
        let err = check(&early, DAY).expect_err("before the epoch");
        assert_eq!(field_of(err), "start");

        for add in [-1.0, f64::NAN, f64::INFINITY, 1e308, 86_401.0] {
            let mut params = params.clone();
            params.add_window_seconds = add;
            let err = check(&params, DAY).expect_err("no write window");
            assert_eq!(field_of(err), "addWindowSeconds", "{add}");
        }
        let mut params = params;
        params.add_window_seconds = 86_400.0;
        assert!(check(&params, DAY).is_ok(), "the longest write window");
    }

    #[test]
    fn prepare_rejects_filters_history_cannot_count() {
        let site = site();
        let ctx = site.context();
        let params = site.params(-600.0, 0.0, 0, 1, Direction::Up);
        for query in [
            "type=host&done=true",
            "type=rest_total",
            "type=host&bmcVersion=v1",
            "type=instance&source=rest",
        ] {
            let err = prepare(&ctx, &filter(query), &params, DAY).expect_err(query);
            assert!(
                matches!(err, HistoryError::Unsupported(_)),
                "{query}: {err:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn every_call_stops_at_cancel_and_deadline() {
        let mut site = site();
        site.scrape(-20.0, 1.0, -25.0, &[]);
        site.scrape(0.0, 1.0, -5.0, &[]);
        let params = site.params(-60.0, 0.0, 0, 1, Direction::Up);
        let mut ctx = site.context();
        ctx.deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let request = prepare(&ctx, &filter(READY), &params, DAY).expect("in time");
        let samples = site.store.samples(UNITS, &[]);
        assert!(summarize(&ctx, &request, &samples).is_ok());
        assert_eq!(final_scrape_seen(&ctx, &request, &samples), Ok(true));

        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            prepare(&ctx, &filter(READY), &params, DAY).map(|_| ()),
            Err(HistoryError::Timeout)
        );
        assert_eq!(
            summarize(&ctx, &request, &samples),
            Err(HistoryError::Timeout)
        );
        assert_eq!(
            final_scrape_seen(&ctx, &request, &samples),
            Err(HistoryError::Timeout)
        );
        ctx.cancel.cancel();
        assert_eq!(
            summarize(&ctx, &request, &samples),
            Err(HistoryError::Cancelled),
            "cancel first"
        );
        assert_eq!(
            final_scrape_seen(&ctx, &request, &samples),
            Err(HistoryError::Cancelled)
        );
    }

    #[test]
    fn prepare_stops_at_a_cancel_while_it_checks() {
        // The clock is read after the first check; a cancel then still
        // stops the request.
        let site = site();
        let mut ctx = site.context();
        let (cancel, now) = (ctx.cancel.clone(), site.base);
        ctx.clock = std::sync::Arc::new(move || {
            cancel.cancel();
            now
        });
        let err = prepare(
            &ctx,
            &filter(READY),
            &site.params(-60.0, 0.0, 0, 1, Direction::Up),
            DAY,
        )
        .expect_err("cancelled");
        assert_eq!(err, HistoryError::Cancelled);
    }

    #[test]
    fn final_scrape_is_one_at_or_after_the_end() {
        let mut site = site();
        site.scrape(-25.3, 1.0, -45.0, &[]);
        site.scrape(-20.3, 1.0, -25.0, &[]);
        let ctx = site.context();
        let request = prepare(
            &ctx,
            &filter(READY),
            &site.params(-60.0, -0.3, 0, 2, Direction::Up),
            DAY,
        )
        .expect("valid");
        let samples = site.store.samples(UNITS, &[]);
        assert_eq!(final_scrape_seen(&ctx, &request, &samples), Ok(false));
        let reply = summarize(&ctx, &request, &samples).expect("summary");
        assert!(!reply.final_scrape_seen);
        assert_eq!(reply.scrape_gaps_total, 1, "the missing tail is a gap");
        assert!(!reply.scrape_gaps[0].new_sweep_across);
        assert_eq!(reply.final_count, Some(1));

        // The scrape exactly at the end counts; its count is the terminal
        // one.
        site.scrape(-0.3, 2.0, -5.3, &[]);
        let samples = site.store.samples(UNITS, &[]);
        assert_eq!(final_scrape_seen(&ctx, &request, &samples), Ok(true));
        let reply = summarize(&ctx, &request, &samples).expect("summary");
        assert!(reply.final_scrape_seen);
        assert_eq!(reply.final_count, Some(2));
        assert_eq!(reply.scrape_gaps_total, 1, "the 20 s between scrapes");
        assert!(
            reply.scrape_gaps[0].new_sweep_across,
            "a gap between two scrapes, no missing tail"
        );
        assert_eq!(
            reply.crossings[0].reached_at,
            Some(DateTime::<Utc>::from(site.at(-5.3)))
        );

        // A count whose sweep times have not landed is not yet a scrape.
        let mut late = self::site();
        late.scrape(-40.0, 1.0, -45.0, &[]);
        let labels: Vec<(&str, &str)> = [("type", "host"), ("stage", "ready"), ("state", "Ready")]
            .into_iter()
            .chain(crate::fixtures::SELECTOR)
            .collect();
        late.store.add(UNITS, &labels, late.ms(1.0), 2.0);
        let samples = late.store.samples(UNITS, &[]);
        assert_eq!(final_scrape_seen(&ctx, &request, &samples), Ok(false));
    }

    #[test]
    fn errors_never_carry_sample_values() {
        // Sentinel values in every place a sample goes.
        let sentinel = 123_456_789.25;
        let mut site = site();
        site.scrape(-20.0, sentinel, -25.0, &[]);
        let ctx = site.context();
        let request = prepare(
            &ctx,
            &filter(READY),
            &site.params(-60.0, 0.0, 0, 1, Direction::Up),
            DAY,
        )
        .expect("valid");
        let err =
            summarize(&ctx, &request, &site.store.samples(UNITS, &[])).expect_err("not a count");
        let text = err.to_string();
        assert!(!text.contains("123456789"), "{text}");

        let mut labels: BTreeMap<String, String> = BTreeMap::new();
        labels.insert("__name__".to_string(), PROCESS_START.to_string());
        labels.insert("instance".to_string(), "http://u:token@p".to_string());
        let samples = Samples {
            times: vec![RawSeries {
                labels,
                samples: vec![(1, -sentinel)],
            }],
            ..Samples::default()
        };
        let err = summarize(&ctx, &request, &samples).expect_err("not a time");
        let text = err.to_string();
        assert!(
            !text.contains("123456789") && !text.contains("token"),
            "{text}"
        );
    }

    #[test]
    fn sources_whose_labels_read_alike_stay_apart() {
        // One source has x="a,y=b", the other x="a" and y="b". Joined into
        // one text they read alike, and the first one's count would join
        // the second one's sweep.
        let site = site();
        let ms = site.ms(-20.0);
        let (end, start, process) = (site.secs(-25.0), site.secs(-35.0), site.secs(-100_000.0));
        let first = [("job", "j"), ("namespace", "ns"), ("x", "a,y=b")];
        let second = [("job", "j"), ("namespace", "ns"), ("x", "a"), ("y", "b")];
        // The count is always the first source's. The sweep is the one of
        // `times_from`.
        let samples = |times_from: &[(&str, &str)]| {
            let labels: Vec<(&str, &str)> = [("reader", "devices"), ("family", "host")]
                .into_iter()
                .chain(times_from.iter().copied())
                .collect();
            Samples {
                counts: vec![series(
                    UNITS,
                    &[
                        ("type", "host"),
                        ("stage", "ready"),
                        ("state", "Ready"),
                        ("job", "j"),
                        ("namespace", "ns"),
                        ("x", "a,y=b"),
                    ],
                    &[(ms, 1.0)],
                )],
                times: vec![
                    series(SWEEP_END, &labels, &[(ms, end)]),
                    series(SWEEP_START, &labels, &[(ms, start)]),
                    series(PROCESS_START, times_from, &[(ms, process)]),
                ],
                ..Samples::default()
            }
        };
        let ctx = site.context();
        let request = prepare(
            &ctx,
            &filter(READY),
            &site.params(-60.0, 0.0, 0, 1, Direction::Up),
            DAY,
        )
        .expect("valid");

        let same = summarize(&ctx, &request, &samples(&first)).expect("summary");
        assert!(
            same.crossings[0].reached_at.is_some(),
            "control: the count and the sweep of one source join"
        );
        let apart = summarize(&ctx, &request, &samples(&second)).expect("summary");
        assert_eq!(
            apart.crossings[0].reached_at, None,
            "a count of one source never crosses on the sweep of another"
        );
    }

    #[test]
    fn conflicting_time_samples_of_one_source_are_rejected_in_any_order() {
        let source = [("job", "j"), ("namespace", "ns")];
        let times = |name: &str, values: &[f64]| -> Vec<RawSeries> {
            values
                .iter()
                .map(|value| series(name, &source, &[(1_000, *value)]))
                .collect()
        };
        let sweep_times = |ends: &[f64]| {
            let mut all = times(SWEEP_END, ends);
            all.extend(times(SWEEP_START, &[90.0]));
            all.extend(times(PROCESS_START, &[1.0]));
            all
        };
        for ends in [[100.0, 101.0], [101.0, 100.0]] {
            let err = join_points(&[], &[], &sweep_times(&ends), &[]).expect_err("conflict");
            assert!(matches!(err, HistoryError::Unavailable(_)), "{err:?}");
        }
        let same = join_points(&[], &[], &sweep_times(&[100.0, 100.0]), &[]).expect("equal");
        assert_eq!(same.len(), 1, "equal duplicates are one sample");

        // The process starts the incomplete count reads.
        let incomplete = |processes: &[f64]| {
            let mut all = times(PROCESS_START, processes);
            all.push(series(
                SWEEPS_TOTAL,
                &[
                    ("reader", "devices"),
                    ("family", "host"),
                    ("result", "incomplete"),
                    source[0],
                    source[1],
                ],
                &[(1_000, 1.0)],
            ));
            all
        };
        for processes in [[1.0, 2.0], [2.0, 1.0]] {
            let err = incomplete_sweeps(&incomplete(&processes), 0.0, 10.0).expect_err("conflict");
            assert!(matches!(err, HistoryError::Unavailable(_)), "{err:?}");
        }
        assert!(incomplete_sweeps(&incomplete(&[1.0, 1.0]), 0.0, 10.0).is_ok());
    }

    #[test]
    fn context_at_now_is_the_injected_clock() {
        let now = UNIX_EPOCH + Duration::from_secs(42);
        assert_eq!(context(now).now(), now);
    }
}
