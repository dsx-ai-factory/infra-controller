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

//! Qualification on recorded samples: series as a probe publishes them and
//! a store returns them, with exact expected crossings and error bounds.

use std::time::{Duration, UNIX_EPOCH};

use carbide_site_health_probe_api::{DEFAULT_Q, Direction, SummaryParams, SummaryResponse};
use chrono::{DateTime, Utc};

use crate::fixtures::{
    PROCESS_START, READY, Recorder, SELECTOR, SWEEP_END, SWEEP_START, SWEEPS_TOTAL, UNITS, context,
    filter,
};
use crate::{prepare, summarize};

const DAY: Duration = Duration::from_secs(86_400);

/// One sweep a probe published: its process start, start, end and count,
/// in Unix seconds.
#[derive(Debug, Clone, Copy)]
struct Published {
    process: f64,
    start: f64,
    end: f64,
    count: u64,
}

fn ms(secs: f64) -> i64 {
    (secs * 1000.0).round() as i64
}

fn utc(secs: f64) -> DateTime<Utc> {
    DateTime::<Utc>::from(UNIX_EPOCH + Duration::from_secs_f64(secs))
}

/// Records one scrape at `at` by the monitoring replica `instance`: the
/// newest sweep published by then, and the incomplete sweeps counted by
/// then.
fn record(store: &mut Recorder, sweeps: &[Published], incomplete: &[f64], at: f64, instance: &str) {
    let Some(sweep) = sweeps.iter().rev().find(|s| s.end <= at) else {
        return;
    };
    let source: Vec<(&str, &str)> = SELECTOR
        .into_iter()
        .chain([("instance", instance)])
        .collect();
    let with = |extra: &[(&'static str, &'static str)]| -> Vec<(&str, &str)> {
        extra
            .iter()
            .copied()
            .chain(source.iter().copied())
            .collect()
    };
    let at_ms = ms(at);
    store.add(
        UNITS,
        &with(&[("type", "host"), ("stage", "ready"), ("state", "Ready")]),
        at_ms,
        sweep.count as f64,
    );
    let times = with(&[("reader", "devices"), ("family", "host")]);
    store.add(SWEEP_START, &times, at_ms, sweep.start);
    store.add(SWEEP_END, &times, at_ms, sweep.end);
    store.add(PROCESS_START, &with(&[]), at_ms, sweep.process);
    store.add(
        SWEEPS_TOTAL,
        &with(&[
            ("reader", "devices"),
            ("family", "host"),
            ("result", "incomplete"),
        ]),
        at_ms,
        incomplete.iter().filter(|t| **t <= at).count() as f64,
    );
}

fn params(start: f64, end: f64, n: u64, q: &str, add_window_seconds: f64) -> SummaryParams {
    SummaryParams {
        start: utc(start),
        end: utc(end),
        base: 0,
        n,
        direction: Direction::Up,
        q: Some(q.to_string()),
        add_window_seconds,
    }
}

fn run(store: &Recorder, now: f64, params: &SummaryParams) -> SummaryResponse {
    let ctx = context(UNIX_EPOCH + Duration::from_secs_f64(now));
    let request = prepare(&ctx, &filter(READY), params, DAY).expect("valid");
    summarize(&ctx, &request, &store.samples(UNITS, &[])).expect("summary")
}

/// A fleet that turns ready one host every 30 s, read by a reader that
/// sweeps every 20 s and takes 4 s, scraped every 5 s by two replicas 2 s
/// apart. Host `i` is ready at `t0 + 30 i`; a sweep counts the hosts ready
/// at its start.
struct Fleet {
    t0: f64,
    hosts: u64,
    sweeps: Vec<Published>,
    incomplete: Vec<f64>,
}

impl Fleet {
    /// Sweeps `k` in `0..31` start at `t0 - 10 + 20 k`. A sweep in `failed`
    /// publishes nothing and counts as incomplete at its end. From
    /// `restart`, a new process runs the sweeps.
    fn new(failed: &[u64], restart: Option<f64>) -> Self {
        let t0 = 1_700_000_000.0;
        let hosts = 18;
        let mut sweeps = Vec::new();
        let mut incomplete = Vec::new();
        for k in 0..31_u64 {
            let start = t0 - 10.0 + 20.0 * k as f64;
            let end = start + 4.0;
            if failed.contains(&k) {
                incomplete.push(end);
                continue;
            }
            let ready = if start < t0 {
                0
            } else {
                ((start - t0) / 30.0).floor() as u64 + 1
            };
            let process = match restart {
                Some(at) if start >= at => at,
                _ => t0 - 10_000.0,
            };
            sweeps.push(Published {
                process,
                start,
                end,
                count: ready.min(hosts),
            });
        }
        Self {
            t0,
            hosts,
            sweeps,
            incomplete,
        }
    }

