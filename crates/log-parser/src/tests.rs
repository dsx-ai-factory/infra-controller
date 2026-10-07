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

use std::collections::HashMap;

use carbide_test_support::Outcome::{Fails, Yields};
use carbide_test_support::{Case, Check, check_cases, check_values};
use regex::Regex;
use serde_json::json;

use super::{
    Configuration, Event, EventConstraints, EventSeverity, EventType, LogFile, MAX_EVENTS,
    check_constraints, process_events, process_log_file_events, queue_event, read_event_definition,
};

fn event_type(name: impl Into<String>) -> EventType {
    EventType {
        regex_string: String::new(),
        name: name.into(),
        description: None,
        target: None,
        severity: EventSeverity::Information,
        ignore_case: false,
        alert: false,
        clears: Vec::new(),
        constraints: None,
    }
}

fn event(name: impl Into<String>, timestamp: i64) -> Event {
    Event {
        name: name.into(),
        description: None,
        target: None,
        severity: EventSeverity::Information,
        alert: false,
        cleared: false,
        timestamp: chrono::DateTime::from_timestamp(timestamp, 0).unwrap(),
        log_entry: String::new(),
        machine_id: String::new(),
        ids: HashMap::new(),
    }
}

#[derive(Clone, Copy)]
struct Rule {
    name: &'static str,
    pattern: &'static str,
    ignore_case: bool,
    constrained: bool,
}

struct RuleRow {
    rules: &'static [Rule],
    buffer: &'static str,
}

fn matching_rules(row: RuleRow) -> Vec<String> {
    let event_types = row
        .rules
        .iter()
        .map(|rule| EventType {
            regex_string: rule.pattern.to_string(),
            name: rule.name.to_string(),
            ignore_case: rule.ignore_case,
            constraints: rule.constrained.then_some(EventConstraints {
                duration: Some(10),
                count: Some(1),
                preceded_by: None,
            }),
            ..event_type(rule.name)
        })
        .collect::<Vec<_>>();
    let regexes = row
        .rules
        .iter()
        .map(|rule| Regex::new(rule.pattern).unwrap())
        .collect::<Vec<_>>();
    let mut log = LogFile::default();

    process_events(100, &event_types, &regexes, &mut log, row.buffer);

    log.events.into_iter().map(|event| event.name).collect()
}

#[test]
fn rule_matching_preserves_declaration_order() {
    check_values(
        [
            Check {
                scenario: "case-sensitive rule",
                input: RuleRow {
                    rules: &[Rule {
                        name: "sensitive",
                        pattern: "ERROR",
                        ignore_case: false,
                        constrained: false,
                    }],
                    buffer: "ERROR",
                },
                expect: vec!["sensitive".to_string()],
            },
            Check {
                scenario: "case-sensitive rule rejects different casing",
                input: RuleRow {
                    rules: &[Rule {
                        name: "sensitive",
                        pattern: "ERROR",
                        ignore_case: false,
                        constrained: false,
                    }],
                    buffer: "error",
                },
                expect: Vec::new(),
            },
            Check {
                scenario: "ASCII-insensitive rule",
                input: RuleRow {
                    rules: &[Rule {
                        name: "insensitive",
                        pattern: "warning",
                        ignore_case: true,
                        constrained: false,
                    }],
                    buffer: "WaRnInG",
                },
                expect: vec!["insensitive".to_string()],
            },
            Check {
                scenario: "ASCII-insensitive rule rejects a different message",
                input: RuleRow {
                    rules: &[Rule {
                        name: "insensitive",
                        pattern: "warning",
                        ignore_case: true,
                        constrained: false,
                    }],
                    buffer: "healthy",
                },
                expect: Vec::new(),
            },
            Check {
                scenario: "matching rules retain declaration order",
                input: RuleRow {
                    rules: &[
                        Rule {
                            name: "first",
                            pattern: "fault",
                            ignore_case: false,
                            constrained: false,
                        },
                        Rule {
                            name: "second",
                            pattern: "fault",
                            ignore_case: false,
                            constrained: false,
                        },
                    ],
                    buffer: "fault",
                },
                expect: vec!["first".to_string(), "second".to_string()],
            },
            Check {
                scenario: "case-sensitive match dispatches its constraint",
                input: RuleRow {
                    rules: &[Rule {
                        name: "sensitive-summary",
                        pattern: "ERROR",
                        ignore_case: false,
                        constrained: true,
                    }],
                    buffer: "ERROR",
                },
                expect: vec!["sensitive-summary".to_string()],
            },
            Check {
                scenario: "ASCII-insensitive match dispatches its constraint",
                input: RuleRow {
                    rules: &[Rule {
                        name: "insensitive-summary",
                        pattern: "warning",
                        ignore_case: true,
                        constrained: true,
                    }],
                    buffer: "WaRnInG",
                },
                expect: vec!["insensitive-summary".to_string()],
            },
        ],
        matching_rules,
    );
}

