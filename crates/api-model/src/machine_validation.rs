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
use std::fmt::{Debug, Display};
use std::str::FromStr;

use carbide_uuid::machine::MachineId;
use carbide_uuid::machine_validation::{
    MachineValidationAttemptId, MachineValidationId, MachineValidationRunItemId,
};
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::machine::MachineValidationFilter;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MachineValidationTestAddRequest {
    pub name: String,
    pub description: Option<String>,
    pub contexts: Vec<String>,
    pub img_name: Option<String>,
    pub execute_in_host: Option<bool>,
    pub container_arg: Option<String>,
    pub command: String,
    pub args: String,
    pub extra_err_file: Option<String>,
    pub external_config_file: Option<String>,
    pub pre_condition: Option<String>,
    pub timeout: Option<i64>,
    pub extra_output_file: Option<String>,
    pub supported_platforms: Vec<String>,
    pub read_only: Option<bool>,
    pub custom_tags: Vec<String>,
    pub components: Vec<String>,
    pub is_enabled: Option<bool>,
    /// Optional OCI plugin configuration; absent retains legacy test execution.
    pub plugin: Option<MachineValidationPlugin>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MachineValidationTestUpdatePayload {
    pub name: Option<String>,
    pub description: Option<String>,
    pub contexts: Vec<String>,
    pub img_name: Option<String>,
    pub execute_in_host: Option<bool>,
    pub container_arg: Option<String>,
    pub command: Option<String>,
    pub args: Option<String>,
    pub extra_err_file: Option<String>,
    pub external_config_file: Option<String>,
    pub pre_condition: Option<String>,
    pub timeout: Option<i64>,
    pub extra_output_file: Option<String>,
    pub supported_platforms: Vec<String>,
    pub verified: Option<bool>,
    pub custom_tags: Vec<String>,
    pub components: Vec<String>,
    pub is_enabled: Option<bool>,
    /// Must be unset. Plugin revisions are immutable and cannot be updated.
    pub plugin: Option<MachineValidationPlugin>,
}

/// Immutable executable settings for an OCI Machine Validation plugin.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MachineValidationPlugin {
    /// OCI image reference pinned to a digest.
    pub image: String,
    /// Executable and arguments invoked without a shell.
    pub entrypoint: Vec<String>,
    /// Non-secret JSON object copied to the plugin input contract.
    pub parameters_json: String,
    /// Requests a privileged container; it is allowed only when site policy permits it.
    pub privileged: bool,
    /// Requests a writable host-root mount; it additionally needs separate approval
    /// for this verified plugin revision before it can be enabled.
    pub host_access_full: bool,
}

#[derive(Clone, Debug)]
pub struct MachineValidationTestUpdateRequest {
    pub test_id: String,
    pub version: String,
    pub payload: Option<MachineValidationTestUpdatePayload>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MachineValidationTestsGetRequest {
    pub supported_platforms: Vec<String>,
    pub contexts: Vec<String>,
    pub test_id: Option<String>,
    pub read_only: Option<bool>,
    pub custom_tags: Vec<String>,
    pub version: Option<String>,
    pub is_enabled: Option<bool>,
    pub verified: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, strum_macros::EnumString)]
pub enum MachineValidationState {
    #[default]
    Started,
    InProgress,
    Success,
    Skipped,
    Failed,
}

impl Display for MachineValidationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

/// represent machine validation over all test status
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MachineValidationStatus {
    pub state: MachineValidationState,
    pub total: i32,
    pub completed: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, strum_macros::EnumString)]
pub enum MachineValidationRunItemState {
    #[default]
    Pending,
    Running,
    Success,
    Skipped,
    Failed,
}

impl Display for MachineValidationRunItemState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, strum_macros::EnumString)]
pub enum MachineValidationAttemptState {
    #[default]
    Pending,
    Running,
    Success,
    Skipped,
    Failed,
}

impl Display for MachineValidationAttemptState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

fn decode_state<T>(raw: String, column: &'static str) -> Result<T, sqlx::Error>
where
    T: FromStr,
    T::Err: Display,
{
    T::from_str(&raw).map_err(|err| {
        sqlx::Error::Decode(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid {column}: {raw} ({err})"),
        )))
    })
}

#[derive(Debug, Clone)]
pub struct MachineValidation {
    pub id: MachineValidationId,
    pub machine_id: MachineId,
    pub name: String,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
    pub filter: Option<MachineValidationFilter>,
    pub context: Option<String>,
    pub status: Option<MachineValidationStatus>,
    pub duration_to_complete: i64,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    // Columns for these exist, but are unused in rust code
    // pub description: Option<String>,
}

