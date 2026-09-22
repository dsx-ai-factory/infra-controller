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

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

use super::{CollectorEvent, DataSink, EventContext, LogRecord};
use crate::HealthError;
use crate::config::LogFileSinkConfig;

/// Durable JSONL log sink. Writes CollectorEvent::Log records to rotating
/// files using sync I/O, safe to call from DataSink::handle_event.
pub struct LogFileSink {
    writer: Mutex<SyncLogFileWriter>,
    include_diagnostics: bool,
}

impl LogFileSink {
    pub fn new(config: &LogFileSinkConfig) -> Result<Self, String> {
        let writer = SyncLogFileWriter::new(
            PathBuf::from(&config.output_dir),
            config.max_file_size,
            config.max_backups,
        )?;
        Ok(Self {
            writer: Mutex::new(writer),
            include_diagnostics: config.include_diagnostics,
        })
    }
}

impl DataSink for LogFileSink {
    fn sink_type(&self) -> &'static str {
        "log_file_sink"
    }

    fn try_handle_event(
        &self,
        context: &EventContext,
        event: &CollectorEvent,
    ) -> Result<(), HealthError> {
        let CollectorEvent::Log(record) = event else {
            return Ok(());
        };

        // Diagnostics are opt-in for log files. When enabled, fold the
        // collector-only diagnostic carrier into the emitted body and
        // attributes before JSONL serialization.
        let record = record.emitted_log_record(self.include_diagnostics);
        let record = record.without_decoded_protobuf_payload();
        let json_record = JsonLogRecord::from_log_record(context, record.as_ref());

        let line = match serde_json::to_string(&json_record) {
            Ok(json) => json,
            Err(e) => {
                tracing::error!(error = ?e, "failed to serialize log record");
                return Err(e.into());
            }
        };

        let Ok(mut writer) = self.writer.lock() else {
            tracing::error!("log file writer lock poisoned");
            return Err(HealthError::GenericError(
                "log file writer lock poisoned".to_string(),
            ));
        };

        if let Err(e) = writer.write_line(&line) {
            tracing::error!(error = ?e, "failed to write log record to file");
            return Err(HealthError::GenericError(e));
        }

        Ok(())
    }
}

/// JSONL representation of a log event written by the file sink.
#[derive(Serialize)]
struct JsonLogRecord<'a> {
    endpoint: &'a str,
    collector: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    machine_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rack_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    machine_serial: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    driver_version: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    component_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nvlink_domain_uuid: Option<String>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    labels: &'a std::collections::BTreeMap<String, String>,
    severity: &'a str,
    body: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attributes: Vec<(&'a str, &'a str)>,
}

impl<'a> JsonLogRecord<'a> {
    /// Builds the JSONL representation for one emitted log record.
    fn from_log_record(context: &'a EventContext, record: &'a LogRecord) -> Self {
        Self {
            endpoint: context.endpoint_key(),
            collector: context.collector_type,
            machine_id: context.machine_id().map(|id| id.to_string()),
            rack_id: context.rack_id().map(|rack_id| rack_id.as_str()),
            system_uuid: context.system_uuid().map(|id| id.to_string()),
            machine_serial: context.machine_serial(),
            driver_version: context.driver_version(),
            component_type: context.component_type(),
            nvlink_domain_uuid: context.nvlink_domain_uuid().map(|id| id.to_string()),
            labels: context.labels(),
            severity: record.severity.as_str(),
            body: &record.body,
            attributes: record
                .attributes
                .iter()
                .map(|(k, v)| (k.as_ref(), v.as_str()))
                .collect(),
        }
    }
}

struct SyncLogFileWriter {
    output_dir: PathBuf,
    max_file_size: u64,
    max_backups: usize,
    current_file: Option<BufWriter<File>>,
    current_size: u64,
}

impl SyncLogFileWriter {
    fn new(output_dir: PathBuf, max_file_size: u64, max_backups: usize) -> Result<Self, String> {
        fs::create_dir_all(&output_dir)
            .map_err(|e| format!("failed to create log output directory: {e}"))?;

        let mut writer = Self {
            output_dir,
            max_file_size,
            max_backups,
            current_file: None,
            current_size: 0,
        };

        writer.open_current_file()?;
        Ok(writer)
    }