#[derive(Clone, Copy)]
enum Constraint {
    Frequency { count: u32, duration: i64 },
    History(&'static [&'static str]),
    Empty,
}

#[derive(Clone, Copy)]
struct ConstraintInvocation {
    event_name: &'static str,
    timestamp: i64,
}

struct ConstraintRow {
    constraint: Constraint,
    history: &'static [&'static str],
    invocations: &'static [ConstraintInvocation],
}

#[derive(Debug, Eq, PartialEq)]
struct ConstraintObservation {
    queued: Vec<String>,
    pending_event: Option<String>,
    pending_count: u32,
    pending_timestamp: Option<i64>,
}

fn evaluate_constraint(row: ConstraintRow) -> ConstraintObservation {
    let constraints = match row.constraint {
        Constraint::Frequency { count, duration } => EventConstraints {
            duration: Some(duration),
            count: Some(count),
            preceded_by: None,
        },
        Constraint::History(pattern) => EventConstraints {
            duration: None,
            count: None,
            preceded_by: Some(pattern.iter().map(|name| (*name).to_string()).collect()),
        },
        Constraint::Empty => EventConstraints {
            duration: None,
            count: None,
            preceded_by: None,
        },
    };
    let mut log = LogFile::default();
    log.events.extend(
        row.history
            .iter()
            .enumerate()
            .map(|(index, name)| event(*name, index as i64)),
    );
    let history_len = log.events.len();

    for invocation in row.invocations {
        check_constraints(
            &event_type(invocation.event_name),
            &constraints,
            &mut log,
            invocation.timestamp,
            invocation.event_name,
        );
    }

    ConstraintObservation {
        queued: log
            .events
            .iter()
            .skip(history_len)
            .map(|event| event.name.clone())
            .collect(),
        pending_event: log.pending_event,
        pending_count: log.pending_event_count,
        pending_timestamp: log.pending_event_ts,
    }
}