impl<'r> FromRow<'r, PgRow> for MachineValidation {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let filter: Option<sqlx::types::Json<MachineValidationFilter>> = row.try_get("filter")?;
        let status = MachineValidationStatus {
            state: match MachineValidationState::from_str(row.try_get("state")?) {
                Ok(status) => status,
                Err(_) => MachineValidationState::Success,
            },
            total: row.try_get("total")?,
            completed: row.try_get("completed")?,
        };

        Ok(MachineValidation {
            id: row.try_get("id")?,
            machine_id: row.try_get("machine_id")?,
            name: row.try_get("name")?,
            start_time: row.try_get("start_time")?,
            end_time: row.try_get("end_time")?,
            context: row.try_get("context")?,
            filter: filter.map(|x| x.0),
            status: Some(status),
            duration_to_complete: row.try_get("duration_to_complete")?,
            last_heartbeat_at: row.try_get("last_heartbeat_at")?,
            // description: row.try_get("description")?, // unused
        })
    }
}

#[derive(Debug, Clone)]
pub struct MachineValidationRunItem {
    pub id: MachineValidationRunItemId,
    pub run_id: MachineValidationId,
    pub current_attempt_id: Option<MachineValidationAttemptId>,
    pub test_id: String,
    pub test_version: Option<String>,
    pub display_name: String,
    pub context: String,
    pub component: Option<String>,
    pub state: MachineValidationRunItemState,
    pub order_index: i32,
    pub attempt: i32,
    pub max_attempts: i32,
    pub timeout_seconds: i64,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub skip_reason: Option<String>,
    pub failure_reason: Option<String>,
}

impl<'r> FromRow<'r, PgRow> for MachineValidationRunItem {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let state_raw: String = row.try_get("state")?;