    fn store(&self) -> Recorder {
        let mut store = Recorder::default();
        let mut at = self.t0 - 7.0;
        while at <= self.t0 + 620.0 {
            for (instance, offset) in [("replica-a", 0.0), ("replica-b", 2.0)] {
                record(
                    &mut store,
                    &self.sweeps,
                    &self.incomplete,
                    at + offset,
                    instance,
                );
            }
            at += 5.0;
        }
        store
    }

    /// When the host that makes `target` ready turned ready.
    fn true_time(&self, target: u64) -> f64 {
        self.t0 + 30.0 * (target - 1) as f64
    }
}

/// The crossing and its timing error bound: every crossing is reported at
/// or after the true time, and at most the timing error after it.
#[test]
fn producer_crossings_are_exact_and_within_the_error_bound() {
    for (failed, restart, max_gap, restarts, incomplete) in [
        (&[][..], None, 20.0, 0, 0),
        (&[20][..], None, 40.0, 0, 1),
        (&[][..], Some(1_700_000_305.0), 20.0, 1, 0),
    ] {
        let fleet = Fleet::new(failed, restart);
        let (start, end) = (fleet.t0 - 1.0, fleet.t0 + 600.0);
        let reply = run(
            &fleet.store(),
            fleet.t0 + 700.0,
            &params(start, end, fleet.hosts, DEFAULT_Q, 0.0),
        );
        let case = format!("failed {failed:?}, restart {restart:?}");

        // Targets 9, 17 and 18 (four quantiles round up to all 18 hosts).
        // A sweep counts the hosts ready at its start, so each is reached
        // at the end of the first sweep that starts at or after the true
        // time: 250 + 4, 490 + 4 and 510 + 4.
        let reached: Vec<(u64, Option<DateTime<Utc>>, Option<f64>)> = reply
            .crossings
            .iter()
            .map(|c| (c.target, c.reached_at, c.seconds))
            .collect();
        let at = |offset: f64| (Some(utc(fleet.t0 + offset)), Some(offset + 1.0));
        let want: Vec<_> = [
            (9, at(254.0)),
            (17, at(494.0)),
            (18, at(514.0)),
            (18, at(514.0)),
            (18, at(514.0)),
            (18, at(514.0)),
        ]
        .into_iter()
        .map(|(target, (at, seconds))| (target, at, seconds))
        .collect();
        assert_eq!(reached, want, "{case}");

        // The bound is the largest sweep gap and the longest sweep.
        let error = reply.timing_error_seconds.expect("bounded");
        assert_eq!(reply.max_sweep_gap_seconds, Some(max_gap), "{case}");
        assert_eq!(reply.max_sweep_duration_seconds, Some(4.0), "{case}");
        assert_eq!(reply.uncovered_scrape_gap_seconds, 0.0, "{case}");
        assert_eq!(error, max_gap + 4.0, "{case}");
        for crossing in &reply.crossings {
            let late = crossing.reached_at.expect("reached").timestamp_millis() as f64 / 1000.0
                - fleet.true_time(crossing.target);
            assert!(
                (0.0..=error).contains(&late),
                "{case}: target {} reached {late} s late, bound {error} s",
                crossing.target
            );
        }

        assert!(reply.final_scrape_seen, "{case}");
        assert_eq!(reply.final_count, Some(18), "{case}");
        assert_eq!(reply.final_sweep_at, Some(utc(fleet.t0 + 594.0)), "{case}");
        assert_eq!(reply.probe_restarts, restarts, "{case}");
        assert_eq!(reply.incomplete_sweeps, incomplete, "{case}");
        assert_eq!(reply.scrape_gaps_total, 0, "{case}");
        assert_eq!(reply.sweeps_seen, 30 - failed.len() as u32, "{case}");
        // A new host every 30 s and a sweep every 20 s: 40 s at most
        // between two new counts.
        assert_eq!(reply.max_zero_progress_seconds, Some(40.0), "{case}");
        // t(0.1) is target 2 at +34 and t(0.9) is target 17 at +494.
        assert_eq!(
            reply.units_per_minute.sustained,
            Some(0.8 * 18.0 / (460.0 / 60.0)),
            "{case}"
        );
        // From 0 at -6 to 18 at +594, exactly 600 s apart.
        assert_eq!(reply.units_per_minute.peak_10m, Some(1.8), "{case}");
        assert_eq!(reply.worst_count_after_first_crossing, Some(9), "{case}");
    }
}