#[test]
fn constraint_windows_and_histories() {
    check_values(
        [
            Check {
                scenario: "frequency threshold is met inside its duration",
                input: ConstraintRow {
                    constraint: Constraint::Frequency {
                        count: 2,
                        duration: 10,
                    },
                    history: &[],
                    invocations: &[
                        ConstraintInvocation {
                            event_name: "summary",
                            timestamp: 100,
                        },
                        ConstraintInvocation {
                            event_name: "summary",
                            timestamp: 109,
                        },
                    ],
                },
                expect: ConstraintObservation {
                    queued: vec!["summary".to_string()],
                    pending_event: None,
                    pending_count: 0,
                    pending_timestamp: None,
                },
            },
            Check {
                scenario: "frequency threshold includes the exact duration boundary",
                input: ConstraintRow {
                    constraint: Constraint::Frequency {
                        count: 2,
                        duration: 10,
                    },
                    history: &[],
                    invocations: &[
                        ConstraintInvocation {
                            event_name: "summary",
                            timestamp: 100,
                        },
                        ConstraintInvocation {
                            event_name: "summary",
                            timestamp: 110,
                        },
                    ],
                },
                expect: ConstraintObservation {
                    queued: vec!["summary".to_string()],
                    pending_event: None,
                    pending_count: 0,
                    pending_timestamp: None,
                },
            },
            Check {
                scenario: "expired frequency window restarts before counting again",
                input: ConstraintRow {
                    constraint: Constraint::Frequency {
                        count: 2,
                        duration: 10,
                    },
                    history: &[],
                    invocations: &[
                        ConstraintInvocation {
                            event_name: "summary",
                            timestamp: 100,
                        },
                        ConstraintInvocation {
                            event_name: "summary",
                            timestamp: 111,
                        },
                        ConstraintInvocation {
                            event_name: "summary",
                            timestamp: 112,
                        },
                    ],
                },
                expect: ConstraintObservation {
                    queued: vec!["summary".to_string()],
                    pending_event: None,
                    pending_count: 0,
                    pending_timestamp: None,
                },
            },
            Check {
                scenario: "a different event starts a new pending frequency",
                input: ConstraintRow {
                    constraint: Constraint::Frequency {
                        count: 2,
                        duration: 10,
                    },
                    history: &[],
                    invocations: &[
                        ConstraintInvocation {
                            event_name: "first-summary",
                            timestamp: 100,
                        },
                        ConstraintInvocation {
                            event_name: "second-summary",
                            timestamp: 101,
                        },
                    ],
                },
                expect: ConstraintObservation {
                    queued: Vec::new(),
                    pending_event: Some("second-summary".to_string()),
                    pending_count: 1,
                    pending_timestamp: Some(101),
                },
            },
            Check {
                scenario: "complete history matches",
                input: ConstraintRow {
                    constraint: Constraint::History(&["first", "second"]),
                    history: &["first", "second"],
                    invocations: &[ConstraintInvocation {
                        event_name: "summary",
                        timestamp: 100,
                    }],
                },
                expect: ConstraintObservation {
                    queued: vec!["summary".to_string()],
                    pending_event: None,
                    pending_count: 0,
                    pending_timestamp: None,
                },
            },
            Check {
                scenario: "matching history may be a suffix of older events",
                input: ConstraintRow {
                    constraint: Constraint::History(&["first", "second"]),
                    history: &["older", "first", "second"],
                    invocations: &[ConstraintInvocation {
                        event_name: "summary",
                        timestamp: 100,
                    }],
                },
                expect: ConstraintObservation {
                    queued: vec!["summary".to_string()],
                    pending_event: None,
                    pending_count: 0,
                    pending_timestamp: None,
                },
            },
            Check {
                scenario: "history in the wrong order does not match",
                input: ConstraintRow {
                    constraint: Constraint::History(&["first", "second"]),
                    history: &["second", "first"],
                    invocations: &[ConstraintInvocation {
                        event_name: "summary",
                        timestamp: 100,
                    }],
                },
                expect: ConstraintObservation {
                    queued: Vec::new(),
                    pending_event: None,
                    pending_count: 0,
                    pending_timestamp: None,
                },
            },
            Check {
                scenario: "short history does not satisfy a longer pattern",
                input: ConstraintRow {
                    constraint: Constraint::History(&["first", "second"]),
                    history: &["second"],
                    invocations: &[ConstraintInvocation {
                        event_name: "summary",
                        timestamp: 100,
                    }],
                },
                expect: ConstraintObservation {
                    queued: Vec::new(),
                    pending_event: None,
                    pending_count: 0,
                    pending_timestamp: None,
                },
            },
            Check {
                scenario: "empty history does not satisfy a nonempty pattern",
                input: ConstraintRow {
                    constraint: Constraint::History(&["first"]),
                    history: &[],
                    invocations: &[ConstraintInvocation {
                        event_name: "summary",
                        timestamp: 100,
                    }],
                },
                expect: ConstraintObservation {
                    queued: Vec::new(),
                    pending_event: None,
                    pending_count: 0,
                    pending_timestamp: None,
                },
            },
            Check {
                scenario: "empty history pattern does not match",
                input: ConstraintRow {
                    constraint: Constraint::History(&[]),
                    history: &["existing"],
                    invocations: &[ConstraintInvocation {
                        event_name: "summary",
                        timestamp: 100,
                    }],
                },
                expect: ConstraintObservation {
                    queued: Vec::new(),
                    pending_event: None,
                    pending_count: 0,
                    pending_timestamp: None,
                },
            },
            Check {
                scenario: "empty constraint does not queue an event",
                input: ConstraintRow {
                    constraint: Constraint::Empty,
                    history: &[],
                    invocations: &[ConstraintInvocation {
                        event_name: "summary",
                        timestamp: 100,
                    }],
                },
                expect: ConstraintObservation {
                    queued: Vec::new(),
                    pending_event: None,
                    pending_count: 0,
                    pending_timestamp: None,
                },
            },
        ],
        evaluate_constraint,
    );
}

