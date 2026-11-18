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

//! The config file `/etc/scale-test/config.yaml`.
//!
//! Every rule is checked here, so a hand-made Job cannot skip one.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use url::Url;

/// Longest run ID. Object names add suffixes like `-scale-test`.
pub const RUN_ID_MAX_LEN: usize = 40;
/// Bounds of `resultMaxBytes`, the size of the result the run writes.
pub const RESULT_MAX_BYTES_MIN: u32 = 262_144;
pub const RESULT_MAX_BYTES_MAX: u32 = 1_000_000;
/// Longest Kubernetes object name: the pod name and the journal claim.
pub const MAX_NAME_BYTES: usize = 253;
pub const MAX_JOURNAL_DIR_BYTES: usize = 512;
/// Upper bounds, so no later duration math can overflow.
pub const MAX_PROBE_TIMEOUT_SECONDS: u64 = 600;
pub const MAX_PROBE_POLL_SECONDS: u64 = 3_600;
pub const MAX_PROBE_CONTACT_SECONDS: u64 = 86_400;
pub const MAX_WHOLE_RUN_SECONDS: u64 = 7 * 86_400;

pub const RUN_ID_PATTERN: &str = "^[a-z0-9]([-a-z0-9]{0,38}[a-z0-9])?$";

/// A run ID matching [`RUN_ID_PATTERN`]. It names the Job, its ConfigMaps, the
/// journal and every batch the run sends.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RunId(String);

impl JsonSchema for RunId {
    fn schema_name() -> Cow<'static, str> {
        "RunId".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": RUN_ID_MAX_LEN,
            "pattern": RUN_ID_PATTERN,
        })
    }
}