/// A recorded run with a slow sweep, a scrape outage and a write window,
/// from two replicas: every value of the reply, by hand.
#[test]
fn recorded_run_with_a_slow_sweep_and_an_outage() {
    let r = 1_700_000_000.0;
    let process = r - 3600.0;
    // (start, end, count) from R. The sweep ending at +71 took 12 s.
    let sweeps: Vec<Published> = [
        (-11.0, -8.0, 0),
        (9.0, 13.0, 2),
        (29.0, 33.0, 2),
        (59.0, 71.0, 5),
        (87.0, 92.0, 6),
    ]
    .into_iter()
    .map(|(start, end, count)| Published {
        process,
        start: r + start,
        end: r + end,
        count,
    })
    .collect();
    // Replica a scrapes every 5 s, b 2.5 s after it; neither scrapes from
    // +40 to +55.
    let mut store = Recorder::default();
    for i in -1..=20 {
        let at = 5.0 * f64::from(i);
        if (40.0..=55.0).contains(&at) {
            continue;
        }
        record(&mut store, &sweeps, &[], r + at, "a");
        record(&mut store, &sweeps, &[], r + at + 2.5, "b");
    }
    let reply = run(&store, r + 200.0, &params(r, r + 100.0, 6, "0.5,1", 5.0));

    let crossings: Vec<_> = reply
        .crossings
        .iter()
        .map(|c| {
            (
                c.target,
                c.reached_at,
                c.seconds,
                c.worst_count_after,
                c.scrape_gap_before,
            )
        })
        .collect();
    assert_eq!(
        crossings,
        vec![
            (3, Some(utc(r + 71.0)), Some(71.0), Some(5), true),
            (6, Some(utc(r + 92.0)), Some(92.0), Some(6), true),
        ]
    );
    // Sweep gaps -8 → 13 → 33 → 71 → 92 → 100: 38 is the largest.
    assert_eq!(reply.max_sweep_gap_seconds, Some(38.0));
    assert_eq!(reply.max_sweep_duration_seconds, Some(12.0));
    assert_eq!(reply.timing_error_seconds, Some(38.0 + 12.0 + 5.0));
    assert_eq!(reply.add_window_seconds, 5.0);
    // Only the outage: b's +37.5 to a's +60.
    assert_eq!(reply.scrape_gaps_total, 1);
    let gap = &reply.scrape_gaps[0];
    assert_eq!(
        (gap.from, gap.to, gap.seconds, gap.new_sweep_across),
        (utc(r + 37.5), utc(r + 60.0), 22.5, false)
    );
    assert_eq!(reply.uncovered_scrape_gap_seconds, 0.0);
    assert!(reply.final_scrape_seen);
    assert_eq!(reply.final_count, Some(6));
    assert_eq!(reply.final_sweep_at, Some(utc(r + 92.0)));
    assert_eq!(reply.sweeps_seen, 4);
    assert_eq!(
        reply.curve.iter().map(|p| p.count).collect::<Vec<_>>(),
        vec![2, 2, 5, 6]
    );
    assert_eq!(reply.max_zero_progress_seconds, Some(58.0));
    assert_eq!(
        reply.units_per_minute.sustained,
        Some(0.8 * 6.0 / (79.0 / 60.0))
    );
    assert_eq!(reply.units_per_minute.peak_10m, Some(0.6));
    assert_eq!(reply.probe_restarts, 0);
    assert_eq!(reply.incomplete_sweeps, 0);
    assert_eq!(reply.worst_count_after_first_crossing, Some(5));

    // The same run read before its last scrapes landed: the sweep that
    // ended at +92 is not scraped yet, and the missing tail is a gap.
    let mut early = Recorder::default();
    for i in -1..=17 {
        let at = 5.0 * f64::from(i);
        if !(40.0..=55.0).contains(&at) {
            record(&mut early, &sweeps, &[], r + at, "a");
        }
    }
    let reply = run(&early, r + 100.0, &params(r, r + 100.0, 6, "0.5,1", 5.0));
    assert!(!reply.final_scrape_seen);
    assert_eq!(reply.final_count, Some(5));
    assert_eq!(reply.crossings[1].reached_at, None);
    assert_eq!(reply.scrape_gaps_total, 2, "the outage and the tail");
}