#[derive(Clone, Copy)]
struct QueuedEvent {
    name: &'static str,
    description: Option<&'static str>,
    target: Option<&'static str>,
    severity: EventSeverity,
    alert: bool,
    clears: &'static [&'static str],
    timestamp: i64,
    buffer: &'static str,
}

struct QueueRow {
    initial_names: Vec<String>,
    queued: QueuedEvent,
    machine_id: Option<&'static str>,
    rack_id: Option<&'static str>,
    inspect: &'static [&'static str],
}

#[derive(Debug, Eq, PartialEq)]
struct QueuedMetadata {
    description: Option<String>,
    target: Option<String>,
    severity: EventSeverity,
    alert: bool,
    timestamp: i64,
    log_entry: String,
    machine_id: String,
    ids: Vec<(String, String)>,
}

#[derive(Debug, Eq, PartialEq)]
struct QueueObservation {
    len: usize,
    first_name: Option<String>,
    last_name: Option<String>,
    inspected_cleared: Vec<(String, bool)>,
    last_metadata: Option<QueuedMetadata>,
}

fn inspect_queue(row: QueueRow) -> QueueObservation {
    let mut log = LogFile::default();
    if let Some(machine_id) = row.machine_id {
        log.file_name_fields
            .insert("machine_id".to_string(), machine_id.to_string());
    }
    if let Some(rack_id) = row.rack_id {
        log.file_name_fields
            .insert("rack_id".to_string(), rack_id.to_string());
    }
    for (index, name) in row.initial_names.iter().enumerate() {
        queue_event(index as i64, &event_type(name), &mut log, name);
    }

    let mut queued_type = event_type(row.queued.name);
    queued_type.description = row.queued.description.map(str::to_string);
    queued_type.target = row.queued.target.map(str::to_string);
    queued_type.severity = row.queued.severity;
    queued_type.alert = row.queued.alert;
    queued_type.clears = row
        .queued
        .clears
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    queue_event(
        row.queued.timestamp,
        &queued_type,
        &mut log,
        row.queued.buffer,
    );

    let inspected_cleared = row
        .inspect
        .iter()
        .map(|name| {
            let cleared = log
                .events
                .iter()
                .find(|event| event.name == *name)
                .map(|event| event.cleared)
                .unwrap();
            ((*name).to_string(), cleared)
        })
        .collect();
    let last_metadata = log.events.back().map(|event| {
        let mut ids = event
            .ids
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Vec<_>>();
        ids.sort();
        QueuedMetadata {
            description: event.description.clone(),
            target: event.target.clone(),
            severity: event.severity,
            alert: event.alert,
            timestamp: event.timestamp.timestamp(),
            log_entry: event.log_entry.clone(),
            machine_id: event.machine_id.clone(),
            ids,
        }
    });

    QueueObservation {
        len: log.events.len(),
        first_name: log.events.front().map(|event| event.name.clone()),
        last_name: log.events.back().map(|event| event.name.clone()),
        inspected_cleared,
        last_metadata,
    }
}