impl RunId {
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        let invalid = |reason: &str| ConfigError::Invalid {
            field: "runId".to_string(),
            reason: format!("{raw:?} {reason}"),
        };
        if raw.is_empty() || raw.len() > RUN_ID_MAX_LEN {
            return Err(invalid("must have 1 to 40 characters"));
        }
        if !raw
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(invalid("may hold only a-z, 0-9 and '-'"));
        }
        if raw.starts_with('-') || raw.ends_with('-') {
            return Err(invalid("must start and end with a letter or digit"));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RunId {
    type Error = ConfigError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl From<RunId> for String {
    fn from(id: RunId) -> Self {
        id.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("read config {path}: {message}")]
    Read { path: PathBuf, message: String },
    #[error("parse config: {0}")]
    Parse(String),
    #[error("config {field}: {reason}")]
    Invalid { field: String, reason: String },
}

fn invalid(field: &str, reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        field: field.to_string(),
        reason: reason.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommonConfig {
    pub run_id: RunId,
    /// Must equal the CLI command.
    pub command: String,
    /// No default. Required when the command writes, must be missing when not.
    #[serde(default)]
    pub dry_run: Option<bool>,
    pub namespace: String,
    /// The pod name. The chart passes it as `SCALE_TEST_EXECUTION_ID`, never in
    /// the file, so main fills it in.
    #[serde(default)]
    pub execution_id: String,
    #[serde(default)]
    pub site: SiteConfig,
    #[serde(default)]
    pub probe: ProbeConfig,
    pub timeouts: Timeouts,
    /// Pods whose restarts are recorded as evidence.
    #[serde(default)]
    pub control_plane: Vec<PodSelector>,
    #[serde(default)]
    pub journal: JournalConfig,
    /// Empty names default to `<runId>-run`, `<runId>-control`, `<runId>-result`.
    #[serde(default)]
    pub config_maps: ConfigMapNames,
    #[serde(default = "default_result_max_bytes")]
    pub result_max_bytes: u32,
    /// Per command, parsed into its own struct.
    #[serde(default)]
    pub settings: serde_yaml::Value,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SiteConfig {
    pub lock_name: String,
}

impl Default for SiteConfig {
    fn default() -> Self {
        Self {
            lock_name: "nico-scale-test-site-lock".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct ProbeConfig {
    pub url: String,
    pub request_timeout_seconds: u64,
    pub poll_seconds: u64,
    /// A reply gap over this is recorded.
    pub contact_seconds: u64,
    /// A reply gap over this makes the stage unresolved.
    pub max_contact_gap_seconds: u64,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            url: "http://nico-site-health-probe-api:9010".to_string(),
            request_timeout_seconds: 5,
            poll_seconds: 15,
            contact_seconds: 60,
            max_contact_gap_seconds: 300,
        }
    }
}

impl ProbeConfig {
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_seconds)
    }

    pub fn poll(&self) -> Duration {
        Duration::from_secs(self.poll_seconds)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Timeouts {
    pub whole_run_seconds: u64,
    #[serde(default = "default_readiness_seconds")]
    pub readiness_seconds: u64,
    #[serde(default = "default_finalize_seconds")]
    pub finalize_seconds: u64,
}

impl Timeouts {
    pub fn whole_run(&self) -> Duration {
        Duration::from_secs(self.whole_run_seconds)
    }

    pub fn readiness(&self) -> Duration {
        Duration::from_secs(self.readiness_seconds)
    }

    pub fn finalize(&self) -> Duration {
        Duration::from_secs(self.finalize_seconds)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PodSelector {
    pub namespace: String,
    pub label_selector: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JournalConfig {
    pub dir: PathBuf,
    /// The PVC mounted at `dir`, named in the result's `evidence`.
    #[serde(default)]
    pub claim: String,
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("/journal"),
            claim: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct ConfigMapNames {
    pub run: String,
    pub control: String,
    pub result: String,
}

fn default_result_max_bytes() -> u32 {
    900_000
}

fn default_readiness_seconds() -> u64 {
    300
}

fn default_finalize_seconds() -> u64 {
    900
}

/// Reads, parses and checks the config file. `command` is the CLI command.
pub fn load(path: &Path, command: &str) -> Result<CommonConfig, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Read {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    parse(&raw, command)
}

/// [`load`] without the file read.
pub fn parse(raw: &str, command: &str) -> Result<CommonConfig, ConfigError> {
    let mut config: CommonConfig =
        serde_yaml::from_str(raw).map_err(|e| ConfigError::Parse(e.to_string()))?;
    config.fill_defaults();
    config.validate(command)?;
    Ok(config)
}

impl CommonConfig {
    fn fill_defaults(&mut self) {
        let run = self.run_id.as_str();
        for (name, suffix) in [
            (&mut self.config_maps.run, "run"),
            (&mut self.config_maps.control, "control"),
            (&mut self.config_maps.result, "result"),
        ] {
            if name.is_empty() {
                *name = format!("{run}-{suffix}");
            }
        }
        if self.settings.is_null() {
            self.settings = serde_yaml::Value::Mapping(Default::default());
        }
    }

    /// The rules that need no knowledge of the command's writes.
    pub fn validate(&self, command: &str) -> Result<(), ConfigError> {
        if self.command != command {
            return Err(invalid(
                "command",
                format!(
                    "{:?} does not match the CLI command {command:?}",
                    self.command
                ),
            ));
        }
        if !is_dns_label(&self.namespace) {
            return Err(invalid("namespace", "must be a DNS label"));
        }
        if !is_dns_subdomain(&self.site.lock_name) {
            return Err(invalid("site.lockName", "must be a DNS subdomain name"));
        }
        // Checked after the defaults, so a hand-set name gets the same rules.
        let names = &self.config_maps;
        let named = [
            ("configMaps.run", &names.run),
            ("configMaps.control", &names.control),
            ("configMaps.result", &names.result),
        ];
        for (field, name) in named {
            if !is_dns_subdomain(name) {
                return Err(invalid(field, "must be a DNS subdomain name"));
            }
        }
        if names.run == names.control || names.run == names.result || names.control == names.result
        {
            return Err(invalid(
                "configMaps",
                "run, control and result must be three different names",
            ));
        }

        let probe = &self.probe;
        match Url::parse(&probe.url) {
            Ok(url) if matches!(url.scheme(), "http" | "https") && url.host().is_some() => {
                // Errors and logs must never carry a secret from the URL.
                if !url.username().is_empty() || url.password().is_some() {
                    return Err(invalid(
                        "probe.url",
                        "must not hold a user name or password",
                    ));
                }
                if url.query().is_some() || url.fragment().is_some() {
                    return Err(invalid("probe.url", "must not hold a query or fragment"));
                }
                // The client puts `/v1/...` at the root, so a path would be
                // dropped, not kept.
                if url.path() != "/" {
                    return Err(invalid("probe.url", "must not hold a path"));
                }
            }
            _ => return Err(invalid("probe.url", "must be an http or https URL")),
        }
        if probe.request_timeout_seconds == 0 {
            return Err(invalid("probe.requestTimeoutSeconds", "must be over 0"));
        }
        if probe.request_timeout_seconds > MAX_PROBE_TIMEOUT_SECONDS {
            return Err(invalid(
                "probe.requestTimeoutSeconds",
                format!("must be at most {MAX_PROBE_TIMEOUT_SECONDS}"),
            ));
        }
        if probe.poll_seconds == 0 {
            return Err(invalid("probe.pollSeconds", "must be over 0"));
        }
        if probe.poll_seconds > MAX_PROBE_POLL_SECONDS {
            return Err(invalid(
                "probe.pollSeconds",
                format!("must be at most {MAX_PROBE_POLL_SECONDS}"),
            ));
        }
        if probe.contact_seconds < probe.poll_seconds {
            return Err(invalid(
                "probe.contactSeconds",
                "must be at least probe.pollSeconds",
            ));
        }
        if probe.max_contact_gap_seconds < probe.contact_seconds {
            return Err(invalid(
                "probe.maxContactGapSeconds",
                "must be at least probe.contactSeconds",
            ));
        }
        if probe.max_contact_gap_seconds > MAX_PROBE_CONTACT_SECONDS {
            return Err(invalid(
                "probe.maxContactGapSeconds",
                format!("must be at most {MAX_PROBE_CONTACT_SECONDS}"),
            ));
        }

        let t = &self.timeouts;
        if t.readiness_seconds == 0 {
            return Err(invalid("timeouts.readinessSeconds", "must be over 0"));
        }
        if t.readiness_seconds >= t.finalize_seconds || t.finalize_seconds >= t.whole_run_seconds {
            return Err(invalid(
                "timeouts",
                "need readinessSeconds < finalizeSeconds < wholeRunSeconds",
            ));
        }
        if t.whole_run_seconds > MAX_WHOLE_RUN_SECONDS {
            return Err(invalid(
                "timeouts.wholeRunSeconds",
                format!("must be at most {MAX_WHOLE_RUN_SECONDS}"),
            ));
        }

        for (i, selector) in self.control_plane.iter().enumerate() {
            if !is_dns_label(&selector.namespace) {
                return Err(invalid(
                    &format!("controlPlane[{i}].namespace"),
                    "must be a DNS label",
                ));
            }
            if selector.label_selector.trim().is_empty() {
                return Err(invalid(
                    &format!("controlPlane[{i}].labelSelector"),
                    "must not be empty",
                ));
            }
        }
        if !self.journal.dir.is_absolute() {
            return Err(invalid("journal.dir", "must be an absolute path"));
        }
        // Both end up in every result, also the bounded terminal one.
        if self.journal.dir.as_os_str().len() > MAX_JOURNAL_DIR_BYTES {
            return Err(invalid(
                "journal.dir",
                format!("must be at most {MAX_JOURNAL_DIR_BYTES} bytes"),
            ));
        }
        if self.journal.claim.len() > MAX_NAME_BYTES {
            return Err(invalid(
                "journal.claim",
                format!("must be at most {MAX_NAME_BYTES} bytes"),
            ));
        }
        if !(RESULT_MAX_BYTES_MIN..=RESULT_MAX_BYTES_MAX).contains(&self.result_max_bytes) {
            return Err(invalid(
                "resultMaxBytes",
                format!("must be {RESULT_MAX_BYTES_MIN} to {RESULT_MAX_BYTES_MAX}"),
            ));
        }
        if !self.settings.is_mapping() {
            return Err(invalid("settings", "must be a mapping"));
        }
        Ok(())
    }

    /// `dryRun` is required for a command that writes and must be missing for
    /// one that does not.
    pub fn check_dry_run(&self, writes: bool) -> Result<(), ConfigError> {
        match (writes, self.dry_run) {
            (true, None) => Err(invalid(
                "dryRun",
                "is required for a command that writes (true or false)",
            )),
            (false, Some(_)) => Err(invalid(
                "dryRun",
                "must be missing for a command that makes no writes",
            )),
            _ => Ok(()),
        }
    }

    /// The pod name must be known before the run takes the lock.
    pub fn check_execution_id(&self) -> Result<(), ConfigError> {
        if self.execution_id.is_empty() {
            return Err(invalid(
                "executionId",
                "is empty (set SCALE_TEST_EXECUTION_ID to the pod name)",
            ));
        }
        if self.execution_id.len() > MAX_NAME_BYTES {
            return Err(invalid(
                "executionId",
                format!("must be at most {MAX_NAME_BYTES} bytes, like a pod name"),
            ));
        }
        Ok(())
    }
}

fn is_dns_label(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

fn is_dns_subdomain(name: &str) -> bool {
    !name.is_empty() && name.len() <= 253 && name.split('.').all(is_dns_label)
}

#[cfg(test)]
pub(crate) mod tests {
    use carbide_test_support::{Case, Outcome, check_cases};

    use super::*;

    /// A valid config with every required key.
    pub(crate) const MINIMAL: &str = "
runId: r1-burst
command: burst
dryRun: false
namespace: nico-mat
timeouts:
  wholeRunSeconds: 3600
settings:
  bursts: 1
";

    fn with(extra: &str) -> String {
        format!("{MINIMAL}{extra}")
    }

    fn replace(from: &str, to: &str) -> String {
        assert!(MINIMAL.contains(from), "{from} is not in MINIMAL");
        MINIMAL.replace(from, to)
    }

    #[test]
    fn minimal_config_fills_defaults() {
        let config = parse(MINIMAL, "burst").unwrap();
        assert_eq!(config.run_id.as_str(), "r1-burst");
        assert_eq!(config.site.lock_name, "nico-scale-test-site-lock");
        assert_eq!(config.probe, ProbeConfig::default());
        assert_eq!(config.timeouts.readiness_seconds, 300);
        assert_eq!(config.timeouts.finalize_seconds, 900);
        assert_eq!(config.config_maps.run, "r1-burst-run");
        assert_eq!(config.config_maps.control, "r1-burst-control");
        assert_eq!(config.config_maps.result, "r1-burst-result");
        assert_eq!(config.result_max_bytes, 900_000);
        assert_eq!(config.journal.dir, PathBuf::from("/journal"));
        assert!(config.execution_id.is_empty());
    }

    fn fails<I, T>(scenario: &'static str, input: I) -> Case<I, T, ConfigError> {
        Case {
            scenario,
            input,
            expect: Outcome::Fails,
        }
    }

    fn yields<I, T>(scenario: &'static str, input: I, value: T) -> Case<I, T, ConfigError> {
        Case {
            scenario,
            input,
            expect: Outcome::Yields(value),
        }
    }

    /// Fails with `ConfigError::Invalid` on `field`.
    fn invalid<I, T>(
        scenario: &'static str,
        input: I,
        field: &str,
        reason: &str,
    ) -> Case<I, T, ConfigError> {
        let (field, reason) = (field.to_string(), reason.to_string());
        Case {
            scenario,
            input,
            expect: Outcome::FailsWith(ConfigError::Invalid { field, reason }),
        }
    }

    #[test]
    fn config_rejects_bad_values() {
        let subdomain = "must be a DNS subdomain name";
        let distinct = "run, control and result must be three different names";
        let ordering = "need readinessSeconds < finalizeSeconds < wholeRunSeconds";
        let cases = vec![
            fails("unknown top-level key", with("surprise: 1\n")),
            fails("unknown probe key", with("probe:\n  urll: http://x:9010\n")),
            fails(
                "missing timeouts",
                replace("timeouts:\n  wholeRunSeconds: 3600\n", ""),
            ),
            invalid(
                "command does not match the CLI",
                replace("command: burst", "command: ingestion"),
                "command",
                "\"ingestion\" does not match the CLI command \"burst\"",
            ),
            invalid(
                "finalize not below whole run",
                replace("wholeRunSeconds: 3600", "wholeRunSeconds: 900"),
                "timeouts",
                ordering,
            ),
            invalid(
                "readiness not below finalize",
                with_timeouts("readinessSeconds: 900"),
                "timeouts",
                ordering,
            ),
            invalid(
                "whole run over seven days",
                replace("wholeRunSeconds: 3600", "wholeRunSeconds: 604801"),
                "timeouts.wholeRunSeconds",
                "must be at most 604800",
            ),
            invalid(
                "result budget too small",
                with("resultMaxBytes: 1000\n"),
                "resultMaxBytes",
                "must be 262144 to 1000000",
            ),
            invalid(
                "result budget over the ConfigMap limit",
                with("resultMaxBytes: 1048576\n"),
                "resultMaxBytes",
                "must be 262144 to 1000000",
            ),
            invalid(
                "probe URL without scheme",
                with("probe:\n  url: nico-site-health-probe-api:9010\n"),
                "probe.url",
                "must be an http or https URL",
            ),
            invalid(
                "probe URL with a password",
                with("probe:\n  url: http://user:secret@probe:9010\n"),
                "probe.url",
                "must not hold a user name or password",
            ),
            invalid(
                "probe URL with a user name",
                with("probe:\n  url: http://user@probe:9010\n"),
                "probe.url",
                "must not hold a user name or password",
            ),
            invalid(
                "probe URL with a query",
                with("probe:\n  url: http://probe:9010/?token=x\n"),
                "probe.url",
                "must not hold a query or fragment",
            ),
            invalid(
                "zero request timeout",
                with("probe:\n  requestTimeoutSeconds: 0\n"),
                "probe.requestTimeoutSeconds",
                "must be over 0",
            ),
            invalid(
                "request timeout over the cap",
                with("probe:\n  requestTimeoutSeconds: 601\n"),
                "probe.requestTimeoutSeconds",
                "must be at most 600",
            ),
            invalid(
                "poll over the cap",
                with("probe:\n  pollSeconds: 3601\n  contactSeconds: 3601\n"),
                "probe.pollSeconds",
                "must be at most 3600",
            ),
            invalid(
                "contact limit below poll",
                with("probe:\n  pollSeconds: 30\n  contactSeconds: 10\n"),
                "probe.contactSeconds",
                "must be at least probe.pollSeconds",
            ),
            invalid(
                "max contact gap below contact limit",
                with("probe:\n  maxContactGapSeconds: 30\n"),
                "probe.maxContactGapSeconds",
                "must be at least probe.contactSeconds",
            ),
            invalid(
                "max contact gap over a day",
                with("probe:\n  maxContactGapSeconds: 86401\n"),
                "probe.maxContactGapSeconds",
                "must be at most 86400",
            ),
            invalid(
                "relative journal dir",
                with("journal:\n  dir: journal\n"),
                "journal.dir",
                "must be an absolute path",
            ),
            invalid(
                "lock name with capitals",
                with("site:\n  lockName: Lock\n"),
                "site.lockName",
                subdomain,
            ),
            invalid(
                "run ConfigMap name with capitals",
                with("configMaps:\n  run: Bad_Name\n"),
                "configMaps.run",
                subdomain,
            ),
            invalid(
                "control ConfigMap name ends with a dot",
                with("configMaps:\n  control: ctl.\n"),
                "configMaps.control",
                subdomain,
            ),
            invalid(
                "result ConfigMap name over 253 bytes",
                with(&format!("configMaps:\n  result: {}\n", "r".repeat(254))),
                "configMaps.result",
                subdomain,
            ),
            invalid(
                "all three ConfigMaps share one name",
                with("configMaps:\n  run: shared\n  control: shared\n  result: shared\n"),
                "configMaps",
                distinct,
            ),
            invalid(
                "result ConfigMap takes the run name",
                with("configMaps:\n  run: shared\n  result: shared\n"),
                "configMaps",
                distinct,
            ),
            invalid(
                "control ConfigMap set to the default result name",
                with("configMaps:\n  control: r1-burst-result\n"),
                "configMaps",
                distinct,
            ),
            yields(
                "three distinct hand-set ConfigMap names",
                with("configMaps:\n  run: a.run\n  control: a.control\n  result: a.result\n"),
                0,
            ),
            invalid(
                "control plane selector without labels",
                with("controlPlane:\n- namespace: nico\n  labelSelector: ''\n"),
                "controlPlane[0].labelSelector",
                "must not be empty",
            ),
            invalid(
                "settings not a mapping",
                replace("settings:\n  bursts: 1\n", "settings: [1]\n"),
                "settings",
                "must be a mapping",
            ),
            fails("result budget not a number", with("resultMaxBytes: lots\n")),
            invalid(
                "result budget at the ConfigMap margin",
                with("resultMaxBytes: 1000001\n"),
                "resultMaxBytes",
                "must be 262144 to 1000000",
            ),
            invalid(
                "probe URL with a path",
                with("probe:\n  url: https://probe.example:9010/base\n"),
                "probe.url",
                "must not hold a path",
            ),
            yields(
                "probe URL with only the root path",
                with("probe:\n  url: https://probe.example:9010/\n"),
                0,
            ),
            yields(
                "valid control plane selector",
                with(
                    "controlPlane:\n- namespace: nico\n  labelSelector: app.kubernetes.io/name=nico-api\n",
                ),
                1,
            ),
        ];
        check_cases(cases, |raw| {
            parse(&raw, "burst").map(|c| c.control_plane.len())
        });

        // serde wraps the run ID's own error in a parse error.
        match parse(&replace("runId: r1-burst", "runId: R1"), "burst") {
            Err(ConfigError::Parse(text)) => {
                assert!(
                    text.contains("\"R1\" may hold only a-z, 0-9 and '-'"),
                    "{text}"
                )
            }
            other => panic!("run ID with capitals: {other:?}"),
        }
    }

    fn with_timeouts(extra: &str) -> String {
        replace(
            "timeouts:\n  wholeRunSeconds: 3600\n",
            &format!("timeouts:\n  wholeRunSeconds: 3600\n  {extra}\n"),
        )
    }

    #[test]
    fn dry_run_required_only_for_writing_commands() {
        // Scenario, dryRun, whether the command writes, and whether the check passes.
        let rows = [
            ("writing command, dryRun false", Some(false), true, true),
            ("writing command, dryRun true", Some(true), true, true),
            ("writing command, dryRun missing", None, true, false),
            ("read-only command, dryRun missing", None, false, true),
            ("read-only command, dryRun false", Some(false), false, false),
            ("read-only command, dryRun true", Some(true), false, false),
        ];
        let base = parse(MINIMAL, "burst").unwrap();
        for (scenario, dry_run, writes, ok) in rows {
            let config = CommonConfig {
                dry_run,
                ..base.clone()
            };
            assert_eq!(config.check_dry_run(writes).is_ok(), ok, "{scenario}");
        }
        // A missing key parses as None, so the rule above sees it.
        let missing = parse(&replace("dryRun: false\n", ""), "burst").unwrap();
        assert_eq!(missing.dry_run, None);
    }

    #[test]
    fn run_id_rules() {
        let ok = |scenario, id: &'static str| yields(scenario, id, id.to_string());
        let cases = vec![
            ok("one character", "a"),
            ok("digits and dashes", "r1-burst-2"),
            ok("40 characters", "a234567890123456789012345678901234567890"),
            fails("41 characters", "a2345678901234567890123456789012345678901"),
            fails("empty", ""),
            fails("capital letter", "R1"),
            fails("leading dash", "-r1"),
            fails("trailing dash", "r1-"),
            fails("underscore", "r1_x"),
            fails("dot", "r1.x"),
        ];
        // Every accepted ID also matches the pattern the schema and chart use.
        let chart = regex::Regex::new(RUN_ID_PATTERN).unwrap();
        check_cases(cases, |raw| {
            let id = RunId::parse(raw)?;
            assert!(chart.is_match(id.as_str()), "{id}");
            Ok(id.to_string())
        });
    }

    #[test]
    fn execution_id_must_be_set() {
        let mut config = parse(MINIMAL, "burst").unwrap();
        assert!(config.check_execution_id().is_err());
        config.execution_id = "r1-burst-abcde".to_string();
        assert!(config.check_execution_id().is_ok());
        config.execution_id = "x".repeat(254);
        assert!(
            config.check_execution_id().is_err(),
            "longer than a pod name"
        );
        let long_dir = format!("journal:\n  dir: /{}\n", "d".repeat(512));
        assert!(parse(&with(&long_dir), "burst").is_err());
        let long_claim = format!("journal:\n  dir: /journal\n  claim: {}\n", "c".repeat(254));
        assert!(parse(&with(&long_claim), "burst").is_err());
    }
}
