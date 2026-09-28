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

use carbide_utils::config::as_std_duration;
use duration_str::deserialize_duration;
use serde::{Deserialize, Serialize};

/// Controls which machine validation tests are active.
#[derive(Default, Clone, Copy, Debug, Deserialize, Serialize)]
pub enum MachineValidationTestSelectionMode {
    /// Only update tests in DB that are specified in the
    /// `tests` config list.
    #[default]
    Default,
    /// Enable all tests in DB, but allow per-test overrides
    /// from the `tests` config list.
    EnableAll,
    /// Disable all tests in DB, but allow per-test overrides
    /// from the `tests` config list.
    DisableAll,
}

/// Configuration for machine validation tests (memory
/// latency, SSD I/O, etc.) run after ingestion to verify
/// hardware health.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MachineValidationConfig {
    /// Enables machine validation testing.
    #[serde(default)]
    pub enabled: bool,

    /// Controls whether to run all tests, no tests, or use
    /// per-test configuration.
    #[serde(default)]
    pub test_selection_mode: MachineValidationTestSelectionMode,

    #[serde(
        default = "MachineValidationConfig::default_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub run_interval: std::time::Duration,

    /// Grace period before an active validation run is considered stale after
    /// its expected duration has elapsed.
    #[serde(
        default = "MachineValidationConfig::default_stale_run_timeout",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub stale_run_timeout: std::time::Duration,

    /// Per-test enable/disable overrides.
    #[serde(default)]
    pub tests: Vec<MachineValidationTestConfig>,

    /// OCI registries from which Machine Validation plugins may be configured.
    /// An empty list denies all plugin registrations; legacy tests are unaffected.
    #[serde(default)]
    pub approved_plugin_registries: Vec<String>,
    /// Plugin execution types permitted for this site. An empty list denies all
    /// plugin registrations; legacy tests are unaffected.
    #[serde(default = "MachineValidationConfig::default_allowed_plugin_types")]
    pub allowed_plugin_types: Vec<String>,
    /// Allows plugin registration with the privileged container profile.
    #[serde(default)]
    pub allow_privileged_plugins: bool,
    /// Allows registration of plugins that request a writable host-root mount.
    #[serde(default)]
    pub allow_full_host_plugins: bool,
    /// Site-wide storage policy for Machine Validation attempt logs.
    #[serde(default)]
    pub attempt_logs: MachineValidationAttemptLogConfig,
}

/// Limits persisted stdout and stderr for Machine Validation attempts.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MachineValidationAttemptLogConfig {
    /// Whether NICo persists attempt log chunks. Disabled chunks are discarded.
    #[serde(default = "MachineValidationAttemptLogConfig::default_enabled")]
    pub enabled: bool,
    /// Maximum UTF-8 byte size of one persisted chunk.
    #[serde(default = "MachineValidationAttemptLogConfig::default_max_chunk_bytes")]
    pub max_chunk_bytes: usize,
    /// Maximum total UTF-8 byte size persisted for one attempt.
    #[serde(default = "MachineValidationAttemptLogConfig::default_max_attempt_bytes")]
    pub max_attempt_bytes: usize,
    /// How long NICo retains persisted attempt logs before cleanup.
    #[serde(
        default = "MachineValidationAttemptLogConfig::default_retention",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub retention: std::time::Duration,
}

impl MachineValidationAttemptLogConfig {
    /// Hard ceilings shared with the database schema.
    pub const MAX_CHUNK_BYTES: usize = 16 * 1024;
    pub const MAX_ATTEMPT_BYTES: usize = 1024 * 1024;

    const fn default_enabled() -> bool {
        true
    }
    const fn default_max_chunk_bytes() -> usize {
        Self::MAX_CHUNK_BYTES
    }
    const fn default_max_attempt_bytes() -> usize {
        Self::MAX_ATTEMPT_BYTES
    }
    const fn default_retention() -> std::time::Duration {
        std::time::Duration::from_secs(30 * 24 * 60 * 60)
    }