fn queued_event(name: &'static str) -> QueuedEvent {
    QueuedEvent {
        name,
        description: None,
        target: None,
        severity: EventSeverity::Information,
        alert: false,
        clears: &[],
        timestamp: 200,
        buffer: name,
    }
}

#[test]
fn queued_events_copy_metadata_clear_matches_and_evict_oldest() {
    check_values(
        [
            Check {
                scenario: "event and filename metadata are copied",
                input: QueueRow {
                    initial_names: Vec::new(),
                    queued: QueuedEvent {
                        name: "fault",
                        description: Some("A fault was detected"),
                        target: Some("DPU"),
                        severity: EventSeverity::Critical,
                        alert: true,
                        clears: &[],
                        timestamp: 123,
                        buffer: "fault details",
                    },
                    machine_id: Some("machine-1"),
                    rack_id: Some("rack-2"),
                    inspect: &["fault"],
                },
                expect: QueueObservation {
                    len: 1,
                    first_name: Some("fault".to_string()),
                    last_name: Some("fault".to_string()),
                    inspected_cleared: vec![("fault".to_string(), false)],
                    last_metadata: Some(QueuedMetadata {
                        description: Some("A fault was detected".to_string()),
                        target: Some("DPU".to_string()),
                        severity: EventSeverity::Critical,
                        alert: true,
                        timestamp: 123,
                        log_entry: "fault details".to_string(),
                        machine_id: "machine-1".to_string(),
                        ids: vec![
                            ("machine_id".to_string(), "machine-1".to_string()),
                            ("rack_id".to_string(), "rack-2".to_string()),
                        ],
                    }),
                },
            },
            Check {
                scenario: "clear patterns leave unrelated events active",
                input: QueueRow {
                    initial_names: vec![
                        "power_fault".to_string(),
                        "temperature".to_string(),
                        "fan_fault".to_string(),
                    ],
                    queued: QueuedEvent {
                        clears: &["fault"],
                        ..queued_event("recovered")
                    },
                    machine_id: None,
                    rack_id: None,
                    inspect: &["power_fault", "temperature", "fan_fault", "recovered"],
                },
                expect: QueueObservation {
                    len: 4,
                    first_name: Some("power_fault".to_string()),
                    last_name: Some("recovered".to_string()),
                    inspected_cleared: vec![
                        ("power_fault".to_string(), true),
                        ("temperature".to_string(), false),
                        ("fan_fault".to_string(), true),
                        ("recovered".to_string(), false),
                    ],
                    last_metadata: Some(QueuedMetadata {
                        description: None,
                        target: None,
                        severity: EventSeverity::Information,
                        alert: false,
                        timestamp: 200,
                        log_entry: "recovered".to_string(),
                        machine_id: String::new(),
                        ids: Vec::new(),
                    }),
                },
            },
            Check {
                scenario: "the oldest event is evicted at the history limit",
                input: QueueRow {
                    initial_names: (0..MAX_EVENTS)
                        .map(|index| format!("event-{index}"))
                        .collect(),
                    queued: queued_event("newest"),
                    machine_id: None,
                    rack_id: None,
                    inspect: &["event-1", "newest"],
                },
                expect: QueueObservation {
                    len: MAX_EVENTS,
                    first_name: Some("event-1".to_string()),
                    last_name: Some("newest".to_string()),
                    inspected_cleared: vec![
                        ("event-1".to_string(), false),
                        ("newest".to_string(), false),
                    ],
                    last_metadata: Some(QueuedMetadata {
                        description: None,
                        target: None,
                        severity: EventSeverity::Information,
                        alert: false,
                        timestamp: 200,
                        log_entry: "newest".to_string(),
                        machine_id: String::new(),
                        ids: Vec::new(),
                    }),
                },
            },
        ],
        inspect_queue,
    );
}