    fn log_path(&self) -> PathBuf {
        self.output_dir.join("health_logs.jsonl")
    }

    fn rotated_path(&self, index: usize) -> PathBuf {
        self.output_dir.join(format!("health_logs.{index}.jsonl"))
    }

    fn open_current_file(&mut self) -> Result<(), String> {
        let path = self.log_path();

        self.current_size = fs::metadata(&path).ok().map(|m| m.len()).unwrap_or(0);

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("failed to open log file {}: {e}", path.display()))?;

        self.current_file = Some(BufWriter::new(file));
        Ok(())
    }

    fn write_line(&mut self, line: &str) -> Result<(), String> {
        let bytes = line.as_bytes();
        let write_size = bytes.len() as u64 + 1; // +1 for trailing newline

        // a single line larger than max_file_size is allowed into a fresh file
        self.rotate_if_needed(write_size)?;

        let file = self.current_file.as_mut().ok_or("log file not open")?;

        file.write_all(bytes)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.flush())
            .map_err(|e| format!("failed to write to log file: {e}"))?;

        self.current_size += write_size;
        Ok(())
    }

    fn rotate_if_needed(&mut self, pending_size: u64) -> Result<(), String> {
        if self.current_size + pending_size <= self.max_file_size {
            return Ok(());
        }

        tracing::info!(file_size_bytes = self.current_size, "rotating log file");

        // Flush and drop the current handle before renaming. If the flush fails
        // we would rename a file still missing its buffered bytes, so keep the
        // writer and defer rotation to the next write instead of losing them.
        if let Some(mut file) = self.current_file.take()
            && let Err(e) = file.flush()
        {
            tracing::warn!(error = %e, "failed to flush log file; deferring rotation");
            self.current_file = Some(file);
            return Ok(());
        }

        let current_path = self.log_path();

        if self.max_backups == 0 {
            if let Err(e) = fs::remove_file(&current_path) {
                tracing::warn!(error = %e, path = %current_path.display(), "failed to remove current log file during rotation");
            }
        } else {
            self.shift_backups(&current_path);
        }

        self.current_size = 0;
        self.open_current_file()
    }

    fn shift_backups(&self, current_path: &Path) {
        // shift existing backups up by one index
        for i in (1..self.max_backups).rev() {
            let from = self.rotated_path(i);
            let to = self.rotated_path(i + 1);
            match fs::rename(&from, &to) {
                Ok(()) => {}
                // No backup at this index yet -- nothing to shift.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(error = %e, from = %from.display(), to = %to.display(), "failed to shift rotated log file");
                }
            }
        }

        // current -> .1
        let backup = self.rotated_path(1);
        if let Err(e) = fs::rename(current_path, &backup) {
            tracing::warn!(error = %e, from = %current_path.display(), to = %backup.display(), "failed to rotate current log file to backup");
        }

        // prune the oldest backup beyond the limit
        let oldest = self.rotated_path(self.max_backups + 1);
        match fs::remove_file(&oldest) {
            Ok(()) => {}
            // Nothing beyond the limit to prune.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(error = %e, path = %oldest.display(), "failed to prune oldest rotated log file");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::str::FromStr;

    use carbide_uuid::nvlink::NvLinkDomainId;
    use carbide_uuid::rack::RackId;
    use mac_address::MacAddress;

    use super::*;
    use crate::endpoint::{BmcAddr, EndpointMetadata, MachineData, SwitchData, SwitchEndpointRole};
    use crate::sink::{DiagnosticLogRecord, LogSeverity};

    /// Builds a base log context without endpoint metadata.
    fn test_context() -> EventContext {
        EventContext {
            endpoint_key: "aa:bb:cc:dd:ee:ff".to_string(),
            addr: BmcAddr {
                ip: "10.0.0.1".parse().expect("valid ip"),
                port: Some(443),
                mac: Some(MacAddress::from_str("aa:bb:cc:dd:ee:ff").unwrap()),
            },
            collector_type: "test",
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        }
    }

    /// Builds a log context with representative machine metadata.
    fn machine_context() -> EventContext {
        EventContext {
            rack_id: Some(RackId::new("RACK_1")),
            labels: std::collections::BTreeMap::from([(
                "site".to_string(),
                "rno-dev7".to_string(),
            )]),
            metadata: Some(EndpointMetadata::Machine(MachineData {
                machine_id: Some(
                    "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0"
                        .parse()
                        .expect("valid machine id"),
                ),
                machine_serial: Some("MN-001".to_string()),
                system_uuid: Some(uuid::uuid!("4c4c4544-0044-4710-8052-cac04f4b4632")).into(),
                slot_number: None,
                tray_index: None,
                nvlink_domain_uuid: Some(NvLinkDomainId::nil()),
                driver_version: Some("570.82".to_string()),
            })),
            ..test_context()
        }
    }

    #[test]
    fn test_ignores_non_log_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = LogFileSinkConfig {
            include_diagnostics: false,
            output_dir: dir.path().to_string_lossy().into_owned(),
            max_file_size: 1024,
            max_backups: 2,
        };
        let sink = LogFileSink::new(&config).expect("sink");
        let ctx = test_context();

        let metric_event = CollectorEvent::MetricCollectionStart;
        sink.handle_event(&ctx, &metric_event);

        let log_path = dir.path().join("health_logs.jsonl");
        let contents = fs::read_to_string(&log_path).unwrap_or_default();
        assert!(contents.is_empty(), "non-log events should be ignored");
    }

    #[test]
    fn test_writes_log_events_as_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = LogFileSinkConfig {
            include_diagnostics: false,
            output_dir: dir.path().to_string_lossy().into_owned(),
            max_file_size: 1024 * 1024,
            max_backups: 2,
        };
        let sink = LogFileSink::new(&config).expect("sink");
        let ctx = test_context();

        let event = CollectorEvent::Log(
            LogRecord {
                body: "something happened".to_string(),
                severity: LogSeverity::Info,
                attributes: vec![(Cow::Borrowed("entry_id"), "42".to_string())],
                diagnostic_record: None,
            }
            .into(),
        );
        sink.handle_event(&ctx, &event);

        let log_path = dir.path().join("health_logs.jsonl");
        let contents = fs::read_to_string(log_path).expect("read log");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);

        let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("valid json");
        assert_eq!(parsed["body"], "something happened");
        assert_eq!(parsed["severity"], "INFO");
        assert_eq!(parsed["endpoint"], "aa:bb:cc:dd:ee:ff");
        assert!(parsed.get("rack_id").is_none());
    }

    #[test]
    fn test_excludes_decoded_protobuf_from_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir");

        let config = LogFileSinkConfig {
            include_diagnostics: true,
            output_dir: dir.path().to_string_lossy().into_owned(),
            max_file_size: 1024 * 1024,
            max_backups: 2,
        };

        let sink = LogFileSink::new(&config).expect("sink");

        let event = CollectorEvent::Log(
            LogRecord {
                body: "protobuf event".to_string(),
                severity: LogSeverity::Info,
                attributes: vec![(
                    Cow::Borrowed(LogRecord::DECODED_PROTOBUF_PAYLOAD_ATTRIBUTE),
                    "decoded-payload-marker".to_string(),
                )],
                diagnostic_record: None,
            }
            .into(),
        );

        sink.handle_event(&test_context(), &event);

        let contents =
            fs::read_to_string(dir.path().join("health_logs.jsonl")).expect("read JSONL output");

        assert!(!contents.contains("decoded-payload-marker"));
        assert!(!contents.contains(LogRecord::DECODED_PROTOBUF_PAYLOAD_ATTRIBUTE));
    }

    /// Verifies log files embed diagnostics in the parent log body when enabled.
    #[test]
    fn test_writes_diagnostic_fields_in_parent_log_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = LogFileSinkConfig {
            include_diagnostics: true,
            output_dir: dir.path().to_string_lossy().into_owned(),
            max_file_size: 1024 * 1024,
            max_backups: 2,
        };
        let sink = LogFileSink::new(&config).expect("sink");
        let ctx = test_context();

        let event = CollectorEvent::Log(
            LogRecord {
                body: "parent log".to_string(),
                severity: LogSeverity::Info,
                attributes: vec![(Cow::Borrowed("entry_id"), "42".to_string())],
                diagnostic_record: Some(DiagnosticLogRecord {
                    body: "opaque-cper".to_string(),
                    attributes: vec![(
                        Cow::Borrowed("redfish.diagnostic_data.type"),
                        "CPER".to_string(),
                    )],
                }),
            }
            .into(),
        );
        sink.handle_event(&ctx, &event);

        let log_path = dir.path().join("health_logs.jsonl");
        let contents = fs::read_to_string(log_path).expect("read log");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);

        let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("valid json");
        let body = parsed["body"].as_str().expect("body string");
        let body: serde_json::Value = serde_json::from_str(body).expect("valid body json");

        assert_eq!(body["message"], "parent log");
        assert_eq!(body["diagnostic_data"], "opaque-cper");
        assert_eq!(
            body["diagnostic_attributes"][0]["key"],
            "redfish.diagnostic_data.type"
        );
        assert_eq!(body["diagnostic_attributes"][0]["value"], "CPER");
        assert_eq!(parsed["attributes"][1][0], "redfish.diagnostic_data.type");
        assert_eq!(parsed["attributes"][1][1], "CPER");
    }

    /// Verifies log files omit diagnostic payloads by default.
    #[test]
    fn test_skips_diagnostic_log_record_by_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = LogFileSinkConfig {
            include_diagnostics: false,
            output_dir: dir.path().to_string_lossy().into_owned(),
            max_file_size: 1024 * 1024,
            max_backups: 2,
        };
        let sink = LogFileSink::new(&config).expect("sink");
        let ctx = test_context();

        let event = CollectorEvent::Log(
            LogRecord {
                body: "parent log".to_string(),
                severity: LogSeverity::Info,
                attributes: Vec::new(),
                diagnostic_record: Some(DiagnosticLogRecord {
                    body: "opaque-cper".to_string(),
                    attributes: Vec::new(),
                }),
            }
            .into(),
        );
        sink.handle_event(&ctx, &event);

        let log_path = dir.path().join("health_logs.jsonl");
        let contents = fs::read_to_string(log_path).expect("read log");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);

        let parent: serde_json::Value = serde_json::from_str(lines[0]).expect("valid parent json");
        assert_eq!(parent["body"], "parent log");
    }

    /// Verifies that machine metadata is emitted as top-level JSONL fields.
    #[test]
    fn test_writes_machine_and_rack_metadata_as_jsonl_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = LogFileSinkConfig {
            include_diagnostics: false,
            output_dir: dir.path().to_string_lossy().into_owned(),
            max_file_size: 1024 * 1024,
            max_backups: 2,
        };
        let sink = LogFileSink::new(&config).expect("sink");
        let ctx = machine_context();

        let event = CollectorEvent::Log(
            LogRecord {
                body: "xid event".to_string(),
                severity: LogSeverity::Warn,
                attributes: Vec::new(),
                diagnostic_record: None,
            }
            .into(),
        );
        sink.handle_event(&ctx, &event);

        let log_path = dir.path().join("health_logs.jsonl");
        let contents = fs::read_to_string(log_path).expect("read log");
        let line = contents.lines().next().expect("one JSONL record");

        let parsed: serde_json::Value = serde_json::from_str(line).expect("valid json");
        assert_eq!(
            parsed["machine_id"],
            "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0"
        );
        assert_eq!(
            parsed["system_uuid"],
            "4c4c4544-0044-4710-8052-cac04f4b4632"
        );
        assert_eq!(parsed["machine_serial"], "MN-001");
        assert_eq!(parsed["driver_version"], "570.82");
        assert_eq!(parsed["component_type"], "compute_node");
        assert_eq!(parsed["rack_id"], "RACK_1");
        assert_eq!(parsed["labels"]["site"], "rno-dev7");
        assert_eq!(
            parsed["nvlink_domain_uuid"],
            "00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn test_writes_switch_nvlink_domain_uuid_as_jsonl_field() {
        let dir = tempfile::tempdir().expect("tempdir");

        let config = LogFileSinkConfig {
            include_diagnostics: false,
            output_dir: dir.path().to_string_lossy().into_owned(),
            max_file_size: 1024 * 1024,
            max_backups: 2,
        };

        let sink = LogFileSink::new(&config).expect("sink");

        let nvlink_domain_uuid = NvLinkDomainId::from_str("9f4b45ec-705a-4af4-89f7-a112bc9c8f4e")
            .expect("valid NVLink domain UUID");

        let mut ctx = test_context();

        ctx.metadata = Some(EndpointMetadata::Switch(SwitchData {
            id: None,
            serial: "SN-SWITCH-001".to_string(),
            slot_number: Some(7),
            tray_index: Some(3),
            nvlink_domain_uuid: Some(nvlink_domain_uuid),
            endpoint_role: SwitchEndpointRole::Host,
            is_primary: true,
            nmxc_enabled: true,
            nmxt_enabled: true,
        }));

        let event = CollectorEvent::Log(
            LogRecord {
                body: "switch event".to_string(),
                severity: LogSeverity::Warn,
                attributes: Vec::new(),
                diagnostic_record: None,
            }
            .into(),
        );

        sink.handle_event(&ctx, &event);

        let log_path = dir.path().join("health_logs.jsonl");
        let contents = fs::read_to_string(log_path).expect("read log");
        let line = contents.lines().next().expect("one JSONL record");
        let parsed: serde_json::Value = serde_json::from_str(line).expect("valid json");

        assert_eq!(
            parsed["nvlink_domain_uuid"],
            "9f4b45ec-705a-4af4-89f7-a112bc9c8f4e"
        );

        assert_eq!(parsed["component_type"], "nvlink_switch");
    }

    #[test]
    fn test_rotation_creates_backups() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = LogFileSinkConfig {
            include_diagnostics: false,
            output_dir: dir.path().to_string_lossy().into_owned(),
            // tiny limit to force rotation quickly
            max_file_size: 50,
            max_backups: 2,
        };
        let sink = LogFileSink::new(&config).expect("sink");
        let ctx = test_context();

        for i in 0..5 {
            let event = CollectorEvent::Log(
                LogRecord {
                    body: format!("log entry {i}"),
                    severity: LogSeverity::Info,
                    attributes: Vec::new(),
                    diagnostic_record: None,
                }
                .into(),
            );
            sink.handle_event(&ctx, &event);
        }

        let current = dir.path().join("health_logs.jsonl");
        let backup1 = dir.path().join("health_logs.1.jsonl");

        assert!(current.exists(), "current log file should exist");
        assert!(backup1.exists(), "at least one backup should exist");
    }

    #[test]
    fn test_rotation_zero_backups_truncates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = LogFileSinkConfig {
            include_diagnostics: false,
            output_dir: dir.path().to_string_lossy().into_owned(),
            max_file_size: 50,
            max_backups: 0,
        };
        let sink = LogFileSink::new(&config).expect("sink");
        let ctx = test_context();

        for i in 0..5 {
            let event = CollectorEvent::Log(
                LogRecord {
                    body: format!("entry {i}"),
                    severity: LogSeverity::Warn,
                    attributes: Vec::new(),
                    diagnostic_record: None,
                }
                .into(),
            );
            sink.handle_event(&ctx, &event);
        }

        let current = dir.path().join("health_logs.jsonl");
        assert!(current.exists());

        // no backup files should exist
        let backup1 = dir.path().join("health_logs.1.jsonl");
        assert!(!backup1.exists(), "no backups when max_backups = 0");
    }
}