    /// Reject unsafe limits before the API starts accepting Scout output.
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.max_chunk_bytes == 0 || self.max_attempt_bytes == 0 {
            return Err("attempt-log limits must be greater than zero".to_string());
        }
        if self.max_chunk_bytes > self.max_attempt_bytes {
            return Err(
                "attempt-log max_chunk_bytes must not exceed max_attempt_bytes".to_string(),
            );
        }
        if self.max_chunk_bytes > Self::MAX_CHUNK_BYTES
            || self.max_attempt_bytes > Self::MAX_ATTEMPT_BYTES
        {
            return Err(format!(
                "attempt-log limits must not exceed {} bytes per chunk or {} bytes per attempt",
                Self::MAX_CHUNK_BYTES,
                Self::MAX_ATTEMPT_BYTES
            ));
        }
        Ok(())
    }
}

impl Default for MachineValidationAttemptLogConfig {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            max_chunk_bytes: Self::default_max_chunk_bytes(),
            max_attempt_bytes: Self::default_max_attempt_bytes(),
            retention: Self::default_retention(),
        }
    }
}

/// Per-test override for machine validation.
///
/// Example:
/// ```toml
/// tests = [
///    { id = "MmMemLatency", enable = true },
///    { id = "FioSSD", enable = true }
/// ]
/// ```
#[derive(Default, Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MachineValidationTestConfig {
    /// Unique test identifier (e.g., "MmMemLatency").
    pub id: String,
    /// Whether this test is enabled.
    pub enable: bool,
}

impl MachineValidationConfig {
    /// Minimum allowed stale timeout.
    ///
    /// Scout sends machine validation heartbeats every 30 seconds. Keep the timeout above three
    /// missed beats so a low configured value cannot fail healthy active runs between heartbeats.
    pub const MIN_STALE_RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

    const fn default_run_interval() -> std::time::Duration {
        std::time::Duration::from_secs(60)
    }

    const fn default_stale_run_timeout() -> std::time::Duration {
        std::time::Duration::from_secs(24 * 60 * 60)
    }

    fn default_allowed_plugin_types() -> Vec<String> {
        vec!["container".to_owned()]
    }

    /// Validate site-wide Machine Validation policy before startup.
    pub fn validate(&self) -> Result<(), String> {
        if self.run_interval.is_zero() {
            return Err("machine validation run_interval must be greater than zero".to_string());
        }
        if let Some(plugin_type) = self
            .allowed_plugin_types
            .iter()
            .find(|plugin_type| plugin_type.as_str() != "container")
        {
            return Err(format!(
                "machine validation allowed_plugin_types contains unsupported type {plugin_type:?}"
            ));
        }
        self.attempt_logs.validate()
    }
}

impl Default for MachineValidationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            test_selection_mode: MachineValidationTestSelectionMode::default(),
            run_interval: Self::default_run_interval(),
            stale_run_timeout: Self::default_stale_run_timeout(),
            tests: Vec::new(),
            approved_plugin_registries: Vec::new(),
            allowed_plugin_types: Self::default_allowed_plugin_types(),
            allow_privileged_plugins: false,
            allow_full_host_plugins: false,
            attempt_logs: MachineValidationAttemptLogConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_log_policy_rejects_invalid_limits() {
        let config = MachineValidationAttemptLogConfig {
            max_chunk_bytes: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());

        let config = MachineValidationAttemptLogConfig {
            max_chunk_bytes: 2,
            max_attempt_bytes: 1,
            ..Default::default()
        };
        assert!(config.validate().is_err());

        let config = MachineValidationAttemptLogConfig {
            max_attempt_bytes: MachineValidationAttemptLogConfig::MAX_ATTEMPT_BYTES + 1,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn disabled_attempt_log_policy_allows_zero_limits() {
        let config = MachineValidationAttemptLogConfig {
            enabled: false,
            max_chunk_bytes: 0,
            max_attempt_bytes: 0,
            retention: std::time::Duration::ZERO,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn plugin_type_policy_rejects_unsupported_types() {
        let config = MachineValidationConfig {
            allowed_plugin_types: vec!["script".to_owned()],
            ..Default::default()
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn plugin_type_policy_allows_an_empty_deny_list() {
        let config = MachineValidationConfig {
            allowed_plugin_types: Vec::new(),
            ..Default::default()
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn policy_rejects_a_zero_run_interval() {
        let config = MachineValidationConfig {
            run_interval: std::time::Duration::ZERO,
            ..Default::default()
        };

        assert!(config.validate().is_err());
    }
}