struct DefinitionRow {
    filename_format: &'static str,
    event_patterns: &'static [&'static str],
    filename: &'static str,
    event_samples: &'static [&'static str],
    capture_name: Option<&'static str>,
}

#[derive(Debug, Eq, PartialEq)]
enum DefinitionObservation {
    Parsed {
        filename_matches: bool,
        capture: Option<String>,
        event_matches: Vec<bool>,
    },
    Rejected,
}

fn inspect_definition(
    runtime: &tokio::runtime::Runtime,
    directory: &tempfile::TempDir,
    row: DefinitionRow,
) -> DefinitionObservation {
    let events = row
        .event_patterns
        .iter()
        .enumerate()
        .map(|(index, pattern)| {
            json!({
                "regex_string": pattern,
                "name": format!("event-{index}"),
                "description": null,
                "target": null,
                "severity": "Warning",
                "ignore_case": false,
                "alert": true,
                "clears": [],
                "constraints": null,
            })
        })
        .collect::<Vec<_>>();
    let definition = json!({
        "pipeline": "test",
        "delimiter": null,
        "filename_format": row.filename_format,
        "logs_path": "/unused",
        "events": events,
    });
    let path = directory.path().join("definition.json");
    std::fs::write(&path, serde_json::to_vec(&definition).unwrap()).unwrap();

    match runtime.block_on(read_event_definition(&path)) {
        Ok(configuration) => {
            let filename_regex = configuration.filename_regex.unwrap();
            let captures = filename_regex.captures(row.filename);
            let capture = row
                .capture_name
                .and_then(|name| captures.as_ref()?.name(name))
                .map(|value| value.as_str().to_string());
            let event_matches = configuration
                .events_regex
                .unwrap()
                .iter()
                .zip(row.event_samples)
                .map(|(regex, sample)| regex.is_match(sample))
                .collect();
            DefinitionObservation::Parsed {
                filename_matches: captures.is_some(),
                capture,
                event_matches,
            }
        }
        Err(_) => DefinitionObservation::Rejected,
    }
}

#[test]
fn event_definitions_compile_filename_and_event_regexes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();

    check_values(
        [
            Check {
                scenario: "valid filename and event regexes",
                input: DefinitionRow {
                    filename_format: r"^node-\d+\.log$",
                    event_patterns: &[r"^ready$"],
                    filename: "node-42.log",
                    event_samples: &["ready"],
                    capture_name: None,
                },
                expect: DefinitionObservation::Parsed {
                    filename_matches: true,
                    capture: None,
                    event_matches: vec![true],
                },
            },
            Check {
                scenario: "doubly escaped patterns are normalized",
                input: DefinitionRow {
                    filename_format: r"^node-(?<machine_id>\\d+)\\.log$",
                    event_patterns: &[r"temperature:\\s+\\d+"],
                    filename: "node-42.log",
                    event_samples: &["temperature: 72"],
                    capture_name: Some("machine_id"),
                },
                expect: DefinitionObservation::Parsed {
                    filename_matches: true,
                    capture: Some("42".to_string()),
                    event_matches: vec![true],
                },
            },
            Check {
                scenario: "named filename field is captured",
                input: DefinitionRow {
                    filename_format: r"^rack-(?<rack_id>[a-z]+)\.log$",
                    event_patterns: &["ready"],
                    filename: "rack-west.log",
                    event_samples: &["ready"],
                    capture_name: Some("rack_id"),
                },
                expect: DefinitionObservation::Parsed {
                    filename_matches: true,
                    capture: Some("west".to_string()),
                    event_matches: vec![true],
                },
            },
            Check {
                scenario: "empty filename regex",
                input: DefinitionRow {
                    filename_format: "",
                    event_patterns: &["ready"],
                    filename: "node-42.log",
                    event_samples: &["ready"],
                    capture_name: None,
                },
                expect: DefinitionObservation::Rejected,
            },
            Check {
                scenario: "invalid filename regex",
                input: DefinitionRow {
                    filename_format: "[",
                    event_patterns: &["ready"],
                    filename: "node-42.log",
                    event_samples: &["ready"],
                    capture_name: None,
                },
                expect: DefinitionObservation::Rejected,
            },
            Check {
                scenario: "empty event regex",
                input: DefinitionRow {
                    filename_format: ".*",
                    event_patterns: &[""],
                    filename: "node-42.log",
                    event_samples: &["ready"],
                    capture_name: None,
                },
                expect: DefinitionObservation::Rejected,
            },
            Check {
                scenario: "invalid event regex",
                input: DefinitionRow {
                    filename_format: ".*",
                    event_patterns: &["("],
                    filename: "node-42.log",
                    event_samples: &["ready"],
                    capture_name: None,
                },
                expect: DefinitionObservation::Rejected,
            },
        ],
        |row| inspect_definition(&runtime, &directory, row),
    );
}