        Ok(MachineValidationRunItem {
            id: row.try_get("id")?,
            run_id: row.try_get("run_id")?,
            current_attempt_id: match row
                .try_get::<Option<MachineValidationAttemptId>, _>("current_attempt_id")
            {
                Ok(value) => value,
                Err(sqlx::Error::ColumnNotFound(_)) => None,
                Err(err) => return Err(err),
            },
            test_id: row.try_get("test_id")?,
            test_version: row.try_get("test_version")?,
            display_name: row.try_get("display_name")?,
            context: row.try_get("context")?,
            component: row.try_get("component")?,
            state: decode_state(state_raw, "machine_validation_run_items.state")?,
            order_index: row.try_get("order_index")?,
            attempt: row.try_get("attempt")?,
            max_attempts: row.try_get("max_attempts")?,
            timeout_seconds: row.try_get("timeout_seconds")?,
            started_at: row.try_get("started_at")?,
            ended_at: row.try_get("ended_at")?,
            last_heartbeat_at: row.try_get("last_heartbeat_at")?,
            skip_reason: row.try_get("skip_reason")?,
            failure_reason: row.try_get("failure_reason")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct MachineValidationAttempt {
    pub id: MachineValidationAttemptId,
    pub run_item_id: MachineValidationRunItemId,
    pub attempt_number: i32,
    pub state: MachineValidationAttemptState,
    pub command: Option<String>,
    pub args: Option<String>,
    pub container_image: Option<String>,
    pub execute_in_host: Option<bool>,
    pub exit_code: Option<i32>,
    pub failure_classification: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub stdout_summary: Option<String>,
    pub stderr_summary: Option<String>,
}

impl<'r> FromRow<'r, PgRow> for MachineValidationAttempt {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let state_raw: String = row.try_get("state")?;

        Ok(MachineValidationAttempt {
            id: row.try_get("id")?,
            run_item_id: row.try_get("run_item_id")?,
            attempt_number: row.try_get("attempt_number")?,
            state: decode_state(state_raw, "machine_validation_attempts.state")?,
            command: row.try_get("command")?,
            args: row.try_get("args")?,
            container_image: row.try_get("container_image")?,
            execute_in_host: row.try_get("execute_in_host")?,
            exit_code: row.try_get("exit_code")?,
            failure_classification: row.try_get("failure_classification")?,
            started_at: row.try_get("started_at")?,
            ended_at: row.try_get("ended_at")?,
            last_heartbeat_at: row.try_get("last_heartbeat_at")?,
            stdout_summary: row.try_get("stdout_summary")?,
            stderr_summary: row.try_get("stderr_summary")?,
        })
    }
}

#[derive(Debug, Deserialize, Clone, Serialize)]
pub struct MachineValidationExternalConfig {
    pub name: String,
    pub description: String,
    pub config: Vec<u8>,
    pub version: ConfigVersion,
}

impl<'r> FromRow<'r, PgRow> for MachineValidationExternalConfig {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(MachineValidationExternalConfig {
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            config: row.try_get("config")?,
            version: row.try_get("version")?,
        })
    }
}

#[derive(Debug, Deserialize, Clone, Serialize)]
pub struct MachineValidationTest {
    pub test_id: String,
    pub name: String,
    pub description: Option<String>,
    pub contexts: Vec<String>,
    pub img_name: Option<String>,
    pub execute_in_host: Option<bool>,
    pub container_arg: Option<String>,
    pub command: String,
    pub args: String,
    pub extra_output_file: Option<String>,
    pub extra_err_file: Option<String>,
    pub external_config_file: Option<String>,
    pub pre_condition: Option<String>,
    pub timeout: Option<i64>,
    pub version: ConfigVersion,
    pub supported_platforms: Vec<String>,
    pub modified_by: String,
    pub verified: bool,
    pub read_only: bool,
    pub custom_tags: Option<Vec<String>>,
    pub components: Vec<String>,
    pub last_modified_at: DateTime<Utc>,
    pub is_enabled: bool,
    /// Plugin configuration for this revision, if it is plugin-backed.
    pub plugin: Option<MachineValidationPlugin>,
    /// Revision-scoped approval for a plugin's writable host-root mount.
    pub full_host_approved: bool,
}

impl<'r> FromRow<'r, PgRow> for MachineValidationTest {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(MachineValidationTest {
            test_id: row.try_get("test_id")?,
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            img_name: row.try_get("img_name")?,
            execute_in_host: row.try_get("execute_in_host")?,
            container_arg: row.try_get("container_arg")?,
            command: row.try_get("command")?,
            args: row.try_get("args")?,
            extra_output_file: row.try_get("extra_output_file")?,
            extra_err_file: row.try_get("extra_err_file")?,
            external_config_file: row.try_get("external_config_file")?,
            contexts: row.try_get("contexts")?,
            pre_condition: row.try_get("pre_condition")?,
            timeout: row.try_get("timeout")?,
            version: row.try_get("version")?,
            supported_platforms: row.try_get("supported_platforms")?,
            modified_by: row.try_get("modified_by")?,
            verified: row.try_get("verified")?,
            read_only: row.try_get("read_only")?,
            custom_tags: row.try_get("custom_tags")?,
            components: row.try_get("components")?,
            last_modified_at: row.try_get("last_modified_at")?,
            is_enabled: row.try_get("is_enabled")?,
            plugin: row
                .try_get::<Option<sqlx::types::Json<MachineValidationPlugin>>, _>("plugin")?
                .map(|plugin| plugin.0),
            full_host_approved: row.try_get("full_host_approved")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct MachineValidationResult {
    pub validation_id: MachineValidationId,
    pub name: String,
    pub description: String,
    pub stdout: String,
    pub stderr: String,
    pub command: String,
    pub args: String,
    pub context: String,
    pub exit_code: i32,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub test_id: Option<String>,
}

impl<'r> FromRow<'r, PgRow> for MachineValidationResult {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(MachineValidationResult {
            validation_id: row.try_get("machine_validation_id")?,
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            command: row.try_get("command")?,
            args: row.try_get("args")?,
            context: row.try_get("context")?,
            stdout: row.try_get("stdout")?,
            stderr: row.try_get("stderr")?,
            exit_code: row.try_get("exit_code")?,
            start_time: row.try_get("start_time")?,
            end_time: row.try_get("end_time")?,
            test_id: row.try_get("test_id")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Check, scenarios, value_scenarios};

    use super::*;

    #[test]
    fn tests_get_request_default_serializes_to_all_null_optionals() {
        let req = MachineValidationTestsGetRequest::default();
        let json = serde_json::to_value(&req).unwrap();
        let obj = json.as_object().unwrap();
        // Optional fields should be null, vec fields should be empty arrays
        assert!(obj["test_id"].is_null());
        assert!(obj["is_enabled"].is_null());
        assert_eq!(obj["supported_platforms"], serde_json::json!([]));
    }

    #[test]
    fn state_from_str_parses_every_variant_and_rejects_the_rest() {
        scenarios!(
            run = |s| MachineValidationState::from_str(s).map_err(drop);
            "Started" {
                "Started" => Yields(MachineValidationState::Started),
            }

            "InProgress" {
                "InProgress" => Yields(MachineValidationState::InProgress),
            }

            "Success" {
                "Success" => Yields(MachineValidationState::Success),
            }

            "Skipped" {
                "Skipped" => Yields(MachineValidationState::Skipped),
            }

            "Failed" {
                "Failed" => Yields(MachineValidationState::Failed),
            }

            "empty string" {
                "" => Fails,
            }

            "unknown variant" {
                "Pending" => Fails,
            }

            "lowercase is not accepted" {
                "started" => Fails,
            }

            "uppercase is not accepted" {
                "SUCCESS" => Fails,
            }

            "leading whitespace is not trimmed" {
                " Started" => Fails,
            }

            "trailing whitespace is not trimmed" {
                "Failed " => Fails,
            }

            "numeric input" {
                "0" => Fails,
            }
        );
    }

    #[test]
    fn state_display_renders_the_variant_name() {
        value_scenarios!(
            run = |state| state.to_string();
            "Started" {
                MachineValidationState::Started => "Started".to_string(),
            }

            "InProgress" {
                MachineValidationState::InProgress => "InProgress".to_string(),
            }

            "Success" {
                MachineValidationState::Success => "Success".to_string(),
            }

            "Skipped" {
                MachineValidationState::Skipped => "Skipped".to_string(),
            }

            "Failed" {
                MachineValidationState::Failed => "Failed".to_string(),
            }
        );
    }

    #[test]
    fn run_item_state_from_str_parses_every_variant_and_rejects_the_rest() {
        scenarios!(
            run = |s| MachineValidationRunItemState::from_str(s).map_err(drop);
            "Pending" {
                "Pending" => Yields(MachineValidationRunItemState::Pending),
            }

            "Running" {
                "Running" => Yields(MachineValidationRunItemState::Running),
            }

            "Success" {
                "Success" => Yields(MachineValidationRunItemState::Success),
            }

            "Skipped" {
                "Skipped" => Yields(MachineValidationRunItemState::Skipped),
            }

            "Failed" {
                "Failed" => Yields(MachineValidationRunItemState::Failed),
            }

            "empty string" {
                "" => Fails,
            }

            "unknown variant" {
                "Started" => Fails,
            }

            "lowercase is not accepted" {
                "pending" => Fails,
            }
        );
    }

    #[test]
    fn run_item_state_display_renders_the_variant_name() {
        value_scenarios!(
            run = |state| state.to_string();
            "Pending" {
                MachineValidationRunItemState::Pending => "Pending".to_string(),
            }

            "Running" {
                MachineValidationRunItemState::Running => "Running".to_string(),
            }

            "Success" {
                MachineValidationRunItemState::Success => "Success".to_string(),
            }

            "Skipped" {
                MachineValidationRunItemState::Skipped => "Skipped".to_string(),
            }

            "Failed" {
                MachineValidationRunItemState::Failed => "Failed".to_string(),
            }
        );
    }

    #[test]
    fn attempt_state_from_str_parses_every_variant_and_rejects_the_rest() {
        scenarios!(
            run = |s| MachineValidationAttemptState::from_str(s).map_err(drop);
            "Pending" {
                "Pending" => Yields(MachineValidationAttemptState::Pending),
            }

            "Running" {
                "Running" => Yields(MachineValidationAttemptState::Running),
            }

            "Success" {
                "Success" => Yields(MachineValidationAttemptState::Success),
            }

            "Skipped" {
                "Skipped" => Yields(MachineValidationAttemptState::Skipped),
            }

            "Failed" {
                "Failed" => Yields(MachineValidationAttemptState::Failed),
            }

            "empty string" {
                "" => Fails,
            }

            "unknown variant" {
                "Started" => Fails,
            }

            "lowercase is not accepted" {
                "pending" => Fails,
            }
        );
    }

    #[test]
    fn attempt_state_display_renders_the_variant_name() {
        value_scenarios!(
            run = |state| state.to_string();
            "Pending" {
                MachineValidationAttemptState::Pending => "Pending".to_string(),
            }

            "Running" {
                MachineValidationAttemptState::Running => "Running".to_string(),
            }

            "Success" {
                MachineValidationAttemptState::Success => "Success".to_string(),
            }

            "Skipped" {
                MachineValidationAttemptState::Skipped => "Skipped".to_string(),
            }

            "Failed" {
                MachineValidationAttemptState::Failed => "Failed".to_string(),
            }
        );
    }

    #[test]
    fn state_default_is_started() {
        Check {
            scenario: "default state",
            input: (),
            expect: MachineValidationState::Started,
        }
        .check(|()| MachineValidationState::default());
    }

    #[test]
    fn status_default_is_started_with_zero_counts() {
        value_scenarios!(
            run = |status| status;
            "default state is Started" {
                MachineValidationStatus::default() => MachineValidationStatus {
                    state: MachineValidationState::Started,
                    total: 0,
                    completed: 0,
                },
            }

            "matches an explicitly built default" {
                MachineValidationStatus {
                    state: MachineValidationState::Started,
                    total: 0,
                    completed: 0,
                } => MachineValidationStatus::default(),
            }
        );
    }
}
