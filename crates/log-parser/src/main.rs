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
#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

mod carbide_reporting;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::io::SeekFrom;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::{fmt, time};

use anyhow::anyhow;
use carbide_reporting::{create_forge_client, get_client_cert_info, get_forge_root_ca_path};
use chrono::{DateTime, Utc};
use regex::{Captures, Regex};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::carbide_reporting::send_health_alerts;
const MAX_EVENTS: usize = 128;

#[derive(Debug, Deserialize, Copy, Clone, Eq, PartialEq)]
enum EventSeverity {
    Critical,
    Warning,
    Information,
    Debug,
}

impl fmt::Display for EventSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Mode {
    Monitor,
    Oneshot,
    Unknown,
}

/// for summary events that occur based on the frequency or pattern of other events occurring
#[derive(Deserialize, Debug)]
struct EventConstraints {
    /// the event has occurred if it met a certain frequency of occurrence
    /// repeated > count inside of duration (seconds)
    /// defaults are zero for event occurring every time
    pub duration: Option<i64>,
    pub count: Option<u32>,
    /// alternatively (instead of frequency), the event has occurred if a pattern of events occurred
    /// event is preceded by one or more events specified by names in chronological order
    /// i.e.: [oldest event in pattern, oldest + 1, ..., latest event]
    pub preceded_by: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
struct EventType {
    pub regex_string: String,
    /// identifier for the event
    pub name: String,
    pub description: Option<String>,
    pub target: Option<String>,
    pub severity: EventSeverity,
    pub ignore_case: bool,
    pub alert: bool,
    pub clears: Vec<String>,
    pub constraints: Option<EventConstraints>,
}

#[derive(Debug)]
struct Event {
    pub name: String,
    pub description: Option<String>,
    pub target: Option<String>,
    pub severity: EventSeverity,
    pub alert: bool,
    pub cleared: bool,
    pub timestamp: DateTime<Utc>,
    pub log_entry: String,
    pub machine_id: String,
    pub ids: HashMap<String, String>,
}

#[derive(Deserialize, Debug)]
struct Configuration {
    #[serde(skip)]
    pub filename: Option<String>,
    pub pipeline: String,
    pub delimiter: Option<String>,
    pub filename_format: String,
    #[serde(skip)]
    pub filename_regex: Option<Regex>,
    /// directory or log file to look at (no recursion),
    pub logs_path: String,
    /// rules for events to parse for
    pub events: Vec<EventType>,
    #[serde(skip)]
    pub events_regex: Option<Vec<Regex>>,
    #[serde(skip)]
    /// the log files being currently tracked
    pub logs: Vec<LogFile>,
    /// to lookup a log struct index in the vec
    #[serde(skip)]
    pub logs_hash: HashMap<Vec<u8>, usize>,
}

impl Configuration {
    fn delimiter_byte(&self) -> Result<u8, anyhow::Error> {
        match self.delimiter.as_deref() {
            None => Ok(b'\n'),
            Some(delimiter) => match delimiter.as_bytes() {
                [delimiter] => Ok(*delimiter),
                bytes => Err(anyhow!(
                    "delimiter must be exactly one byte, got {delimiter:?} ({} bytes)",
                    bytes.len()
                )),
            },
        }
    }
}

/// track each log file we're looking for events in
/// we only care about the current offset into the file we want to read and search from
/// the length should always be the latest known length of the file to read up to
/// we track 2 prior events:
/// the last one that occurred, if a new one depends on it
/// a pending event that requires a certain number of occurrences in a time window or frequency
/// we reopen the file each time we want to read, so that we're not affected by truncation or
/// deletion and stale file handles
#[derive(Debug)]
struct LogFile {
    pub file_path: String,
    pub file_name_fields: HashMap<String, String>,
    pub offset: u64,
    pub length: u64,
    pub pending_event: Option<String>,
    pub pending_event_count: u32,
    /// first time this event was detected, cleared/reset when the event constraint duration expires
    pub pending_event_ts: Option<i64>,
    pub events: VecDeque<Event>,
}

impl Default for LogFile {
    fn default() -> Self {
        Self {
            file_path: "".to_string(),
            file_name_fields: HashMap::default(),
            offset: 0,
            length: 0,
            pending_event: None,
            pending_event_count: 0,
            pending_event_ts: None,
            events: VecDeque::default(),
        }
    }
}

fn clear_prior_events(event_type: &EventType, log: &mut LogFile) {
    for event in log.events.iter_mut() {
        for clear_events in event_type.clears.iter() {
            if event.name.contains(clear_events) {
                event.cleared = true;
            }
        }
    }
}

fn queue_event(timestamp: i64, event_type: &EventType, log: &mut LogFile, buffer: &str) {
    if !event_type.clears.is_empty() {
        clear_prior_events(event_type, log);
    }
    let event = Event {
        name: event_type.name.clone(),
        description: event_type.description.clone(),
        target: event_type.target.clone(),
        severity: event_type.severity,
        alert: event_type.alert,
        cleared: false,
        timestamp: DateTime::from_timestamp(timestamp, 0).unwrap_or_default(),
        log_entry: buffer.to_string(),
        machine_id: log
            .file_name_fields
            .get("machine_id")
            .unwrap_or(&"".to_string())
            .to_string(),
        ids: log.file_name_fields.clone(),
    };
    if log.events.len() >= MAX_EVENTS {
        let _ = log.events.pop_front();
    }
    log.events.push_back(event);
}

fn check_constraints(
    event_type: &EventType,
    constraints: &EventConstraints,
    log: &mut LogFile,
    timestamp: i64,
    buffer: &str,
) {
    // duration and count of occurences of this event
    if constraints.count.is_some() && constraints.duration.is_some() {
        if Some(event_type.name.clone()) == log.pending_event {
            log.pending_event_count += 1;
            if let Some(start_ts) = log.pending_event_ts
                && let Some(duration) = constraints.duration
                && (timestamp - start_ts) > duration
            {
                // time window expired, reset it
                log.pending_event_ts = Some(timestamp);
                log.pending_event_count = 1;
            }
        } else {
            // setup the pending summary event.
            // we only support tracking one summary event across multiple regular events
            log.pending_event = Some(event_type.name.clone());
            log.pending_event_count = 1;
            log.pending_event_ts = Some(timestamp);
        }
        // now check the count
        if let Some(count) = constraints.count
            && log.pending_event_count >= count
        {
            log.pending_event_count = 0;
            log.pending_event_ts = None;
            log.pending_event = None;
            // send the summary event, it has met the constraints and considered as occurred
            queue_event(timestamp, event_type, log, buffer);
        }
    } else if let Some(event_pattern) = constraints.preceded_by.as_ref() {
        let event_pattern_matched = !event_pattern.is_empty()
            && log.events.len() >= event_pattern.len()
            && event_pattern
                .iter()
                .rev()
                .zip(log.events.iter().rev())
                .all(|(event_name, prior_event)| *event_name == prior_event.name);
        if event_pattern_matched {
            queue_event(timestamp, event_type, log, buffer);
        }
    }
}

fn process_events(
    timestamp: i64,
    event_types: &[EventType],
    event_regexes: &[Regex],
    log: &mut LogFile,
    buffer: &str,
) {
    // process slice
    for (event_type, regex_str) in event_types.iter().zip(event_regexes.iter()) {
        // check if ignore case is specified and match lowercase string (regex specified MUST be lowercase)
        if event_type.ignore_case {
            if let Some(_no_case_matched) = regex_str.captures(buffer.to_ascii_lowercase().as_str())
            {
                if let Some(constraints) = &event_type.constraints {
                    check_constraints(event_type, constraints, log, timestamp, buffer);
                } else {
                    queue_event(timestamp, event_type, log, buffer);
                }
            }
        } else if let Some(_matched) = regex_str.captures(buffer) {
            if let Some(constraints) = &event_type.constraints {
                check_constraints(event_type, constraints, log, timestamp, buffer);
            } else {
                queue_event(timestamp, event_type, log, buffer);
            }
        }
    }
}

/// look at the given log file and figure out how much to read from it
/// read from the file and generate events
async fn process_log_file_events(
    cfg: &mut Configuration,
    log_index: usize,
    set_offset: bool,
) -> Result<(), anyhow::Error> {
    let delimiter = cfg.delimiter_byte()?;
    if let Some(log) = cfg.logs.get_mut(log_index) {
        let file_length = tokio::fs::metadata(&log.file_path).await?.len();
        if file_length == log.length {
            // file unchanged (except if a very strange emitter truncates and writes the exact same length as seen before)
            return Ok(());
        }
        log.length = file_length;
        // amount of new data to read
        if set_offset {
            log.offset = file_length;
        }
        let len = if file_length < log.offset {
            // log has been truncated
            log.offset = 0;
            file_length
        } else {
            file_length - log.offset
        };
        // cap the buffer to 1MB
        let buffer_length = if len > 0x40000000 { 0x40000000 } else { len };

        let now: DateTime<Utc> = Utc::now();
        let mut file = tokio::fs::File::open(&log.file_path).await?;

        let mut consumed = 0;

        while consumed < len {
            let mut buffer = vec![0u8; buffer_length as usize];
            file.seek(SeekFrom::Start(log.offset)).await?;
            file.read_exact(&mut buffer).await?;
            consumed += buffer_length;

            if buffer.contains(&delimiter) {
                // find last delimiter and move seek offset to that, truncate buffer to that
                if let Some(seek_position) = buffer.iter().rev().position(|&c| c == delimiter) {
                    log.offset += buffer_length - seek_position as u64;
                    buffer.truncate(buffer_length as usize - seek_position);
                } else {
                    log.offset += buffer_length;
                }
                let segments = buffer.split(|&c| c == delimiter);
                for segment in segments {
                    if segment.is_empty() {
                        continue;
                    }
                    let str_buffer = String::from_utf8_lossy(segment);
                    process_events(
                        now.timestamp(),
                        &cfg.events,
                        cfg.events_regex.as_ref().unwrap(), // this Vec<regex> is guaranteed to exist at this point
                        log,
                        &str_buffer,
                    );
                }
            } else {
                eprintln!(
                    "{}: buffer of size {buffer_length} did not contain the delimiter",
                    log.file_path
                );
                log.offset += buffer_length;
            }
        }
    }

    Ok(())
}

/// add the given file path to the list of tracked files and setup its struct
fn add_log_file(
    cfg: &mut Configuration,
    file_path: &Path,
    file_name_matches: Captures,
) -> Result<usize, anyhow::Error> {
    let mut log = LogFile {
        file_path: file_path.to_str().unwrap_or_default().to_string(),
        ..Default::default()
    };
    // cfg.filename_regex is guaranteed to exist at this point
    for x in cfg
        .filename_regex
        .as_ref()
        .unwrap()
        .capture_names()
        .flatten()
    {
        if let Some(val) = file_name_matches.name(x.as_ref()) {
            log.file_name_fields
                .insert(x.to_string(), val.as_str().to_string());
        }
    }
    cfg.logs.push(log);
    let index = cfg.logs.len() - 1;
    cfg.logs_hash
        .insert(Vec::from(file_path.as_os_str().as_bytes()), index);
    Ok(index)
}

/// work on one log file
async fn one_log_file(
    cfg: &mut Configuration,
    file_path: &Path,
    file_name: &str,
    set_offset: bool,
) -> Result<(), anyhow::Error> {
    // cfg.filename_regex is guaranteed to exist at this point
    if let Some(matched) = cfg.filename_regex.as_ref().unwrap().captures(file_name) {
        // look for this file path in the currently tracked log file paths hash in this event pipeline
        let index = match cfg.logs_hash.get(file_path.as_os_str().as_bytes()) {
            Some(i) => {
                if *i >= cfg.logs.len() {
                    // remove the invalid entry so it gets correctly populated
                    eprintln!(
                        "invalid {i} for match {file_name} in logs vec for {}",
                        cfg.filename.as_ref().unwrap_or(&"invalid".to_string())
                    );
                    cfg.logs_hash.remove(file_path.as_os_str().as_bytes());
                    add_log_file(cfg, file_path, matched)?
                } else {
                    *i
                }
            }
            None => add_log_file(cfg, file_path, matched)?,
        };
        process_log_file_events(cfg, index, set_offset).await
    } else {
        eprintln!(
            "file name {file_name} did not match the regex pattern for events pipeline {}",
            cfg.filename.as_ref().unwrap_or(&"invalid".to_string())
        );
        Ok(())
    }
}

/// scan the given event pipeline path for filename_format regex matching files
async fn scan_files(cfg: &mut Configuration, set_offset: bool) -> Result<(), anyhow::Error> {
    let path_str = cfg.logs_path.clone();
    let path = Path::new(&path_str);
    if path.is_dir() {
        for f in path.read_dir().expect("read_dir failed").flatten() {
            if f.path().is_file()
                && let Some(name) = f.file_name().to_str()
            {
                // file name matches filename_format regex
                one_log_file(cfg, f.path().as_path(), name, set_offset).await?;
            }
        }
    } else if path.is_file() {
        if let Some(x) = path.file_name()
            && let Some(name) = x.to_str()
        {
            one_log_file(cfg, path, name, set_offset).await?;
        }
    } else {
        eprintln!(
            "Events pipeline {} path specified {} is not a directory or a file",
            cfg.filename.as_ref().unwrap_or(&"".to_string()),
            cfg.logs_path
        )
    }
    Ok(())
}

/// read the json event definition pipeline specified
/// for event definitions with regex patterns and constraints
async fn read_event_definition(path: &Path) -> Result<Configuration, anyhow::Error> {
    let json = tokio::fs::read_to_string(path).await?;
    let mut config: Configuration = serde_json::from_str(&json)?;
    config.delimiter_byte()?;
    config.filename = Some(
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
    );
    let filename_format = config.filename_format.replace("\\\\", "\\");
    if filename_format.is_empty() {
        return Err(anyhow!(
            "invalid filename_format regex pattern {}",
            config.filename_format
        ));
    }
    config.filename_regex = Some(Regex::new(filename_format.as_str())?);
    let mut events_regex = Vec::new();
    for event_type in &config.events {
        let regex_string = event_type.regex_string.replace("\\\\", "\\");
        if regex_string.is_empty() {
            return Err(anyhow!(
                "invalid event regex pattern {}",
                event_type.regex_string
            ));
        }
        let event_regex = Regex::new(regex_string.as_str())?;
        events_regex.push(event_regex);
    }
    config.events_regex = Some(events_regex);
    Ok(config)
}

fn help() {
    println!(
        "Usage: -c [carbide api url] -e <event definition file1,file2,..> -m <monitor|oneshot> -t [poll interval in seconds]"
    );
    println!("Examples:");
    println!(
        "log-parser -c https://carbide-api.forge-system.svc.cluster.local:1079 -e /opt/forge/event_definitions -m monitor -t 10"
    );
    println!("log-parser -e event_definition.json -m oneshot");
    println!("log-parser -v for application version");
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    // there's 2 modes of operation:
    // load event regex definitions from json
    // if monitoring for sending health alerts to carbide (default mode)
    // - open log streams read-only and monitor
    // - process buffer based on delimiter (default is newline)
    // - find full or partial matches
    // - process constraint rules
    // - deliver event health alert
    // else if testing new event definitions in toml/json or logs
    // - open log files read-only
    // - process buffer based on delimiter (default is newline)
    // - find full or partial matches
    // - process constraint rules (duration constraint is NOT applied)
    // - print event detected for buffer at position

    // each event definition json can point to a log or log directory path
    let mut opts = getopts::Options::new();
    opts.optflag("h", "help", "Print this help");
    opts.optopt("c", "carbide", "carbide api url", "api");
    opts.optopt(
        "e",
        "events",
        "event definition json files (specify multiple files comma separated)",
        "json",
    );
    opts.optopt(
        "m",
        "mode",
        "Operating mode, monitor|oneshot, monitor for carbide health reporting, oneshot for debugging on the terminal",
        "production/debugging",
    );
    opts.optopt(
        "t",
        "time",
        "Polling time interval in seconds (default=5s)",
        "number in seconds",
    );
    opts.optflag("v", "version", "Log parser application version");

    let args: Vec<String> = std::env::args().collect();
    let args_given = opts.parse(&args[1..])?;
    if args_given.opt_present("h") {
        help();
        return Ok(());
    }
    if args_given.opt_present("v") {
        println!(env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let api = args_given.opt_str("c").unwrap_or_default();

    let mut event_definitions: Vec<String> = Vec::new();
    if args_given.opt_present("e") {
        if let Some(e) = args_given.opt_str("e") {
            if e.contains(",") {
                for x in args_given.opt_str("e").unwrap().split(",") {
                    event_definitions.push(x.to_string());
                }
            } else {
                event_definitions.push(e);
            }
        }
    } else {
        help();
        return Ok(());
    }

    let mut poll_interval: u64 = 5;
    if args_given.opt_present("t")
        && let Some(t) = args_given.opt_str("t")
    {
        poll_interval = t.parse()?;
        if poll_interval == 0 {
            eprintln!("-t {t} time interval specified is invalid");
            help();
            return Ok(());
        }
    }
    let mut mode = Mode::Unknown;
    if args_given.opt_present("m")
        && let Some(m) = args_given.opt_str("m")
    {
        match m.as_str() {
            "monitor" => {
                if api.is_empty() {
                    eprintln!("-c carbide api url argument required for monitor mode");
                    help();
                    return Ok(());
                }
                mode = Mode::Monitor;
            }
            "oneshot" => {
                mode = Mode::Oneshot;
            }
            _ => {
                mode = Mode::Unknown;
            }
        }
    }

    if mode == Mode::Unknown {
        eprintln!("-m mode argument required");
        help();
        return Ok(());
    }

    let mut configs: Vec<Configuration> = Vec::new();
    for file in event_definitions {
        let path = Path::new(&file);
        if path.is_dir() {
            for f in path.read_dir().expect("read_dir failed").flatten() {
                if f.path().is_file() {
                    let config = read_event_definition(f.path().as_path()).await?;
                    configs.push(config);
                }
            }
        } else {
            let config = read_event_definition(path).await?;
            configs.push(config);
        }
    }

    if mode == Mode::Oneshot {
        for config in configs.iter_mut() {
            match scan_files(config, false).await {
                Ok(_) => {
                    println!(
                        "successfully processed event pipeline defined in {}",
                        config.filename.as_ref().unwrap_or(&"invalid".to_string())
                    );
                }
                Err(e) => {
                    eprintln!("{e}");
                }
            }
            for log in &config.logs {
                for event in &log.events {
                    for id in &event.ids {
                        println!("{}: {}", id.0, id.1);
                    }
                    println!(
                        "[{}] [{}] {}: {}",
                        event.timestamp,
                        event.severity,
                        event.name,
                        event.description.clone().unwrap_or_default()
                    );
                    println!("from: {}", event.log_entry);
                }
            }
        }
        return Ok(());
    }

    let root_ca = get_forge_root_ca_path(None);
    let client_certs = get_client_cert_info(None, None);

    let mut forge_client =
        create_forge_client(root_ca, client_certs.cert_path, client_certs.key_path, api)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
    // in monitoring mode, start at the end of each log file and monitor so that we don't send stale events
    let mut set_offset = true;
    let mut first_grpc_error_stamp = 0;
    loop {
        // process every event definition json config file found
        for config in configs.iter_mut() {
            // tolerate any errors during logs processing
            match scan_files(config, set_offset).await {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("{e}");
                }
            }
            for log in &config.logs {
                match send_health_alerts(
                    &mut forge_client,
                    &log.events,
                    &config.pipeline,
                    &log.file_path,
                )
                .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("{e}");
                        let now: DateTime<Utc> = Utc::now();
                        if first_grpc_error_stamp == 0 {
                            first_grpc_error_stamp = now.timestamp();
                        } else if now.timestamp() - first_grpc_error_stamp > 600 {
                            // 10 minutes of grpc errors
                            return Err(e.into());
                        }
                    }
                }
            }
            tokio::time::sleep(time::Duration::from_secs(poll_interval)).await;
        }
        set_offset = false;
    }
}