struct DelimiterRow {
    delimiter: Option<&'static str>,
}

fn inspect_delimiter(
    runtime: &tokio::runtime::Runtime,
    directory: &tempfile::TempDir,
    row: DelimiterRow,
) -> Result<(), ()> {
    let definition = json!({
        "pipeline": "test",
        "delimiter": row.delimiter,
        "filename_format": ".*",
        "logs_path": "/unused",
        "events": [],
    });
    let path = directory.path().join("delimiter-definition.json");
    std::fs::write(&path, serde_json::to_vec(&definition).unwrap()).unwrap();

    runtime
        .block_on(read_event_definition(&path))
        .map(drop)
        .map_err(drop)
}

#[test]
fn event_definition_delimiter_is_exactly_one_byte() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();

    check_cases(
        [
            Case {
                scenario: "omitted delimiter defaults to newline",
                input: DelimiterRow { delimiter: None },
                expect: Yields(()),
            },
            Case {
                scenario: "single-byte delimiter",
                input: DelimiterRow {
                    delimiter: Some("|"),
                },
                expect: Yields(()),
            },
            Case {
                scenario: "empty delimiter",
                input: DelimiterRow {
                    delimiter: Some(""),
                },
                expect: Fails,
            },
            Case {
                scenario: "multiple-byte ASCII delimiter",
                input: DelimiterRow {
                    delimiter: Some("||"),
                },
                expect: Fails,
            },
            Case {
                scenario: "multiple-byte UTF-8 delimiter",
                input: DelimiterRow {
                    delimiter: Some("→"),
                },
                expect: Fails,
            },
        ],
        |row| inspect_delimiter(&runtime, &directory, row),
    );
}

#[tokio::test]
async fn log_records_use_the_configured_delimiter() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("events.log");
    std::fs::write(&path, b"matched|unfinished").unwrap();

    let mut configuration = Configuration {
        filename: None,
        pipeline: "test".to_string(),
        delimiter: Some("|".to_string()),
        filename_format: ".*".to_string(),
        filename_regex: None,
        logs_path: path.to_string_lossy().into_owned(),
        events: vec![event_type("matched")],
        events_regex: Some(vec![Regex::new("^matched$").unwrap()]),
        logs: vec![LogFile {
            file_path: path.to_string_lossy().into_owned(),
            ..Default::default()
        }],
        logs_hash: HashMap::new(),
    };

    process_log_file_events(&mut configuration, 0, false)
        .await
        .unwrap();

    let log = &configuration.logs[0];
    assert_eq!(log.offset, b"matched|".len() as u64);
    assert_eq!(
        log.events
            .iter()
            .map(|event| event.name.as_str())
            .collect::<Vec<_>>(),
        vec!["matched"]
    );
}
